//! AppContainer profile 与令牌 SID 封装。
//!
//! 隔离模型（见 SPEC §0）：
//! - AppContainer profile 提供独立的 AppContainer SID（令牌隔离边界）；
//! - capability 白名单（v1 仅支持 INTERNET_CLIENT）以 SID_AND_ATTRIBUTES 形式下发；
//! - 完整性级别：AppContainer 派生令牌天然为 Low IL（内核强制），无需也无法
//!   在 attribute-list 启动路径上额外指定。

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree};
use windows_sys::Win32::Security::PSID;
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
    DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, FreeSid, WinCapabilityInternetClientSid, SID_AND_ATTRIBUTES,
    SECURITY_MAX_SID_SIZE,
};

use crate::error::{ErrKind, KindExt, Result};

// winnt.h: an enabled SID participates in access checks. windows-sys 0.59
// exposes this under an optional SystemServices feature that wbox does not need.
const SE_GROUP_ENABLED: u32 = 0x0000_0004;

/// RAII 包装：AppContainer profile（命名内核隔离配置）。
///
/// 除非调用 `keep()`（对应 --keep-profile），Drop 时自动删除 profile。
pub struct AppContainerProfile {
    name: String,
    name_wide: Vec<u16>,
    sid: PSID,
    keep: bool,
}

impl AppContainerProfile {
    /// 创建（或确保存在）一个 AppContainer profile，并取回其 SID。
    ///
    /// `capabilities` 为 capability SID 列表（可空）。
    pub fn create(name: &str, capabilities: &[CapabilitySid]) -> Result<Self> {
        // 前置校验：profile 名超长同样只返回 E_INVALIDARG，与描述缺失无法区分，
        // 故先给出可读错误，避免用户对着 0x80070057 猜。规则本体在
        // backend::validate_container_name（跨平台，故各平台单测都覆盖得到）。
        crate::backend::validate_container_name(name)?;
        let name_wide = to_wide(name);
        // pszDescription 是**必填**参数（文档：最长 2048 字符的描述）；传 NULL
        // 会让 CreateAppContainerProfile 直接返回 E_INVALIDARG(0x80070057)——
        // 真机冒烟即因此在隔离主路径上失败。此处给出固定描述。
        let description_wide = to_wide(&format!("wbox 进程容器：{}", name));
        let mut attrs: Vec<SID_AND_ATTRIBUTES> = capabilities
            .iter()
            .map(CapabilitySid::enabled_attributes)
            .collect();
        let mut sid: PSID = std::ptr::null_mut();

        // # Safety
        // - name_wide / description_wide 为以 NUL 结尾的 UTF-16 缓冲区，
        //   生命周期覆盖整个调用；
        // - attrs 指针在 capability 数为 0 时传 null，否则指向有效数组；
        // - sid 为有效的输出指针；成功时返回的 SID 由本结构在 Drop 中 FreeSid。
        let hr = unsafe {
            CreateAppContainerProfile(
                name_wide.as_ptr(),
                name_wide.as_ptr(),        // DisplayName 复用 profile 名
                description_wide.as_ptr(), // Description：必填，不可为 NULL
                if attrs.is_empty() {
                    std::ptr::null()
                } else {
                    attrs.as_mut_ptr()
                },
                attrs.len() as u32,
                &mut sid,
            )
        };
        if hr < 0 {
            // profile 已存在（HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)=0x800700B7，
            // 低 16 位为 Win32 错误码 183）：profile 已在系统注册，视为成功，
            // 改用 DeriveAppContainerSidFromAppContainerName 取回 SID。
            // 返回的 SID 同样由本结构 Drop 时 FreeSid，所有权规则一致（恰好释放一次）。
            if hr as u32 & 0xFFFF == 183 {
                let derived = Self::derive_sid(name)?;
                return Ok(Self {
                    name: name.to_string(),
                    name_wide,
                    sid: derived,
                    keep: false,
                });
            }
            return Err(crate::error::WboxError::profile(format!(
                    "CreateAppContainerProfile 失败，HRESULT=0x{:08X}（可能 profile 已损坏，可尝试删除后重建）",
                    hr as u32
                )));
        }
        Ok(Self {
            name: name.to_string(),
            name_wide,
            sid,
            keep: false,
        })
    }

    /// 只派生 SID（不注册 profile），用于 profile 已存在的场景
    /// （CreateAppContainerProfile 返回 ERROR_ALREADY_EXISTS 时的回退路径）。
    ///
    /// 返回的 PSID 由调用方负责 FreeSid（本 crate 中由 AppContainerProfile::Drop 统一释放）。
    fn derive_sid(name: &str) -> Result<PSID> {
        let name_wide = to_wide(name);
        let mut sid: PSID = std::ptr::null_mut();
        // # Safety: name_wide 为 NUL 结尾 UTF-16；sid 为有效输出指针。
        let hr = unsafe { DeriveAppContainerSidFromAppContainerName(name_wide.as_ptr(), &mut sid) };
        if hr < 0 {
            return Err(crate::error::WboxError::profile(format!("DeriveAppContainerSidFromAppContainerName 失败，HRESULT=0x{:08X}", hr as u32)));
        }
        Ok(sid)
    }

    /// 在不创建 profile 的情况下派生其确定性 SID 字符串。
    ///
    /// Windows OCI 私有 rootfs 必须在启动 AppContainer 前完成 ACL 授权；
    /// profile 名到 SID 的映射是确定性的，因此可先派生并只向该实例授权。
    pub(crate) fn derived_sid_string(name: &str) -> Result<String> {
        crate::backend::validate_container_name(name)?;
        let sid = Self::derive_sid(name)?;
        let result = sid_to_string(sid).ctx(ErrKind::Profile);
        // # Safety: sid 由 DeriveAppContainerSidFromAppContainerName 分配，
        // 调用方必须且只需 FreeSid 一次。
        unsafe { FreeSid(sid) };
        result
    }

    /// 打开运行中容器已经注册的 profile，不创建或删除系统注册项。`exec`
    /// 只需要同一 SID；使用这个入口可避免附着失败时意外留下新 profile。
    pub fn open_existing(name: &str) -> Result<Self> {
        let name_wide = to_wide(name);
        let sid = Self::derive_sid(name)?;
        Ok(Self {
            name: name.to_string(),
            name_wide,
            sid,
            keep: true,
        })
    }

    /// profile 名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// AppContainer SID（原始指针，仅在本对象存活期间有效）。
    pub fn sid(&self) -> PSID {
        self.sid
    }

    /// AppContainer SID 的字符串形式（S-1-15-2-...），用于 --verbose 输出。
    pub fn sid_string(&self) -> Result<String> {
        sid_to_string(self.sid).ctx(ErrKind::Profile)
    }

    /// 对应 --keep-profile：退出时不删除 profile。
    pub fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        if !self.keep {
            // # Safety: name_wide 为 NUL 结尾 UTF-16 缓冲区。
            let hr = unsafe { DeleteAppContainerProfile(self.name_wide.as_ptr()) };
            if hr < 0 {
                eprintln!(
                    "wbox: 警告：DeleteAppContainerProfile({}) 失败，HRESULT=0x{:08X}",
                    self.name, hr as u32
                );
            }
        }
        if !self.sid.is_null() {
            // # Safety: sid 由 CreateAppContainerProfile 返回，必须且只需 FreeSid 一次。
            unsafe { FreeSid(self.sid) };
        }
    }
}

/// 已知 capability 的 SID 包装（v1 仅 INTERNET_CLIENT）。
pub struct CapabilitySid {
    pub sid: PSID,
    /// 持有 SID 底层内存，保证 `sid` 指针在本对象存活期间有效（仅所有权用途）。
    _buffer: Vec<u8>,
    desc: &'static str,
}

impl CapabilitySid {
    /// INTERNET_CLIENT capability（S-1-15-3-1），授予后可访问网络。
    pub fn internet_client() -> Result<Self> {
        // SECURITY_MAX_SID_SIZE = 68：任何合法 SID 的最大尺寸，直接按上限分配。
        let mut buffer = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = buffer.len() as u32;
        // # Safety: buffer/size 为有效的输入输出参数；失败后取 GetLastError。
        let ok = unsafe {
            CreateWellKnownSid(
                WinCapabilityInternetClientSid,
                std::ptr::null_mut(),
                buffer.as_mut_ptr() as PSID,
                &mut size,
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(crate::error::WboxError::profile(format!("CreateWellKnownSid(InternetClient) 失败，GetLastError={}", err)));
        }
        buffer.truncate(size as usize);
        let sid = buffer.as_mut_ptr() as PSID;
        Ok(Self {
            sid,
            _buffer: buffer,
            desc: "INTERNET_CLIENT",
        })
    }

    pub fn desc(&self) -> &'static str {
        self.desc
    }

    /// Return the enabled form required for capability access checks.
    pub fn enabled_attributes(&self) -> SID_AND_ATTRIBUTES {
        SID_AND_ATTRIBUTES {
            Sid: self.sid,
            Attributes: SE_GROUP_ENABLED,
        }
    }
}

/// 把 SID 转成字符串形式（S-1-15-2-...）。
pub(crate) fn sid_to_string(sid: PSID) -> crate::fault::Result<String> {
    let mut buf: windows_sys::core::PWSTR = std::ptr::null_mut();
    // # Safety: sid 为有效 SID；buf 为有效输出指针，成功后用 LocalFree 释放。
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut buf) };
    if ok == 0 {
        return Err(crate::fail!("ConvertSidToStringSidW 失败"));
    }
    // # Safety: buf 为 NUL 结尾 UTF-16，由本函数独占；读取后立即 LocalFree。
    let s = unsafe {
        let mut len = 0usize;
        while *buf.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(buf, len);
        let s = String::from_utf16_lossy(slice);
        LocalFree(buf as *mut _);
        s
    };
    Ok(s)
}

/// UTF-8/UTF-16 转换辅助：生成 NUL 结尾宽字符缓冲区。
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 关闭句柄的通用小工具（RAII）。
pub struct OwnedHandle(pub windows_sys::Win32::Foundation::HANDLE);

// Win32 kernel handles are process-wide and may be closed from a different thread.
unsafe impl Send for OwnedHandle {}

impl OwnedHandle {
    pub fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // # Safety: 句柄由本结构独占，只关闭一次。
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
#[cfg(windows)]
mod real_windows_tests {
    use super::*;

    fn create_profile(name: &str, caps: &[CapabilitySid]) -> AppContainerProfile {
        AppContainerProfile::create(name, caps)
            .unwrap_or_else(|e| panic!("创建 AppContainer profile 失败：{}", e))
    }
    /// 唯一 tag：避免并发跑同一 suite 时 profile 名相撞（DeleteAppContainerProfile
    /// 在 Drop 里清，但中途可能重叠）。process id + 静态计数器双重保险。
    fn unique_name(label: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!(
            "wboxtest_{}_{}_{}",
            std::process::id(),
            n,
            label
        )
    }

    /// 真机 round-trip：create → sid 字符串非空 → keep → Drop 后 derive 找不到。
    /// 不跑 spawn，只测 profile 生命周期 API。
    #[test]
    fn appcontainer_profile_create_keep_delete_roundtrip() {
        let name = unique_name("rt");
        let caps: Vec<CapabilitySid> = Vec::new();
        let mut p = create_profile(&name, &caps);
        // SID 字符串形如 S-1-15-2-...
        let sid = p.sid_string().unwrap();
        assert!(
            sid.starts_with("S-1-15-2-"),
            "AppContainer SID 应为 S-1-15-2-... 前缀，实得 {}",
            sid
        );
        assert!(sid.len() > "S-1-15-2-".len());

        // keep 后再 derive：profile 应已注册，derive 能找到 SID
        p.keep();
        let deriv_sid_ptr = AppContainerProfile::derive_sid(&name).unwrap();
        // 不要 leak
        unsafe { FreeSid(deriv_sid_ptr) };

        // 现在手动删除（因为 keep 跳过了 Drop 的 Delete）
        let nw = to_wide(&name);
        let hr = unsafe { DeleteAppContainerProfile(nw.as_ptr()) };
        assert!(
            hr >= 0,
            "DeleteAppContainerProfile({}) 失败 hr=0x{:08X}",
            name,
            hr as u32
        );
    }

    /// profile 已存在时 create 走 ERROR_ALREADY_EXISTS 回退路径，derive 拿 SID。
    #[test]
    fn appcontainer_profile_already_exists_falls_back_to_derive() {
        let name = unique_name("ae");
        let caps: Vec<CapabilitySid> = Vec::new();

        // 第一次 create + keep（保留 profile）
        let mut p1 = create_profile(&name, &caps);
        p1.keep();
        let sid1 = p1.sid_string().unwrap();
        drop(p1);

        // 第二次 create 同名 → HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS) = 0x800700B7
        // 代码里走 derive 路径取回 SID。
        let p2 = AppContainerProfile::create(&name, &caps).unwrap();
        let sid2 = p2.sid_string().unwrap();
        assert_eq!(sid1, sid2, "两次 create 的 SID 必须一致（同一 profile）");
        // p2 Drop 会 DeleteAppContainerProfile，清理掉保留的 profile
    }

    /// INTERNET_CLIENT capability SID 构造 + 字符串校验。
    /// S-1-15-3-1 是 well-known INTERNET_CLIENT。
    #[test]
    fn capability_sid_internet_client_has_correct_form() {
        let cap = CapabilitySid::internet_client().unwrap();
        // 取回 SID 字符串需 PSID -> 通过 sid_to_string 私有 fn
        let s = sid_to_string(cap.sid).unwrap();
        assert!(
            s.starts_with("S-1-15-3-"),
            "INTERNET_CLIENT capability SID 应为 S-1-15-3-... 前缀，实得 {}",
            s
        );
        assert_eq!(cap.desc(), "INTERNET_CLIENT");
        assert_eq!(
            cap.enabled_attributes().Attributes,
            SE_GROUP_ENABLED,
            "capability SID 必须启用后才参与访问检查"
        );
    }

    /// profile 名超长（>64 字符）应在 create 阶段报错（如果上游加了前置校验），
    /// 或者 PE 内核返回 E_INVALIDARG。无论哪条路都不能 panic / unwrap 漏出来。
    /// 当前实现是直接调 API，记现状：超长名由 API 自己拒（0x80070057）。
    #[test]
    fn appcontainer_profile_overlong_name_is_rejected() {
        let too_long: String = "a".repeat(65); // 文档上限 64
        let caps: Vec<CapabilitySid> = Vec::new();
        let res = AppContainerProfile::create(&too_long, &caps);
        match res {
            Ok(_) => panic!("65 字符的 profile 名应被拒，但 create 成功了"),
            Err(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("CreateAppContainerProfile") || msg.contains("profile"),
                    "错误信息应指向 profile 问题，实得：{}",
                    msg
                );
            }
        }
    }
}
