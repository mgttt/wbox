//! 子进程环境变量白名单（H2/H6 修复）。
//!
//! 威胁模型：
//! - 恶意镜像可在 config.json `Env` 中设置 `BLINK_PREFIX=/`、`WBOX_ROOT=C:\`、
//!   `WBOX_VA_BITS=43` 等键，关闭 blink 层隔离或放大 guest 内存（H2）；
//! - 子进程若直通 wbox 的全部宿主环境，`WBOX_REGISTRY_PASS` 等机密会泄露给
//!   不可信负载（H6）。
//!
//! 对策（对 config.Env 与宿主 env 双路生效）：
//! 1. [`is_reserved_key`]：隔离/凭证相关的保留键（`BLINK_*`、`WBOX_*`）一律
//!    不进入子进程环境——镜像 Env 里出现则丢弃，宿主环境默认也不透传；
//! 2. [`build_child_env`]：构造**显式白名单**环境（SystemRoot/PATH/COMSPEC
//!    等最小集 + 过滤后的镜像 Env + 调用方强制项），`--env-pass-all`
//!    逃生口下也仍过滤保留键并应用强制覆盖；
//! 3. [`redact_value`]：verbose / `image show` 打印时按键名脱敏。

/// 保留键前缀（大小写不敏感）：blink 隔离旋钮与 wbox 自身配置/凭证。
const RESERVED_PREFIXES: &[&str] = &["WBOX_", "BLINK_"];

/// 判定键是否属于隔离/凭证相关的保留键。
pub fn is_reserved_key(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    RESERVED_PREFIXES.iter().any(|p| k.starts_with(p))
}

/// 默认透传给子进程的宿主环境白名单（Windows 进程正常运转的最小集）。
const HOST_WHITELIST: &[&str] = &[
    "SystemRoot",
    "SYSTEMROOT", // 部分工具读大写形式
    "windir",
    "PATH",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "USERNAME",
    "HOMEDRIVE",
    "HOMEPATH",
    "OS",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

/// 过滤镜像 config.Env：丢弃保留键与非法键（空键/含 '='）。
/// 返回 (保留的项, 被丢弃的键名)（丢弃清单供 verbose 告警）。
pub fn sanitize_image_env(
    image_env: &[(String, String)],
) -> (Vec<(String, String)>, Vec<String>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for (k, v) in image_env {
        if k.is_empty() || k.contains('=') || is_reserved_key(k) {
            dropped.push(k.clone());
        } else {
            kept.push((k.clone(), v.clone()));
        }
    }
    (kept, dropped)
}

/// 构造子进程的完整显式环境。
///
/// - `image_env`：已过滤的镜像 Env（非保留键，覆盖同名白名单项）；
/// - `forced`：wbox 强制项（如 `BLINK_PREFIX=<rootfs>`），最终优先级最高；
/// - `pass_all`：`--env-pass-all` 逃生口——继承全部宿主环境，
///   但仍过滤保留键（防 H2/H6）并应用 `forced` 覆盖。
pub fn build_child_env(
    image_env: &[(String, String)],
    forced: &[(String, String)],
    pass_all: bool,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();

    if pass_all {
        for (k, v) in std::env::vars() {
            if !is_reserved_key(&k) {
                out.push((k, v));
            }
        }
    } else {
        for w in HOST_WHITELIST {
            if let Ok(v) = std::env::var(w) {
                // 白名单内不同大小写别名只保留第一个取到值的
                if !out.iter().any(|(k, _)| k.eq_ignore_ascii_case(w)) {
                    out.push((w.to_string(), v));
                }
            }
        }
        // SystemRoot 兜底：很多 Win32 进程缺了它无法初始化
        if !out.iter().any(|(k, _)| k.eq_ignore_ascii_case("SystemRoot")) {
            out.push(("SystemRoot".to_string(), r"C:\Windows".to_string()));
        }
    }

    upsert_all(&mut out, image_env.iter());
    upsert_all(&mut out, forced.iter());
    out
}

/// 按键名（大小写不敏感，Windows 语义）覆盖或追加。
fn upsert_all<'a>(
    out: &mut Vec<(String, String)>,
    items: impl Iterator<Item = &'a (String, String)>,
) {
    for (k, v) in items {
        if let Some(e) = out.iter_mut().find(|(ek, _)| ek.eq_ignore_ascii_case(k)) {
            e.1 = v.clone();
        } else {
            out.push((k.clone(), v.clone()));
        }
    }
}

/// 打印环境变量时的脱敏：键名含 PASS/SECRET/TOKEN/CREDENTIAL/PRIVATE
/// 的值替换为 `***`（防 verbose 输出 / `image show` 泄露凭证）。
pub fn redact_value(key: &str, value: &str) -> String {
    let k = key.to_ascii_uppercase();
    const SENSITIVE: &[&str] = &["PASS", "SECRET", "TOKEN", "CREDENTIAL", "PRIVATE"];
    if SENSITIVE.iter().any(|s| k.contains(s)) {
        "***".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_keys_detected_case_insensitive() {
        for k in [
            "BLINK_PREFIX",
            "blink_prefix",
            "WBOX_ROOT",
            "wbox_va_bits",
            "WBOX_LINUX",
            "WBOX_REGISTRY_PASS",
            "WBOX_REGISTRY_USER",
        ] {
            assert!(is_reserved_key(k), "{}", k);
        }
        for k in ["PATH", "HOME", "LANG", "FOO", "MYAPP_WBOX"] {
            assert!(!is_reserved_key(k), "{}", k);
        }
    }

    #[test]
    fn sanitize_drops_reserved_and_illegal_keys() {
        let env = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("BLINK_PREFIX".to_string(), "/".to_string()),
            ("WBOX_ROOT".to_string(), r"C:\".to_string()),
            ("WBOX_VA_BITS".to_string(), "43".to_string()),
            ("LANG".to_string(), "C".to_string()),
            ("=bad".to_string(), "x".to_string()),
            ("A=B".to_string(), "x".to_string()),
        ];
        let (kept, dropped) = sanitize_image_env(&env);
        assert_eq!(
            kept,
            vec![
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("LANG".to_string(), "C".to_string())
            ]
        );
        assert_eq!(dropped.len(), 5);
    }

    #[test]
    fn child_env_defaults_to_whitelist_only() {
        // 宿主机密不透传
        std::env::set_var("WBOX_REGISTRY_PASS", "hunter2");
        std::env::set_var("MY_SECRET_HOST_VAR", "x");
        let env = build_child_env(&[], &[], false);
        assert!(!env.iter().any(|(k, _)| k == "WBOX_REGISTRY_PASS"));
        assert!(!env.iter().any(|(k, _)| k == "MY_SECRET_HOST_VAR"));
        assert!(env.iter().any(|(k, _)| k.eq_ignore_ascii_case("SystemRoot")));
        std::env::remove_var("WBOX_REGISTRY_PASS");
        std::env::remove_var("MY_SECRET_HOST_VAR");
    }

    #[test]
    fn child_env_image_overrides_whitelist_forced_wins() {
        let img = vec![("PATH".to_string(), "/img/bin".to_string())];
        let forced = vec![("BLINK_PREFIX".to_string(), "/rootfs".to_string())];
        let env = build_child_env(&img, &forced, false);
        assert_eq!(
            env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str()),
            Some("/img/bin")
        );
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "BLINK_PREFIX")
                .map(|(_, v)| v.as_str()),
            Some("/rootfs")
        );
    }

    #[test]
    fn pass_all_still_filters_reserved_and_applies_forced() {
        std::env::set_var("WBOX_REGISTRY_PASS", "hunter2");
        std::env::set_var("BLINK_PREFIX", "/evil");
        std::env::set_var("HOST_OK", "1");
        let forced = vec![("BLINK_PREFIX".to_string(), "/rootfs".to_string())];
        let env = build_child_env(&[], &forced, true);
        assert!(env.iter().any(|(k, _)| k == "HOST_OK"));
        // 保留键不透传（H6：机密；H2：隔离旋钮）
        assert!(!env.iter().any(|(k, _)| k == "WBOX_REGISTRY_PASS"));
        // forced 覆盖
        assert_eq!(
            env.iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("BLINK_PREFIX"))
                .count(),
            1
        );
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "BLINK_PREFIX")
                .map(|(_, v)| v.as_str()),
            Some("/rootfs")
        );
        std::env::remove_var("WBOX_REGISTRY_PASS");
        std::env::remove_var("BLINK_PREFIX");
        std::env::remove_var("HOST_OK");
    }

    #[test]
    fn redact_sensitive_values() {
        assert_eq!(redact_value("PASSWORD", "abc"), "***");
        assert_eq!(redact_value("api_token", "abc"), "***");
        assert_eq!(redact_value("AWS_SECRET_ACCESS_KEY", "abc"), "***");
        assert_eq!(redact_value("PATH", "/usr/bin"), "/usr/bin");
    }
}
