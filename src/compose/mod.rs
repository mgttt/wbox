//! `wbox compose`：多容器编排子集（`PRD.md` F9.14）。
//!
//! # 范围先钉死
//!
//! 不做"重新实现 docker-compose"。第一刀只收 compose 文件里**真正用得上**的
//! 八个字段与三个动词，其余一律**明确报错**——静默忽略一个没实现的字段，
//! 会让用户以为配置生效了，那比不支持更糟。
//!
//! # 复用而不是另起一套
//!
//! `up` 不自己实现容器启动：它把每个 service 翻译成一条 `wbox run` 的 argv，
//! 交给 [`crate::cli::run::cmd_run`]。`down` 同理走 `stop` + `rm`。这样 compose
//! 天然继承了 run 的全部校验与语义（`-v`/`-p`/`--restart`/healthcheck/冲突检查），
//! 不会出现"compose 起的容器和 run 起的行为不一样"这种最难查的偏差。
//!
//! # 网络语义与 docker 不同，要直说
//!
//! docker compose 给每个项目建一个 bridge 网络 + 内建 DNS，服务之间用服务名
//! 互访。wbox 在 rootless 下没有 bridge（见 §2.4：那需要 slirp4netns 级常驻
//! 网络栈，与"免安装、无服务"冲突）。这里的做法是：**第一个服务持有网络，
//! 其余服务用 `--network container:` 加入它**，服务之间经 `localhost` 互访。
//! 对 sidecar 这类主要场景等价；但"用服务名当主机名"是做不到的。

pub mod yaml;

use crate::error::{Result, WboxError};
use yaml::Node;

/// compose 文件里 service 支持的字段。**白名单**——不在其中的一律报错。
const SUPPORTED_FIELDS: &[&str] = &[
    "image",
    "command",
    "volumes",
    "ports",
    "environment",
    "depends_on",
    "restart",
    "healthcheck",
];

/// healthcheck 支持的字段。
const SUPPORTED_HEALTH_FIELDS: &[&str] = &["test", "interval", "retries", "start_period"];

/// 一个 service 的规格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub image: String,
    /// 覆盖镜像 Cmd。空 = 用镜像自带的。
    pub command: Vec<String>,
    pub volumes: Vec<String>,
    pub ports: Vec<String>,
    /// `KEY=VALUE` 形式（两种写法都归一到这里）
    pub environment: Vec<String>,
    pub depends_on: Vec<String>,
    pub restart: Option<String>,
    pub healthcheck: Option<HealthSpec>,
}

/// compose 的 healthcheck 子集。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSpec {
    /// 已归一成"交给 /bin/sh -c 的那条命令"
    pub test: String,
    pub interval: Option<u64>,
    pub retries: Option<u32>,
    pub start_period: Option<u64>,
}

/// 一份 compose 文件。
#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    /// 已按 `depends_on` 拓扑排好序：直接顺序启动即可。
    pub services: Vec<Service>,
}

impl Project {
    /// 容器名：`<project>_<service>`，与 docker compose 的命名习惯一致，
    /// 让同一台机器上跑多份 compose 不打架。
    pub fn container_name(&self, service: &str) -> String {
        format!("{}_{}", self.name, service)
    }
}

/// 解析 compose 文件内容。
pub fn parse(input: &str, project: &str) -> Result<Project> {
    let root = yaml::parse(input)?;
    // `version:` 是 compose v2 的遗留字段，v3 起已废弃。存在就忽略，不报错
    // ——报错会让大量现成文件用不了，而它对语义没有影响。
    let services_node = root
        .get("services")
        .ok_or_else(|| WboxError::args("compose 文件缺少顶层 `services:`"))?;
    let entries = services_node
        .as_map()
        .ok_or_else(|| WboxError::args("`services:` 下必须是服务名到配置的映射"))?;
    if entries.is_empty() {
        return Err(WboxError::args("`services:` 下没有任何服务"));
    }

    let mut services = Vec::new();
    for (name, node) in entries {
        services.push(parse_service(name, node)?);
    }
    // depends_on 引用的服务必须存在，否则会在启动时才炸，且错得莫名其妙
    let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
    for s in &services {
        for d in &s.depends_on {
            if !names.contains(&d.as_str()) {
                return Err(WboxError::args(format!(
                    "服务 '{}' 的 depends_on 指向不存在的服务 '{}'",
                    s.name, d
                )));
            }
        }
    }
    let services = topo_sort(services)?;
    Ok(Project {
        name: project.to_string(),
        services,
    })
}

fn parse_service(name: &str, node: &Node) -> Result<Service> {
    let map = node.as_map().ok_or_else(|| {
        WboxError::args(format!("服务 '{}' 的配置必须是映射", name))
    })?;
    for (k, _) in map {
        if !SUPPORTED_FIELDS.contains(&k.as_str()) {
            return Err(WboxError::args(format!(
                "服务 '{}' 使用了尚未支持的字段 '{}'（当前支持：{}）。\
                 明确报错而不是忽略——忽略会让你以为它生效了",
                name,
                k,
                SUPPORTED_FIELDS.join(", ")
            )));
        }
    }
    let image = node
        .get("image")
        .and_then(Node::as_str)
        .ok_or_else(|| {
            WboxError::args(format!(
                "服务 '{}' 缺少 `image:`（尚不支持 `build:`，请先 wbox build 再引用）",
                name
            ))
        })?
        .to_string();

    let list = |key: &str| -> Result<Vec<String>> {
        match node.get(key) {
            None => Ok(Vec::new()),
            Some(n) => n.as_list().ok_or_else(|| {
                WboxError::args(format!("服务 '{}' 的 `{}` 必须是标量或序列", name, key))
            }),
        }
    };

    // environment 两种写法都要收：序列 `- K=V` 与映射 `K: V`。
    // 归一成 `K=V`，下游只认一种形状。
    let environment = match node.get("environment") {
        None => Vec::new(),
        Some(Node::Map(m)) => m
            .iter()
            .map(|(k, v)| {
                let val = v.as_str().unwrap_or("");
                format!("{}={}", k, val)
            })
            .collect(),
        Some(other) => other.as_list().ok_or_else(|| {
            WboxError::args(format!("服务 '{}' 的 `environment` 形式不支持", name))
        })?,
    };

    Ok(Service {
        name: name.to_string(),
        image,
        command: list("command")?,
        volumes: list("volumes")?,
        ports: list("ports")?,
        environment,
        depends_on: list("depends_on")?,
        restart: node.get("restart").and_then(Node::as_str).map(str::to_string),
        healthcheck: match node.get("healthcheck") {
            None => None,
            Some(h) => Some(parse_health(name, h)?),
        },
    })
}

fn parse_health(service: &str, node: &Node) -> Result<HealthSpec> {
    let map = node.as_map().ok_or_else(|| {
        WboxError::args(format!("服务 '{}' 的 healthcheck 必须是映射", service))
    })?;
    for (k, _) in map {
        if !SUPPORTED_HEALTH_FIELDS.contains(&k.as_str()) {
            return Err(WboxError::args(format!(
                "服务 '{}' 的 healthcheck 使用了尚未支持的字段 '{}'（当前支持：{}）",
                service,
                k,
                SUPPORTED_HEALTH_FIELDS.join(", ")
            )));
        }
    }
    let raw = node.get("test").ok_or_else(|| {
        WboxError::args(format!("服务 '{}' 的 healthcheck 缺少 `test`", service))
    })?;
    let items = raw.as_list().ok_or_else(|| {
        WboxError::args(format!("服务 '{}' 的 healthcheck.test 形式不支持", service))
    })?;
    // docker 的 test 有 CMD / CMD-SHELL / NONE 三种前缀。wbox 的
    // --health-cmd 本就交给 /bin/sh -c，故两种前缀都归一成一条 shell 命令；
    // NONE 明确报错——它的语义是"禁用继承来的探针"，我们没有继承这回事。
    let test = match items.first().map(String::as_str) {
        Some("NONE") => {
            return Err(WboxError::args(format!(
                "服务 '{}' 的 healthcheck.test 为 NONE：wbox 不存在从镜像继承探针，\
                 无需禁用；请直接删掉 healthcheck",
                service
            )))
        }
        Some("CMD-SHELL") => items[1..].join(" "),
        Some("CMD") => items[1..].join(" "),
        _ => items.join(" "),
    };
    if test.trim().is_empty() {
        return Err(WboxError::args(format!(
            "服务 '{}' 的 healthcheck.test 为空",
            service
        )));
    }
    let dur = |key: &str| -> Result<Option<u64>> {
        match node.get(key).and_then(Node::as_str) {
            None => Ok(None),
            Some(s) => parse_duration(s)
                .map(Some)
                .ok_or_else(|| {
                    WboxError::args(format!(
                        "服务 '{}' 的 healthcheck.{} 取值 '{}' 无法解析（支持 30s / 5m / 纯秒数）",
                        service, key, s
                    ))
                }),
        }
    };
    let retries = match node.get("retries").and_then(Node::as_str) {
        None => None,
        Some(s) => Some(s.parse::<u32>().map_err(|_| {
            WboxError::args(format!(
                "服务 '{}' 的 healthcheck.retries 取值 '{}' 不是整数",
                service, s
            ))
        })?),
    };
    Ok(HealthSpec {
        test,
        interval: dur("interval")?,
        retries,
        start_period: dur("start_period")?,
    })
}

/// compose 的时长写法 → 秒。支持 `30s` / `5m` / `1h` / 纯数字。
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = match s.as_bytes()[s.len() - 1] {
        b's' => (&s[..s.len() - 1], 1),
        b'm' => (&s[..s.len() - 1], 60),
        b'h' => (&s[..s.len() - 1], 3600),
        _ => (s, 1),
    };
    num.trim().parse::<u64>().ok().map(|n| n * mult)
}

/// 按 `depends_on` 拓扑排序。
///
/// 只保证**启动顺序**，不做 docker 的 `condition: service_healthy`——那需要在
/// 启动过程中等待另一个容器变健康，是另一档复杂度。这一点在 PRD 里写明了，
/// 免得用户以为 depends_on 能保证"依赖已就绪"。
fn topo_sort(services: Vec<Service>) -> Result<Vec<Service>> {
    let n = services.len();
    let mut done = vec![false; n];
    let mut out: Vec<Service> = Vec::with_capacity(n);
    // 每轮挑出"依赖都已就位"的服务。挑不出来就说明有环。
    // 同一轮内按**原文件顺序**取，保证输出稳定可预期。
    while out.len() < n {
        let mut progressed = false;
        for i in 0..n {
            if done[i] {
                continue;
            }
            let ready = services[i]
                .depends_on
                .iter()
                .all(|d| out.iter().any(|s| &s.name == d));
            if ready {
                out.push(services[i].clone());
                done[i] = true;
                progressed = true;
            }
        }
        if !progressed {
            let stuck: Vec<&str> = (0..n)
                .filter(|i| !done[*i])
                .map(|i| services[i].name.as_str())
                .collect();
            return Err(WboxError::args(format!(
                "depends_on 存在循环依赖，涉及：{}",
                stuck.join(", ")
            )));
        }
    }
    Ok(out)
}

/// 把一个 service 翻译成 `wbox run` 的 argv。
///
/// `net_peer` 非 None 时加 `--network container:<peer>`：第一个服务持有网络，
/// 其余加入它（见模块文档的网络语义说明）。
pub fn run_args(
    project: &Project,
    svc: &Service,
    net_peer: Option<&str>,
    detach: bool,
) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    a.push("--name".into());
    a.push(project.container_name(&svc.name));
    if detach {
        a.push("-d".into());
    }
    for v in &svc.volumes {
        a.push("-v".into());
        a.push(v.clone());
    }
    for e in &svc.environment {
        a.push("-e".into());
        a.push(e.clone());
    }
    if let Some(peer) = net_peer {
        // 加入模式下 -p 会被 run 拒绝（本容器没有自己的网络可发布），
        // 故端口只在持有网络的那个服务上发布。
        a.push("--network".into());
        a.push(format!("container:{}", peer));
    } else {
        for p in &svc.ports {
            a.push("-p".into());
            a.push(p.clone());
        }
        // 有端口要发布就必须有网络；没有 -p 时保持默认（断网），
        // 与 run 的默认一致。
    }
    if let Some(r) = &svc.restart {
        a.push("--restart".into());
        a.push(r.clone());
    }
    if let Some(h) = &svc.healthcheck {
        a.push("--health-cmd".into());
        a.push(h.test.clone());
        if let Some(i) = h.interval {
            a.push("--health-interval".into());
            a.push(i.to_string());
        }
        if let Some(r) = h.retries {
            a.push("--health-retries".into());
            a.push(r.to_string());
        }
        if let Some(sp) = h.start_period {
            a.push("--health-start-period".into());
            a.push(sp.to_string());
        }
    }
    // 镜像必须在选项之后：run 的位置语义是"首个位置参数是镜像，
    // 其后全部属于 guest command"。
    a.push(svc.image.clone());
    a.extend(svc.command.iter().cloned());
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
services:
  web:
    image: nginx:1.25
    depends_on:
      - db
    ports:
      - \"8080:80\"
    environment:
      - MODE=prod
    healthcheck:
      test: [\"CMD-SHELL\", \"wget -q -O- localhost\"]
      interval: 5s
      retries: 2
  db:
    image: postgres:16
    volumes:
      - /srv/data:/var/lib/pg:ro
    restart: always
";

    #[test]
    fn parses_full_sample_and_orders_by_depends_on() {
        let p = parse(SAMPLE, "proj").unwrap();
        // db 被 web 依赖，必须排在前面
        assert_eq!(
            p.services.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["db", "web"]
        );
        let web = p.services.iter().find(|s| s.name == "web").unwrap();
        assert_eq!(web.image, "nginx:1.25");
        assert_eq!(web.ports, vec!["8080:80"]);
        assert_eq!(web.environment, vec!["MODE=prod"]);
        let h = web.healthcheck.as_ref().unwrap();
        assert_eq!(h.test, "wget -q -O- localhost");
        assert_eq!(h.interval, Some(5));
        assert_eq!(h.retries, Some(2));
        let db = p.services.iter().find(|s| s.name == "db").unwrap();
        assert_eq!(db.volumes, vec!["/srv/data:/var/lib/pg:ro"]);
        assert_eq!(db.restart.as_deref(), Some("always"));
        assert_eq!(p.container_name("db"), "proj_db");
    }

    /// environment 的两种写法必须等价——下游只认 `K=V` 一种形状。
    #[test]
    fn environment_map_and_seq_are_equivalent() {
        let seq = parse(
            "services:\n  a:\n    image: x\n    environment:\n      - K=V\n",
            "p",
        )
        .unwrap();
        let map = parse(
            "services:\n  a:\n    image: x\n    environment:\n      K: V\n",
            "p",
        )
        .unwrap();
        assert_eq!(seq.services[0].environment, map.services[0].environment);
        assert_eq!(seq.services[0].environment, vec!["K=V"]);
    }

    /// 没实现的字段必须报错。静默忽略会让用户以为它生效了——
    /// 对 `build:`/`networks:` 这类字段尤其危险。
    #[test]
    fn unsupported_service_field_is_rejected_and_lists_supported() {
        let e = parse(
            "services:\n  a:\n    image: x\n    networks:\n      - front\n",
            "p",
        )
        .unwrap_err();
        let m = format!("{}", e);
        assert!(m.contains("networks"), "{}", m);
        assert!(m.contains("当前支持"), "要列出支持的字段：{}", m);
        assert!(m.contains("以为它生效"), "要说清为什么报错：{}", m);
    }

    #[test]
    fn missing_image_points_at_build_limitation() {
        let e = parse("services:\n  a:\n    restart: always\n", "p").unwrap_err();
        assert!(format!("{}", e).contains("build"), "{}", e);
    }

    #[test]
    fn circular_depends_on_is_rejected_with_names() {
        let e = parse(
            "services:\n  a:\n    image: x\n    depends_on:\n      - b\n  b:\n    image: y\n    depends_on:\n      - a\n",
            "p",
        )
        .unwrap_err();
        let m = format!("{}", e);
        assert!(m.contains("循环依赖"), "{}", m);
        assert!(m.contains('a') && m.contains('b'), "要点名：{}", m);
    }

    #[test]
    fn depends_on_unknown_service_is_rejected() {
        let e = parse(
            "services:\n  a:\n    image: x\n    depends_on:\n      - ghost\n",
            "p",
        )
        .unwrap_err();
        assert!(format!("{}", e).contains("ghost"));
    }

    #[test]
    fn missing_services_key_is_rejected() {
        assert!(parse("version: '3'\n", "p").is_err());
        assert!(parse("services:\n", "p").is_err());
    }

    /// `version:` 是废弃字段，存在就忽略——报错会让大量现成文件用不了。
    #[test]
    fn legacy_version_field_is_tolerated() {
        let p = parse("version: '3.8'\nservices:\n  a:\n    image: x\n", "p").unwrap();
        assert_eq!(p.services.len(), 1);
    }

    #[test]
    fn duration_forms() {
        assert_eq!(parse_duration("30"), Some(30));
        assert_eq!(parse_duration("30s"), Some(30));
        assert_eq!(parse_duration("5m"), Some(300));
        assert_eq!(parse_duration("1h"), Some(3600));
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration(""), None);
    }

    /// NONE 的语义是"禁用继承来的探针"，而 wbox 没有继承这回事。
    #[test]
    fn healthcheck_none_is_rejected_with_explanation() {
        let e = parse(
            "services:\n  a:\n    image: x\n    healthcheck:\n      test: [\"NONE\"]\n",
            "p",
        )
        .unwrap_err();
        assert!(format!("{}", e).contains("无需禁用"), "{}", e);
    }

    /// 翻译成 run 的 argv：镜像必须在所有选项**之后**，否则 run 会把后续选项
    /// 当成 guest command 的一部分。
    #[test]
    fn run_args_put_image_after_options() {
        let p = parse(SAMPLE, "proj").unwrap();
        let db = p.services.iter().find(|s| s.name == "db").unwrap();
        let a = run_args(&p, db, None, true);
        let img = a.iter().position(|x| x == "postgres:16").unwrap();
        assert!(a.iter().position(|x| x == "-v").unwrap() < img);
        assert!(a.iter().position(|x| x == "--restart").unwrap() < img);
        assert_eq!(a[0], "--name");
        assert_eq!(a[1], "proj_db");
        assert!(a.contains(&"-d".to_string()));
    }

    /// 非首个服务加入首个服务的 netns；此时**不能**再带 -p
    /// （run 会拒绝：本容器没有自己的网络可发布）。
    #[test]
    fn joining_service_gets_network_container_and_no_ports() {
        let p = parse(SAMPLE, "proj").unwrap();
        let web = p.services.iter().find(|s| s.name == "web").unwrap();
        let a = run_args(&p, web, Some("proj_db"), true);
        assert!(a.windows(2).any(|w| w[0] == "--network" && w[1] == "container:proj_db"));
        assert!(!a.contains(&"-p".to_string()), "加入模式不该带 -p：{:?}", a);
        // healthcheck 照常翻译
        assert!(a.windows(2).any(|w| w[0] == "--health-cmd" && w[1].contains("wget")));
        assert!(a.windows(2).any(|w| w[0] == "--health-interval" && w[1] == "5"));
    }
}
