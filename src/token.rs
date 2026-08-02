//! AppContainer profile 与令牌 SID 封装。
//!
//! 隔离模型（见 SPEC §0）：
//! - AppContainer profile 提供独立的 AppContainer SID（令牌隔离边界）；
//! - capability 白名单由 platform 在挂起创建边界转换为原生属性；
//! - 完整性级别：AppContainer 派生令牌天然为 Low IL（内核强制），无需也无法
//!   在 attribute-list 启动路径上额外指定。

use crate::error::Result;
use agenterm_platform::adapters::windows::app_container::{
    self, AppContainerCapability, AppContainerCapabilityKind, AppContainerCapabilitySid,
    AppContainerProfileErrorKind, OwnedAppContainerSid,
};

/// RAII 包装：AppContainer profile（命名内核隔离配置）。
///
/// 除非调用 `keep()`（对应 --keep-profile），Drop 时自动删除 profile。
pub struct AppContainerProfile {
    name: String,
    sid: OwnedAppContainerSid,
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
        // pszDescription 是**必填**参数（文档：最长 2048 字符的描述）；传 NULL
        // 会让 CreateAppContainerProfile 直接返回 E_INVALIDARG(0x80070057)——
        // 真机冒烟即因此在隔离主路径上失败。此处给出固定描述。
        let description = format!("wbox 进程容器：{name}");
        let attrs = capabilities
            .iter()
            .map(|capability| {
                AppContainerCapability::enabled(capability.as_bytes()).map_err(|error| {
                    crate::error::WboxError::profile(format!(
                        "构造 AppContainer capability 失败：{error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let sid = match app_container::create_profile(name, name, &description, &attrs) {
            Ok(sid) => sid,
            // profile 已存在（HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)=0x800700B7，
            // 低 16 位为 Win32 错误码 183）：profile 已在系统注册，视为成功，
            // 改用 DeriveAppContainerSidFromAppContainerName 取回 SID。
            Err(error) if error.kind() == AppContainerProfileErrorKind::AlreadyExists => {
                let derived = Self::derive_sid(name)?;
                return Ok(Self {
                    name: name.to_string(),
                    sid: derived,
                    // This instance did not create the profile and therefore
                    // must not delete another owner's registration on Drop.
                    keep: true,
                });
            }
            Err(error) => {
                return Err(crate::error::WboxError::profile(format!(
                    "CreateAppContainerProfile 失败：{error}（可能 profile 已损坏，可尝试删除后重建）"
                )));
            }
        };
        Ok(Self {
            name: name.to_string(),
            sid,
            keep: false,
        })
    }

    /// 只派生 SID（不注册 profile），用于 profile 已存在的场景
    /// （CreateAppContainerProfile 返回 ERROR_ALREADY_EXISTS 时的回退路径）。
    ///
    fn derive_sid(name: &str) -> Result<OwnedAppContainerSid> {
        app_container::derive_profile_sid(name).map_err(|error| {
            crate::error::WboxError::profile(format!(
                "DeriveAppContainerSidFromAppContainerName 失败：{error}"
            ))
        })
    }

    /// 在不创建 profile 的情况下派生其确定性 SID。
    ///
    /// Windows OCI 私有 rootfs 必须在启动 AppContainer 前完成 ACL 授权；
    /// profile 名到 SID 的映射是确定性的，因此可先派生并只向该实例授权。
    pub(crate) fn derived_sid(name: &str) -> Result<OwnedAppContainerSid> {
        crate::backend::validate_container_name(name)?;
        Self::derive_sid(name)
    }

    /// 打开运行中容器已经注册的 profile，不创建或删除系统注册项。`exec`
    /// 只需要同一 SID；使用这个入口可避免附着失败时意外留下新 profile。
    pub fn open_existing(name: &str) -> Result<Self> {
        let sid = Self::derive_sid(name)?;
        Ok(Self {
            name: name.to_string(),
            sid,
            keep: true,
        })
    }

    /// profile 名。
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn sid_bytes(&self) -> &[u8] {
        self.sid.as_bytes()
    }

    /// AppContainer SID 的字符串形式（S-1-15-2-...），用于 --verbose 输出。
    pub fn sid_string(&self) -> Result<String> {
        self.sid.string().map_err(|error| {
            crate::error::WboxError::profile(format!("转换 AppContainer SID 失败：{error}"))
        })
    }

    /// 对应 --keep-profile：退出时不删除 profile。
    pub fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        if !self.keep {
            if let Err(error) = app_container::delete_profile(&self.name) {
                eprintln!(
                    "wbox: 警告：DeleteAppContainerProfile({}) 失败：{}",
                    self.name, error
                );
            }
        }
    }
}

/// 已知 AppContainer capability 的 SID 包装。
pub struct CapabilitySid {
    sid: AppContainerCapabilitySid,
    desc: &'static str,
}

impl CapabilitySid {
    /// INTERNET_CLIENT capability（S-1-15-3-1），授予后可访问网络。
    pub fn internet_client() -> Result<Self> {
        Self::well_known(
            AppContainerCapabilityKind::InternetClient,
            "INTERNET_CLIENT",
        )
    }

    /// INTERNET_CLIENT_SERVER capability（S-1-15-3-2）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn internet_client_server() -> Result<Self> {
        Self::well_known(
            AppContainerCapabilityKind::InternetClientServer,
            "INTERNET_CLIENT_SERVER",
        )
    }

    /// PRIVATE_NETWORK_CLIENT_SERVER capability（S-1-15-3-3）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn private_network_client_server() -> Result<Self> {
        Self::well_known(
            AppContainerCapabilityKind::PrivateNetworkClientServer,
            "PRIVATE_NETWORK_CLIENT_SERVER",
        )
    }

    fn well_known(kind: AppContainerCapabilityKind, desc: &'static str) -> Result<Self> {
        let sid = AppContainerCapabilitySid::well_known(kind).map_err(|error| {
            crate::error::WboxError::profile(format!(
                "构造 AppContainer capability {desc} 失败：{error}"
            ))
        })?;
        Ok(Self { sid, desc })
    }

    pub fn desc(&self) -> &'static str {
        self.desc
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.sid.as_bytes()
    }

    pub fn process_capability(
        &self,
    ) -> agenterm_platform::app_container_process::AppContainerProcessCapability<'_> {
        agenterm_platform::app_container_process::AppContainerProcessCapability::enabled(
            self.as_bytes(),
        )
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
        format!("wboxtest_{}_{}_{}", std::process::id(), n, label)
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
        let derived_sid = AppContainerProfile::derive_sid(&name).unwrap();
        assert_eq!(sid, derived_sid.string().unwrap());

        // 现在手动删除（因为 keep 跳过了 Drop 的 Delete）
        app_container::delete_profile(&name).unwrap();
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
        // p2 只是借用既有 profile，Drop 不得删除别人的注册项。
        drop(p2);
        let still_registered = AppContainerProfile::derive_sid(&name).unwrap();
        assert_eq!(sid1, still_registered.string().unwrap());
        app_container::delete_profile(&name).unwrap();
    }

    /// INTERNET_CLIENT capability SID 构造 + 字符串校验。
    /// S-1-15-3-1 是 well-known INTERNET_CLIENT。
    #[test]
    fn capability_sid_internet_client_has_correct_form() {
        let cap = CapabilitySid::internet_client().unwrap();
        let s = app_container::sid_string(cap.as_bytes()).unwrap();
        assert!(
            s.starts_with("S-1-15-3-"),
            "INTERNET_CLIENT capability SID 应为 S-1-15-3-... 前缀，实得 {}",
            s
        );
        assert_eq!(cap.desc(), "INTERNET_CLIENT");
        assert_eq!(
            cap.process_capability().attributes(),
            4,
            "capability SID 必须启用后才参与访问检查"
        );
    }

    #[test]
    fn server_capability_sids_match_windows_well_known_values() {
        for (cap, expected, desc) in [
            (
                CapabilitySid::internet_client_server().unwrap(),
                "S-1-15-3-2",
                "INTERNET_CLIENT_SERVER",
            ),
            (
                CapabilitySid::private_network_client_server().unwrap(),
                "S-1-15-3-3",
                "PRIVATE_NETWORK_CLIENT_SERVER",
            ),
        ] {
            assert_eq!(app_container::sid_string(cap.as_bytes()).unwrap(), expected);
            assert_eq!(cap.desc(), desc);
            assert_eq!(cap.process_capability().attributes(), 4);
        }
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
