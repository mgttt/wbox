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
    // `Paused` 原本写死 false——那比没有这个字段更糟：docker 用户会拿
    // `.State.Paused` 去写脚本，而它结构性地永远为假。改成从 /proc 实测，
    // 且与 `ps`/`compose ps` 共用同一个状态口径（`status::label`）。
    let status = super::status::label(&dir, liveness);
    let paused = status == "paused";
    let exit_code = if liveness != Liveness::Exited {
        serde_json::Value::Null
    } else {
        runstate::read_exit_code(&dir)
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null)
    };
    // `Mounts` 与端口原本是写死的空数组／缺席——容器明明挂着卷、发布着端口，
    // `inspect` 却说没有。docker 用户读 `.Mounts` 与 `.HostConfig.PortBindings`，
    // 给一个恒空的字段比不给更糟。
    let ctx = entry.exec_context.as_ref();
    let mounts: Vec<serde_json::Value> = ctx
        .map(|c| c.volumes.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|v| {
            // 记录形如 `host:guest` 或 `host:guest:ro`。从右往左剥模式，
            // 免得把 Windows 盘符冒号当成分隔符（与 `-v` 的解析同一取舍）。
            let (rest, ro) = match v.strip_suffix(":ro") {
                Some(r) => (r, true),
                None => (v.strip_suffix(":rw").unwrap_or(v), false),
            };
            let (host, guest) = rest.rsplit_once(':').unwrap_or((rest, ""));
            serde_json::json!({
                "Type": "bind",
                "Source": host,
                "Destination": guest,
                "RW": !ro,
            })
        })
        .collect();
    let port_bindings: serde_json::Map<String, serde_json::Value> = ctx
        .map(|c| c.ports.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| {
            let (host, guest) = p.split_once(':')?;
            Some((
                // docker 的键形如 "80/tcp"。wbox 的 -p 只支持 TCP（F9.2），
                // 写死 tcp 是如实的，不是偷懒。
                format!("{}/tcp", guest),
                serde_json::json!([{ "HostIp": "", "HostPort": host }]),
            ))
        })
        .collect();

    // 网络模式是三态（none / host / container:X），而 `allow_network` 只有两态。
    // 此前只看后者，于是一个确实共享了对端 netns 的容器被报成 "none"。
    let (network_mode, workdir) = entry
        .exec_context
        .as_ref()
        .map(|context| {
            let mode = match context.network_container.as_deref() {
                Some(peer) => format!("container:{}", peer),
                None if context.allow_network => "host".to_string(),
                None => "none".to_string(),
            };
            (mode, context.workdir.as_str())
        })
        .unwrap_or(("unknown".to_string(), ""));

    Ok(serde_json::json!({
        "Id": entry.name,
        "Name": format!("/{}", entry.name),
        "Created": entry.created_unix,
        "Path": entry.cmd.first().cloned().unwrap_or_default(),
        "Args": entry.cmd.iter().skip(1).cloned().collect::<Vec<_>>(),
        "State": {
            "Status": status,
            "Running": running,
            "Paused": paused,
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
            "PortBindings": port_bindings,
        },
        "Mounts": mounts,
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
    /// `Mounts` 与 `PortBindings` 原本是写死的空值——容器明明挂着卷、发布着端口，
    /// inspect 却说没有。给一个恒空的字段比不给更糟：docker 用户会拿它写脚本。
    #[test]
    fn mounts_and_ports_reflect_the_recorded_context() {
        let _home = crate::testenv::TempHome::new("inspectmounts");
        let dir = crate::runstate::dir_for("c").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("meta.json"),
            serde_json::json!({
                "name": "c", "pid": 1, "created_unix": 1,
                "cmd": ["x"], "target": "t",
                "exec_context": {
                    "allow_network": false,
                    "workdir": "/",
                    "volumes": ["/h/data:/mnt/data:ro", "/h/rw:/mnt/rw"],
                    "ports": ["18080:80"],
                },
            })
            .to_string(),
        )
        .unwrap();
        let v = super::container_value("c").unwrap();
        let mounts = v["Mounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0]["Source"], "/h/data");
        assert_eq!(mounts[0]["Destination"], "/mnt/data");
        assert_eq!(mounts[0]["RW"], false, ":ro 要如实反映成只读");
        assert_eq!(mounts[1]["RW"], true, "没写 :ro 的是可写");
        // -p 只支持 TCP（F9.2），键写死 tcp 是如实的
        assert_eq!(
            v["HostConfig"]["PortBindings"]["80/tcp"][0]["HostPort"],
            "18080"
        );
    }

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
                ..Default::default()
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
