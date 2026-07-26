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

/// `CreateAppContainerProfile` 对 `pszAppContainerName` 的长度上限（字符）。
pub(crate) const MAX_PROFILE_NAME_CHARS: usize = 64;

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
        // 前置校验：profile 名超长同样只返回 E_INVALIDARG，与描述缺失无法区分。
        // 这里先给出可读错误，避免用户对着 0x80070057 猜。
        // 限制见 CreateAppContainerProfile 文档：pszAppContainerName ≤ 64 字符。
        let name_len = name.chars().count();
        if name_len == 0 || name_len > MAX_PROFILE_NAME_CHARS {
            return Err(crate::error::WboxError::args(format!(
                "容器名长度非法（{} 字符）：AppContainer profile 名须为 1..={} 字符",
                name_len, MAX_PROFILE_NAME_CHARS
            )));
        }
        let name_wide = to_wide(name);
        // pszDescription 是**必填**参数（文档：最长 2048 字符的描述）；传 NULL
        // 会让 CreateAppContainerProfile 直接返回 E_INVALIDARG(0x80070057)——
        // 真机冒烟即因此在隔离主路径上失败。此处给出固定描述。
        let description_wide = to_wide(&format!("wbox 进程容器：{}", name));
        let mut attrs: Vec<SID_AND_ATTRIBUTES> = capabilities
            .iter()
            .map(|c| SID_AND_ATTRIBUTES {
                Sid: c.sid,
                Attributes: 0, // capability 组不附带 attribute 标志
            })
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
}

/// 把 SID 转成字符串形式（S-1-15-2-...）。
fn sid_to_string(sid: PSID) -> anyhow::Result<String> {
    let mut buf: windows_sys::core::PWSTR = std::ptr::null_mut();
    // # Safety: sid 为有效 SID；buf 为有效输出指针，成功后用 LocalFree 释放。
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut buf) };
    if ok == 0 {
        return Err(anyhow::anyhow!("ConvertSidToStringSidW 失败"));
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
