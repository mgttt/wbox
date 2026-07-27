//! Docker/Podman 风格机器可读 inspect（当前覆盖容器与本地 OCI 镜像）。

use crate::backend;
use crate::error::{Result, WboxError};
use crate::oci;
use crate::runstate::{self, Liveness};

fn container_value(name: &str) -> Result<serde_json::Value> {
    let dir = runstate::resolve_existing(name)?;
    let entry = runstate::read_meta(&dir)
        .ok_or_else(|| WboxError::args(format!("容器 '{}' 的 meta.json 缺失或不可读", name)))?;
    let liveness = runstate::liveness(&dir);
    let running = liveness == Liveness::Running;
    let status = match liveness {
        Liveness::Created => "created",
        Liveness::Running => "running",
        Liveness::Exited => "exited",
    };
    let exit_code = if liveness != Liveness::Exited {
        serde_json::Value::Null
    } else {
        runstate::read_exit_code(&dir)
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null)
    };
    let (network_mode, workdir) = entry
        .exec_context
        .as_ref()
        .map(|context| {
            (
                if context.allow_network {
                    "host"
                } else {
                    "none"
                },
                context.workdir.as_str(),
            )
        })
        .unwrap_or(("unknown", ""));

    Ok(serde_json::json!({
        "Id": entry.name,
        "Name": format!("/{}", entry.name),
        "Created": entry.created_unix,
        "Path": entry.cmd.first().cloned().unwrap_or_default(),
        "Args": entry.cmd.iter().skip(1).cloned().collect::<Vec<_>>(),
        "State": {
            "Status": status,
            "Running": running,
            "Paused": false,
            "ExitCode": exit_code,
            "Pid": if running { entry.pid } else { 0 },
            "StartedAtUnix": entry.created_unix,
        },
        "Config": {
            "Image": entry.target,
            "Cmd": entry.cmd,
            "WorkingDir": workdir,
        },
        "HostConfig": {
            "NetworkMode": network_mode,
        },
        "Mounts": [],
        "Wbox": {
            "SchemaVersion": 1,
            "StateDir": dir.to_string_lossy(),
            "Stopping": entry.stopping,
        },
    }))
}

fn image_value(reference: &str) -> Result<serde_json::Value> {
    let iref = oci::ImageRef::parse(reference, None)?;
    let dir = oci::image_dir(&iref)?;
    if !dir.is_dir() {
        return Err(WboxError::registry(format!(
            "镜像 '{}' 未 pull（缓存目录 '{}' 不存在）",
            iref.repo_tag(),
            dir.display()
        )));
    }
    let config = oci::config::ImageConfig::load(&dir)?.unwrap_or_default();
    let env = config
        .env
        .iter()
        .map(|(key, value)| format!("{}={}", key, backend::env::redact_value(key, value)))
        .collect::<Vec<_>>();
    let manifest = std::fs::read_to_string(dir.join("manifest.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .unwrap_or(serde_json::Value::Null);
    let id = manifest
        .pointer("/config/digest")
        .and_then(|value| value.as_str())
        .unwrap_or(reference);
    let layers = manifest
        .get("layers")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("digest").and_then(|value| value.as_str()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(serde_json::json!({
        "Id": id,
        "RepoTags": [iref.qualified_ref()],
        "Architecture": "amd64",
        "Os": "linux",
        "Config": {
            "Env": env,
            "Cmd": config.cmd,
            "Entrypoint": config.entrypoint,
            "WorkingDir": config.working_dir.unwrap_or_default(),
        },
        "RootFS": {
            "Type": "layers",
            "Layers": layers,
        },
        "GraphDriver": {
            "Name": "wbox-unpacked",
            "Data": {
                "Rootfs": dir.join("rootfs").to_string_lossy(),
            },
        },
        "Wbox": {
            "SchemaVersion": 1,
            "Registry": iref.registry,
            "CacheDir": dir.to_string_lossy(),
        },
    }))
}

fn print_values(values: Vec<serde_json::Value>) -> Result<u32> {
    let text = serde_json::to_string_pretty(&values)
        .map_err(|e| WboxError::args(format!("序列化 inspect JSON 失败：{}", e)))?;
    println!("{}", text);
    Ok(0)
}

pub fn cmd_container_inspect(args: &[String]) -> Result<u32> {
    if args.is_empty() {
        return Err(WboxError::args("container inspect 缺少容器名"));
    }
    print_values(
        args.iter()
            .map(|name| container_value(name))
            .collect::<Result<Vec<_>>>()?,
    )
}

pub fn cmd_image_inspect(args: &[String]) -> Result<u32> {
    if args.is_empty() {
        return Err(WboxError::args("image inspect 缺少镜像引用"));
    }
    print_values(
        args.iter()
            .map(|reference| image_value(reference))
            .collect::<Result<Vec<_>>>()?,
    )
}

pub fn cmd_inspect(args: &[String]) -> Result<u32> {
    if args.is_empty() {
        return Err(WboxError::args("inspect 缺少容器名或镜像引用"));
    }
    let mut values = Vec::with_capacity(args.len());
    for target in args {
        let is_container = runstate::dir_for(target)
            .ok()
            .is_some_and(|dir| dir.is_dir());
        values.push(if is_container {
            container_value(target)?
        } else {
            image_value(target)?
        });
    }
    print_values(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn image_inspect_contains_machine_readable_config_and_redacts_secrets() {
        let home = TempHome::new("inspect-image");
        let dir = home.plant_fake_image("registry-1.docker.io", "library_fake", "latest");
        std::fs::write(
            dir.join("config.json"),
            r#"{"config":{"Env":["TOKEN=hunter2"],"Cmd":["sh"],"WorkingDir":"/work"}}"#,
        )
        .unwrap();
        let value = image_value("fake:latest").unwrap();
        assert_eq!(
            value.pointer("/Config/Cmd/0").and_then(|v| v.as_str()),
            Some("sh")
        );
        let env = value
            .pointer("/Config/Env/0")
            .and_then(|v| v.as_str())
            .unwrap();
        assert!(env.starts_with("TOKEN="));
        assert!(!env.contains("hunter2"));
    }

    #[test]
    fn container_inspect_reports_exit_code_and_state() {
        let _home = TempHome::new("inspect-container");
        let mut reservation = runstate::reserve_detached("inspected").unwrap();
        let token = reservation.token().to_string();
        let reg = runstate::register_with_context(
            "inspected",
            &["false".into()],
            "alpine:3.20",
            true,
            Some(runstate::ExecContext {
                allow_network: false,
                workdir: "/".into(),
            }),
            Some(&token),
        )
        .unwrap();
        reservation.disarm();
        runstate::record_exit_code(reg.dir(), 23).unwrap();
        drop(reg);

        let value = container_value("inspected").unwrap();
        assert_eq!(
            value.pointer("/State/Status").and_then(|v| v.as_str()),
            Some("exited")
        );
        assert_eq!(
            value.pointer("/State/ExitCode").and_then(|v| v.as_u64()),
            Some(23)
        );
        assert_eq!(
            value
                .pointer("/HostConfig/NetworkMode")
                .and_then(|v| v.as_str()),
            Some("none")
        );
        runstate::remove("inspected").unwrap();
    }

    #[test]
    fn inspect_requires_targets() {
        assert!(cmd_inspect(&[]).is_err());
        assert!(cmd_container_inspect(&[]).is_err());
        assert!(cmd_image_inspect(&[]).is_err());
    }
}
