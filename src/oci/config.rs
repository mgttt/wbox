//! OCI image config.json 消费。
//!
//! `image pull` 已把 config.json 写入缓存目录（见 image.rs），本模块负责
//! 读取并解析其中 `config` 段的四个字段：Env / Cmd / Entrypoint / WorkingDir，
//! 供 `wbox run <IMAGE>` 合并出最终命令行。
//!
//! 命令合并规则对齐 docker：
//! - 显式 cmd（`wbox run <img> -- <cmd>`）覆盖镜像 Cmd；
//! - Entrypoint 始终前置（无论显式 cmd 是否给出）；
//! - 两者皆空则视为无命令（由调用方报错提示）。
//! Env 以 `KEY=VALUE` 形式存储，由后端在 spawn 前注入子进程环境；
//! WorkingDir 为空字符串时按 docker 语义视为未设置（默认 `/`）。

use crate::error::{ErrKind, KindExt, Result, WboxError};
use anyhow::Context;
use std::path::Path;

/// 已 pull 镜像的 config.json 中 `config` 段摘要。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageConfig {
    /// 环境变量（`KEY=VALUE` 对，保持镜像内声明顺序）
    pub env: Vec<(String, String)>,
    /// 镜像 Cmd（docker 语义：Entrypoint 的默认参数 / 独立默认命令）
    pub cmd: Vec<String>,
    /// 镜像 Entrypoint（始终前置）
    pub entrypoint: Vec<String>,
    /// 镜像 WorkingDir（guest 路径；空 = 未设置）
    pub working_dir: Option<String>,
}

impl ImageConfig {
    /// 从镜像缓存目录加载 config.json；文件不存在返回 `Ok(None)`
    /// （例如手工构造的缓存目录），存在但解析失败返回错误。
    pub fn load(image_dir: &Path) -> Result<Option<Self>> {
        let path = image_dir.join("config.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(WboxError::new(
                    ErrKind::Registry,
                    anyhow::anyhow!(e).context(format!("读取 {} 失败", path.display())),
                ))
            }
        };
        let v: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("解析 {} 失败", path.display()))
            .ctx(ErrKind::Registry)?;
        Ok(Some(Self::from_json(&v)))
    }

    /// 从完整 config.json 的 JSON Value 提取 config 段。
    pub fn from_json(v: &serde_json::Value) -> Self {
        let cfg = v.get("config").cloned().unwrap_or(serde_json::Value::Null);

        let str_list = |key: &str| -> Vec<String> {
            cfg.get(key)
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        };

        let env = cfg
            .get("Env")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str())
                    // docker 约定 Env 元素为 KEY=VALUE；拆不出 '=' 的畸形项跳过。
                    .filter_map(|kv| {
                        kv.split_once('=')
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let working_dir = cfg
            .get("WorkingDir")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        Self {
            env,
            cmd: str_list("Cmd"),
            entrypoint: str_list("Entrypoint"),
            working_dir,
        }
    }

    /// 按 docker 规则合并出最终命令：
    /// `Entrypoint + (显式 cmd 若给出，否则镜像 Cmd)`。
    /// 显式 cmd 为空切片表示用户未在 `--` 后提供命令。
    pub fn merged_command(&self, explicit_cmd: &[String]) -> Vec<String> {
        let mut out = self.entrypoint.clone();
        if explicit_cmd.is_empty() {
            out.extend(self.cmd.iter().cloned());
        } else {
            out.extend(explicit_cmd.iter().cloned());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(entrypoint: &[&str], cmd: &[&str]) -> ImageConfig {
        ImageConfig {
            env: Vec::new(),
            entrypoint: entrypoint.iter().map(|s| s.to_string()).collect(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            working_dir: None,
        }
    }

    fn explicit(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_full_config() {
        let v = serde_json::json!({
            "architecture": "amd64",
            "config": {
                "Env": ["PATH=/usr/bin", "FOO=bar=baz"],
                "Cmd": ["bash"],
                "Entrypoint": ["/docker-entrypoint.sh"],
                "WorkingDir": "/app"
            }
        });
        let c = ImageConfig::from_json(&v);
        assert_eq!(
            c.env,
            vec![
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("FOO".to_string(), "bar=baz".to_string()) // 值内含 '=' 原样保留
            ]
        );
        assert_eq!(c.cmd, vec!["bash"]);
        assert_eq!(c.entrypoint, vec!["/docker-entrypoint.sh"]);
        assert_eq!(c.working_dir.as_deref(), Some("/app"));
    }

    #[test]
    fn parse_missing_config_section_and_empty_workdir() {
        // 无 config 段：全部默认
        let c = ImageConfig::from_json(&serde_json::json!({"architecture": "amd64"}));
        assert_eq!(c, ImageConfig::default());
        // WorkingDir 空字符串 = 未设置
        let c = ImageConfig::from_json(&serde_json::json!({"config": {"WorkingDir": ""}}));
        assert_eq!(c.working_dir, None);
    }

    // ---- docker 合并规则 ----

    #[test]
    fn merge_explicit_overrides_cmd() {
        let c = cfg(&[], &["bash"]);
        // 显式 cmd 覆盖镜像 Cmd
        assert_eq!(c.merged_command(&explicit(&["sh", "-l"])), vec!["sh", "-l"]);
    }

    #[test]
    fn merge_uses_image_cmd_when_no_explicit() {
        let c = cfg(&[], &["bash"]);
        assert_eq!(c.merged_command(&[]), vec!["bash"]);
    }

    #[test]
    fn merge_entrypoint_always_prepended() {
        let c = cfg(&["/entry", "--serve"], &["--default-arg"]);
        // 无显式 cmd：Entrypoint + Cmd
        assert_eq!(
            c.merged_command(&[]),
            vec!["/entry", "--serve", "--default-arg"]
        );
        // 有显式 cmd：Entrypoint + 显式（Cmd 被覆盖）
        assert_eq!(
            c.merged_command(&explicit(&["--override"])),
            vec!["/entry", "--serve", "--override"]
        );
    }

    #[test]
    fn merge_empty_everything_yields_empty() {
        let c = cfg(&[], &[]);
        assert!(c.merged_command(&[]).is_empty());
    }

    #[test]
    fn load_missing_config_json_returns_none() {
        let dir = std::env::temp_dir().join("wbox-test-no-config");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(ImageConfig::load(&dir).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_roundtrip() {
        let dir = std::env::temp_dir().join("wbox-test-config-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"config":{"Env":["A=1"],"Cmd":["bash"],"WorkingDir":"/root"}}"#,
        )
        .unwrap();
        let c = ImageConfig::load(&dir).unwrap().unwrap();
        assert_eq!(c.cmd, vec!["bash"]);
        assert_eq!(c.env, vec![("A".to_string(), "1".to_string())]);
        assert_eq!(c.working_dir.as_deref(), Some("/root"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
