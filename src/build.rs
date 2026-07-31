//! `wbox build`：Dockerfile 子集构建（`PRD.md` F9.3）。
//!
//! 只实现自用场景够用的子集：`FROM` / `RUN` / `COPY` / `ENV` / `WORKDIR` /
//! `CMD` / `ENTRYPOINT` / `LABEL` / `EXPOSE` / `USER` / `ARG` / `ADD`，
//! 以及**多阶段构建**（`FROM <镜像> AS <名字>` + `COPY --from=<名字>`，F9.39）。
//! **未实现的指令一律明确报错**，不静默跳过——静默跳过会产出一个"看着构建成功、
//! 其实少做了事"的镜像，比构建失败难查得多。
//!
//! 后四条里有两条是**纯声明**，必须说清它们不做什么：`EXPOSE` 只写进镜像 config，
//! 不会真的发布端口（发布要 `-p`）；`USER` 同样只是镜像声明的默认身份，
//! 运行时是否生效取决于 `--user` 与那一格支持不支持（F9.7）。
//! 把声明当成生效，是这两条指令最常见的误解。
//!
//! `RUN` 复用现成的容器执行路径（`wbox run` 的镜像模式），所以构建期的隔离
//! 与运行期完全一致，不另起一套。

use crate::error::{Result, WboxError};
use std::path::{Path, PathBuf};

/// Dockerfile 里的一条指令（已解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    /// `FROM <镜像> [AS <阶段名>]`。`as_name` 供多阶段构建的 `COPY --from` 引用。
    From {
        image: String,
        as_name: Option<String>,
    },
    Run(String),
    /// `COPY [--from=<阶段>] <src> <dst>`：
    /// `from_stage` 为 `None` 时 src 相对构建上下文；为 `Some(名字)` 时
    /// src 是**该阶段产物 rootfs 内**的绝对路径（多阶段构建，F9.38）。
    /// dst 始终是容器内绝对路径。
    Copy {
        src: String,
        dst: String,
        from_stage: Option<String>,
    },
    /// `ADD <src> <dst>`：与 `COPY` 的差别只有一条——**src 是本地 tar 时自动解开**。
    /// 远程 URL **不做**（见解析处的说明）。
    Add {
        src: String,
        dst: String,
    },
    Env {
        key: String,
        value: String,
    },
    Workdir(String),
    Cmd(Vec<String>),
    Entrypoint(Vec<String>),
    /// `LABEL k=v`：写进镜像 config 的 `Labels`。
    Label {
        key: String,
        value: String,
    },
    /// `EXPOSE 80` / `EXPOSE 80/tcp`：**纯声明**，不会真的发布端口。
    Expose(String),
    /// `USER 1000[:1000]`：镜像声明的默认身份。
    User(String),
    /// `ARG k[=默认值]`：构建期变量。
    Arg {
        key: String,
        default: Option<String>,
    },
}

/// 解析 Dockerfile 文本。
///
/// 支持行尾 `\` 续行与 `#` 注释。**指令必须以 `FROM` 开头**，与 docker 一致：
/// 没有基础镜像就无从谈起，早报错好过构建到一半才发现。
pub fn parse_dockerfile(text: &str) -> Result<Vec<Instruction>> {
    let mut logical: Vec<String> = Vec::new();
    let mut pending = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_suffix('\\') {
            pending.push_str(rest.trim_end());
            pending.push(' ');
            continue;
        }
        pending.push_str(line);
        logical.push(std::mem::take(&mut pending));
    }
    if !pending.trim().is_empty() {
        return Err(WboxError::args("Dockerfile 以续行符 '\\' 结尾，指令不完整"));
    }

    let mut out = Vec::new();
    for line in &logical {
        let (verb, rest) = line
            .split_once(char::is_whitespace)
            .unwrap_or((line.as_str(), ""));
        let rest = rest.trim();
        let upper = verb.to_ascii_uppercase();
        let need = |what: &str| -> Result<()> {
            if rest.is_empty() {
                Err(WboxError::args(format!("{} 缺少{}", upper, what)))
            } else {
                Ok(())
            }
        };
        match upper.as_str() {
            "FROM" => {
                need("基础镜像")?;
                // `FROM x AS name`：AS 不区分大小写（docker 同此）
                let mut it = rest.split_whitespace();
                let image = it.next().unwrap_or_default().to_string();
                let as_name = match (it.next(), it.next()) {
                    (Some(kw), Some(name)) if kw.eq_ignore_ascii_case("AS") => {
                        Some(name.to_string())
                    }
                    (None, _) => None,
                    (Some(other), _) => {
                        return Err(WboxError::args(format!(
                            "FROM 只支持 `FROM <镜像> [AS <阶段名>]`，多出的 '{}' 无法解析",
                            other
                        )))
                    }
                };
                if it.next().is_some() {
                    return Err(WboxError::args(
                        "FROM 的参数过多（用法 FROM <镜像> [AS <阶段名>]）",
                    ));
                }
                out.push(Instruction::From { image, as_name });
            }
            "RUN" => {
                need("命令")?;
                out.push(Instruction::Run(rest.to_string()));
            }
            "COPY" => {
                let mut it = rest.split_whitespace().peekable();
                let mut from_stage = None;
                if let Some(first) = it.peek() {
                    if let Some(stage) = first.strip_prefix("--from=") {
                        if stage.is_empty() {
                            return Err(WboxError::args("COPY --from= 缺少阶段名"));
                        }
                        from_stage = Some(stage.to_string());
                        it.next();
                    }
                }
                let (Some(src), Some(dst)) = (it.next(), it.next()) else {
                    return Err(WboxError::args("COPY 需要 <src> <dst> 两个参数"));
                };
                if it.next().is_some() {
                    return Err(WboxError::args("COPY 暂只支持单个 src（多源未实现）"));
                }
                if from_stage.is_some() && !src.starts_with('/') {
                    // `--from` 的源在**那个阶段的 rootfs 内**，不是构建上下文里，
                    // 所以必须是绝对路径。相对路径没有可解释的基准点。
                    return Err(WboxError::args(format!(
                        "COPY --from 的源 '{}' 必须是该阶段内的绝对路径",
                        src
                    )));
                }
                out.push(Instruction::Copy {
                    src: src.to_string(),
                    dst: dst.to_string(),
                    from_stage,
                });
            }
            // `ADD` 与 `COPY` 只差一条：src 是本地 tar 时自动解开。
            //
            // **远程 URL 不做**，而且是明确拒绝而非静默当成路径：docker 自己的文档
            // 都建议别用（拿不到缓存、拿不到校验、构建期出网还常被审批挡住），
            // 而 wbox 的目标用户里正有"出网要审批"的那一类（§3.1）。
            // 要取远程文件，用 RUN + 你信任的下载工具，那样至少校验和重试都在你手里。
            "ADD" => {
                let mut it = rest.split_whitespace();
                let (Some(src), Some(dst)) = (it.next(), it.next()) else {
                    return Err(WboxError::args("ADD 需要 <src> <dst> 两个参数"));
                };
                if it.next().is_some() {
                    return Err(WboxError::args("ADD 暂只支持单个 src（多源未实现）"));
                }
                if src.starts_with("http://") || src.starts_with("https://") {
                    return Err(WboxError::args(format!(
                        "ADD 不支持远程 URL '{}'：构建期出网拿不到缓存与校验，\
                         且常被网络策略挡住。请用 RUN + 你信任的下载工具（校验与重试都在你手里）",
                        src
                    )));
                }
                out.push(Instruction::Add {
                    src: src.to_string(),
                    dst: dst.to_string(),
                });
            }
            "ENV" => {
                need("KEY=VALUE")?;
                let (k, v) = rest
                    .split_once('=')
                    .ok_or_else(|| WboxError::args("ENV 需要 KEY=VALUE 形式"))?;
                if k.trim().is_empty() {
                    return Err(WboxError::args("ENV 的键不能为空"));
                }
                out.push(Instruction::Env {
                    key: k.trim().to_string(),
                    value: v.trim().to_string(),
                });
            }
            "WORKDIR" => {
                need("路径")?;
                out.push(Instruction::Workdir(rest.to_string()));
            }
            "CMD" => {
                need("命令")?;
                out.push(Instruction::Cmd(parse_exec_form(rest)));
            }
            "LABEL" => {
                need("KEY=VALUE")?;
                let (k, v) = rest
                    .split_once('=')
                    .ok_or_else(|| WboxError::args("LABEL 需要 KEY=VALUE 形式"))?;
                if k.trim().is_empty() {
                    return Err(WboxError::args("LABEL 的键不能为空"));
                }
                // 值两侧的引号是 Dockerfile 的写法惯例（LABEL a="b c"），剥掉；
                // 中间的引号原样保留——那是值的一部分。
                out.push(Instruction::Label {
                    key: k.trim().to_string(),
                    value: strip_quotes(v.trim()).to_string(),
                });
            }
            "EXPOSE" => {
                need("端口")?;
                out.push(Instruction::Expose(rest.to_string()));
            }
            "USER" => {
                need("身份")?;
                out.push(Instruction::User(rest.to_string()));
            }
            "ARG" => {
                need("变量名")?;
                let (k, default) = match rest.split_once('=') {
                    Some((k, v)) => (k.trim(), Some(strip_quotes(v.trim()).to_string())),
                    None => (rest, None),
                };
                if k.is_empty() {
                    return Err(WboxError::args("ARG 的变量名不能为空"));
                }
                out.push(Instruction::Arg {
                    key: k.to_string(),
                    default,
                });
            }
            "ENTRYPOINT" => {
                need("命令")?;
                out.push(Instruction::Entrypoint(parse_exec_form(rest)));
            }
            other => {
                return Err(WboxError::args(format!(
                    "Dockerfile 指令 '{}' 未实现（本子集支持 FROM/RUN/COPY/ENV/WORKDIR/\
                     CMD/ENTRYPOINT/LABEL/EXPOSE/USER/ARG/ADD）；\
                     静默跳过会产出一个看似构建成功、实则少做了事的镜像",
                    other
                )));
            }
        }
    }
    match out.first() {
        Some(Instruction::From { .. }) => Ok(out),
        Some(_) => Err(WboxError::args("Dockerfile 的第一条指令必须是 FROM")),
        None => Err(WboxError::args("Dockerfile 为空")),
    }
}

/// 这个文件是不是 tar 归档。
///
/// **看内容不看扩展名**：`ADD payload /x` 里那个没有后缀的文件可能就是 tar，
/// 而 `notes.tar` 也可能只是个名字里带 tar 的文本。tar 在偏移 257 处有
/// `ustar` 魔数，读 265 字节就能定。
fn is_tar_archive(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 265];
    let Ok(n) = f.read(&mut head) else {
        return false;
    };
    n >= 265 && &head[257..262] == b"ustar"
}

/// 把 tar 解到 `dst`，返回解出的条目数。
///
/// 安全约束与 `wbox import` 同一套（F9.25）：逐条挡绝对路径与 `..`，
/// 一律拼到 `dst` 之下。Dockerfile 是本仓的输入不假，但 `ADD` 的那个 tar
/// 常常是第三方下载来的——按不可信输入处理才对。
fn extract_tar_into(src: &Path, dst: &Path) -> Result<usize> {
    let bytes = std::fs::read(src)
        .map_err(|e| WboxError::args(format!("读取 '{}' 失败：{}", src.display(), e)))?;
    let mut ar = wbox_codec::tar::Archive::new(std::io::Cursor::new(&bytes));
    let mut count = 0usize;
    let entries = ar
        .entries()
        .map_err(|e| WboxError::args(format!("读取归档失败：{}", e)))?;
    for entry in entries {
        let e = entry.map_err(|e| WboxError::args(format!("读取归档条目失败：{}", e)))?;
        let path = e
            .path()
            .map_err(|e| WboxError::args(format!("归档条目路径非法：{}", e)))?
            .into_owned();
        let mut rel = PathBuf::new();
        for c in path.components() {
            match c {
                std::path::Component::Normal(seg) => rel.push(seg),
                std::path::Component::CurDir => {}
                _ => {
                    return Err(WboxError::args(format!(
                        "ADD 的归档里含绝对路径或 '..'（'{}'），拒绝解包",
                        path.display()
                    )))
                }
            }
        }
        if rel.as_os_str().is_empty() {
            continue;
        }
        // 单条解不出来不中止：归档里常有本机建不出来的条目（设备节点要 root）。
        // 与 `import` 同一取舍。走 `unpack_in` 保证不会经由符号链接写出 dst。
        if e.unpack_in(dst, &rel).is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

/// 剥掉值两侧成对的引号（`LABEL a="b c"` 的写法惯例）。
///
/// 只剥**两侧成对**的那一层，中间的引号原样保留——它们是值的一部分。
/// 不做转义处理：处理了就得把一整套 shell 引用规则搬进来，而 Dockerfile 的
/// 这几条指令用不到，半套规则比没有更难预料。
fn strip_quotes(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// `CMD ["a","b"]`（exec 形式）或 `CMD a b`（shell 形式）。
///
/// shell 形式按 docker 语义应当经 `/bin/sh -c` 执行——这里如实照做，
/// 而不是简单按空格切分：`CMD echo a && echo b` 按空格切会得到一串无意义的
/// 参数，运行时才发现不对。
fn parse_exec_form(rest: &str) -> Vec<String> {
    let t = rest.trim();
    if t.starts_with('[') && t.ends_with(']') {
        if let Ok(wbox_codec::json::Value::Array(items)) = wbox_codec::json::from_str(t) {
            return items
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
    }
    vec!["/bin/sh".to_string(), "-c".to_string(), t.to_string()]
}

/// 把 `COPY` 的源路径解析到构建上下文内，并**拒绝逃出上下文**。
///
/// 这是安全断言而非参数校验：`COPY ../../etc/shadow /x` 若放行，构建就成了
/// 一个把宿主任意文件读进镜像的通道。
pub fn resolve_context_path(context: &Path, src: &str) -> Result<PathBuf> {
    let joined = context.join(src);
    let canon = joined
        .canonicalize()
        .map_err(|e| WboxError::args(format!("COPY 源 '{}' 不可用：{}", src, e)))?;
    let root = context.canonicalize().map_err(|e| {
        WboxError::args(format!("构建上下文 '{}' 不可用：{}", context.display(), e))
    })?;
    if !canon.starts_with(&root) {
        return Err(WboxError::args(format!(
            "COPY 源 '{}' 逃出了构建上下文——不允许把上下文之外的宿主文件打进镜像",
            src
        )));
    }
    Ok(canon)
}

/// 把一个容器内绝对路径解析到宿主上的某棵 rootfs 里，并**拒绝逃出 rootfs**。
///
/// 三处在用：build 的 `COPY`/`ADD` 目标与 `COPY --from` 源、`wbox cp` 的容器端
/// 路径、以及 `layers.rs` 的分层查找。都是同一类输入（用户或镜像给的容器内
/// 路径），所以共用同一份校验——分头写迟早有一份漏掉。
///
/// # 只消解 `..` 是不够的（PRD L8）
///
/// 早先这里是**纯词法**的：逐段消解 `..`，从不看符号链接。可镜像是外部输入，
/// 里面放一个 `/evil -> /home/someone`（宿主绝对路径）就够了——`COPY x /evil/y`
/// 词法上老老实实待在 rootfs 里，落盘时宿主却跟着 `evil` 走出去，在**宿主**
/// 上写了文件。`..` 只是逃逸的一种写法，符号链接是另一种，挡一半等于没挡。
///
/// 现在委托给 [`wbox_codec::path::resolve_in_root`]：逐段展开，链接目标重新
/// 以 rootfs 为根解析。这与 guest 运行期 VFS（L12）是同一套策略，跟归档解包
/// （L7，[`wbox_codec::tar::safe_join`]）的"见链接即拒"是有意的不同——归档
/// 完全不可信，"跟着链接走"没有合理语义；而镜像里的 `/etc -> /usr/etc` 是
/// 正常内容，该在容器内照常生效。
pub fn resolve_rootfs_path(rootfs: &Path, dst: &str) -> Result<PathBuf> {
    if !dst.starts_with('/') {
        return Err(WboxError::args(format!(
            "容器内路径 '{}' 必须以 / 开头（Dockerfile 的 COPY 目标同此要求）",
            dst
        )));
    }
    // 末段跟随：底层的 `File::create` / 读取本来就会跟随，这里跟随只是把它
    // 提前到**受限**的解析里做，免得宿主替我们跟出 rootfs 去。
    wbox_codec::path::resolve_in_root(rootfs, Path::new(dst), true).map_err(|e| match e {
        wbox_codec::path::ResolveError::Escaped => {
            WboxError::args(format!("容器内路径 '{}' 用 '..' 逃出了 rootfs", dst))
        }
        wbox_codec::path::ResolveError::Loop => WboxError::args(format!(
            "容器内路径 '{}' 的符号链接成环（或嵌套超过 {} 层）",
            dst,
            wbox_codec::path::MAX_SYMLINK_DEPTH
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 新增的四条指令：LABEL/EXPOSE/USER 进镜像 config，ARG **不进**
    /// （构建参数常带凭证，落进镜像等于随镜像发出去）。
    #[test]
    fn parses_label_expose_user_arg() {
        let df = "FROM alpine:3.20\n\
                  LABEL org.opencontainers.image.title=\"my app\"\n\
                  EXPOSE 80\n\
                  EXPOSE 8443/tcp\n\
                  USER 1000:1000\n\
                  ARG BUILD_ID=42\n\
                  ARG NO_DEFAULT\n";
        let got = parse_dockerfile(df).unwrap();
        assert_eq!(
            got[1],
            Instruction::Label {
                key: "org.opencontainers.image.title".into(),
                // 两侧成对的引号剥掉，中间的原样留（这里没有中间的）
                value: "my app".into()
            }
        );
        assert_eq!(got[2], Instruction::Expose("80".into()));
        assert_eq!(got[3], Instruction::Expose("8443/tcp".into()));
        assert_eq!(got[4], Instruction::User("1000:1000".into()));
        assert_eq!(
            got[5],
            Instruction::Arg {
                key: "BUILD_ID".into(),
                default: Some("42".into())
            }
        );
        assert_eq!(
            got[6],
            Instruction::Arg {
                key: "NO_DEFAULT".into(),
                default: None
            }
        );

        // 缺参数一律报错，不静默跳过
        assert!(parse_dockerfile("FROM a\nLABEL\n").is_err());
        assert!(parse_dockerfile("FROM a\nLABEL nokey\n").is_err());
        assert!(parse_dockerfile("FROM a\nEXPOSE\n").is_err());
        assert!(parse_dockerfile("FROM a\nUSER\n").is_err());
        assert!(parse_dockerfile("FROM a\nARG\n").is_err());
    }

    /// 这四条落进 config 的形状要与 docker/OCI 一致，否则 `run` 读不回来。
    #[test]
    fn config_json_shape_for_the_new_instructions() {
        let mut cfg = ConfigAccum::default();
        cfg.labels.push(("a".into(), "1".into()));
        cfg.exposed.push("80/tcp".into());
        cfg.user = Some("1000".into());
        cfg.args.push(("SECRET".into(), "shh".into()));
        let v: wbox_codec::json::Value = wbox_codec::json::from_str(&cfg.to_json()).unwrap();
        assert_eq!(v["config"]["Labels"]["a"], "1");
        // ExposedPorts 的值是空对象，与 OCI 一致
        assert!(v["config"]["ExposedPorts"]["80/tcp"].is_object());
        assert_eq!(v["config"]["User"], "1000");
        // ARG 绝不能出现在镜像 config 里
        assert!(
            !cfg.to_json().contains("SECRET"),
            "构建参数不该落进镜像 config：它常带凭证"
        );
    }

    /// 只剥两侧成对的那一层，中间的引号是值的一部分。
    #[test]
    fn strip_quotes_only_removes_a_matched_outer_pair() {
        assert_eq!(strip_quotes("\"a b\""), "a b");
        assert_eq!(strip_quotes("'a b'"), "a b");
        assert_eq!(strip_quotes("a\"b"), "a\"b");
        assert_eq!(strip_quotes("\"unbalanced"), "\"unbalanced");
        assert_eq!(strip_quotes("plain"), "plain");
    }

    #[test]
    fn parses_supported_subset() {
        let df = r#"
# 注释行
FROM alpine:3.20
ENV FOO=bar
WORKDIR /app
COPY app.sh /app/app.sh
RUN chmod +x /app/app.sh && \
    echo done
ENTRYPOINT ["/app/app.sh"]
CMD ["--help"]
"#;
        let got = parse_dockerfile(df).unwrap();
        assert_eq!(
            got[0],
            Instruction::From {
                image: "alpine:3.20".into(),
                as_name: None
            }
        );
        assert_eq!(
            got[1],
            Instruction::Env {
                key: "FOO".into(),
                value: "bar".into()
            }
        );
        assert_eq!(got[2], Instruction::Workdir("/app".into()));
        // 续行要被拼成一条
        assert_eq!(
            got[4],
            Instruction::Run("chmod +x /app/app.sh && echo done".into())
        );
        assert_eq!(got[5], Instruction::Entrypoint(vec!["/app/app.sh".into()]));
    }

    /// shell 形式必须包成 /bin/sh -c，不能按空格切——否则 `CMD echo a && echo b`
    /// 会变成一串无意义的参数，运行时才暴露。
    #[test]
    fn shell_form_goes_through_sh_c() {
        let got = parse_dockerfile("FROM x\nCMD echo a && echo b").unwrap();
        assert_eq!(
            got[1],
            Instruction::Cmd(vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo a && echo b".into()
            ])
        );
    }

    /// 未实现的指令要**报错**而不是跳过。
    #[test]
    fn unknown_instruction_is_rejected_not_skipped() {
        let e = parse_dockerfile("FROM x\nVOLUME /data").unwrap_err();
        let m = format!("{}", e);
        assert!(m.contains("未实现"), "{}", m);
        assert!(m.contains("静默跳过"), "要解释为什么不跳过：{}", m);
    }

    #[test]
    fn requires_from_first_and_rejects_malformed() {
        assert!(parse_dockerfile("RUN echo hi").is_err(), "首条须 FROM");
        assert!(parse_dockerfile("").is_err(), "空文件");
        assert!(parse_dockerfile("FROM").is_err(), "FROM 缺参数");
        assert!(
            parse_dockerfile("FROM x\nCOPY only-one").is_err(),
            "COPY 缺参数"
        );
        assert!(
            parse_dockerfile("FROM x\nENV noequals").is_err(),
            "ENV 需 K=V"
        );
        assert!(
            parse_dockerfile("FROM x\nRUN cmd \\").is_err(),
            "续行未收尾"
        );
    }

    /// COPY 源不得逃出构建上下文——否则构建成了读取宿主任意文件的通道。
    #[test]
    fn copy_source_cannot_escape_context() {
        let root = std::env::temp_dir().join(format!("wbox-bctx-{}", std::process::id()));
        let ctx = root.join("context");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(ctx.join("sub")).unwrap();
        std::fs::write(ctx.join("sub/f.txt"), b"x").unwrap();
        // 越界源必须真实存在，否则 canonicalize 只测到“不存在”，没有覆盖
        // starts_with(root) 这条安全断言。放在同一临时根下可跨 Windows/Linux。
        std::fs::write(root.join("outside.txt"), b"host-only").unwrap();
        assert!(resolve_context_path(&ctx, "sub/f.txt").is_ok());
        let e = resolve_context_path(&ctx, "../outside.txt").unwrap_err();
        assert!(format!("{}", e).contains("逃出"), "{}", e);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// COPY 目标不得用 `..` 逃出 rootfs。
    #[test]
    fn copy_destination_cannot_escape_rootfs() {
        let root = Path::new("/tmp/rootfs");
        assert_eq!(
            resolve_rootfs_path(root, "/app/x").unwrap(),
            PathBuf::from("/tmp/rootfs/app/x")
        );
        // 深度足够时 `..` 只是正常的路径消解
        assert_eq!(
            resolve_rootfs_path(root, "/app/../x").unwrap(),
            PathBuf::from("/tmp/rootfs/x")
        );
        assert!(
            resolve_rootfs_path(root, "/../escape").is_err(),
            "越界的 .. 要拒绝"
        );
        assert!(resolve_rootfs_path(root, "relative").is_err(), "须绝对路径");
    }
}

// ---------------------------------------------------------------------------
// 构建编排
// ---------------------------------------------------------------------------

/// `wbox build` 的参数。
pub struct BuildOptions {
    pub context: PathBuf,
    pub dockerfile: PathBuf,
    /// `-t name:tag`
    pub tag: String,
}

/// 累积 Dockerfile 里的配置类指令，最后写成 `config.json`。
#[derive(Default)]
struct ConfigAccum {
    env: Vec<(String, String)>,
    workdir: Option<String>,
    cmd: Option<Vec<String>>,
    entrypoint: Option<Vec<String>>,
    /// `LABEL` 累积（同键后写覆盖先写，与 `ENV` 一致）。
    labels: Vec<(String, String)>,
    /// `EXPOSE` 声明的端口，形如 `80/tcp`。
    exposed: Vec<String>,
    /// `USER` 声明的默认身份。
    user: Option<String>,
    /// `ARG` 的构建期变量（含默认值）。**不进镜像 config**——
    /// 构建参数常带凭证，落进镜像等于把它发出去。
    args: Vec<(String, String)>,
}

impl ConfigAccum {
    /// 产出与 `oci::config::ImageConfig::load` 能读回的同一份结构。
    /// 复用同一套字段名不是巧合——构建产物必须与 `pull` 下来的镜像**无差别**，
    /// 否则 `run` 会对自家构建的镜像另眼相待。
    fn to_json(&self) -> String {
        let env: Vec<String> = self
            .env
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        let mut cfg = wbox_codec::json::Map::new();
        cfg.insert("Env".into(), wbox_codec::json!(env));
        if let Some(w) = &self.workdir {
            cfg.insert("WorkingDir".into(), wbox_codec::json!(w));
        }
        if let Some(c) = &self.cmd {
            cfg.insert("Cmd".into(), wbox_codec::json!(c));
        }
        if let Some(e) = &self.entrypoint {
            cfg.insert("Entrypoint".into(), wbox_codec::json!(e));
        }
        if !self.labels.is_empty() {
            let map: wbox_codec::json::Map = self
                .labels
                .iter()
                .map(|(k, v)| (k.clone(), wbox_codec::json!(v)))
                .collect();
            cfg.insert("Labels".into(), wbox_codec::json::Value::Object(map));
        }
        if !self.exposed.is_empty() {
            // OCI/docker 的形状是 {"80/tcp": {}}，值是个空对象
            let map: wbox_codec::json::Map = self
                .exposed
                .iter()
                .map(|p| (p.clone(), wbox_codec::json!({})))
                .collect();
            cfg.insert("ExposedPorts".into(), wbox_codec::json::Value::Object(map));
        }
        if let Some(u) = &self.user {
            cfg.insert("User".into(), wbox_codec::json!(u));
        }
        // ARG 刻意不写进 config：构建参数常带凭证（token、密码），
        // 落进镜像等于随镜像一起发出去。docker 也不把 ARG 写进 config。
        wbox_codec::json!({ "config": cfg }).to_string()
    }
}

#[cfg(windows)]
struct BuildStaging(PathBuf);

#[cfg(windows)]
impl Drop for BuildStaging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 递归复制目录。构建的第一步是把基础镜像的 rootfs 整份拷出来当作可写层。
///
/// 不做硬链接/overlay：那是性能优化，而 overlayfs 在 rootless 下未必可用
/// （PRD §2.4 的差距表里"无 overlay"就是这条）。先要正确，再谈快。
#[cfg(not(windows))]
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let fail = |what: &str, p: &Path, e: std::io::Error| {
        WboxError::args(format!("{} '{}' 失败：{}", what, p.display(), e))
    };
    std::fs::create_dir_all(dst).map_err(|e| fail("创建目录", dst, e))?;
    let rd = std::fs::read_dir(src).map_err(|e| fail("读取目录", src, e))?;
    for ent in rd {
        let ent = ent.map_err(|e| fail("枚举目录项", src, e))?;
        let from = ent.path();
        let to = dst.join(ent.file_name());
        let ft = ent.file_type().map_err(|e| fail("读取类型", &from, e))?;
        if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else if ft.is_symlink() {
            // 符号链接原样复制（保留目标字符串），不跟随——跟随会把链接指向的
            // 内容复制进来，改变镜像语义。
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&from).map_err(|e| fail("读取链接", &from, e))?;
                let _ = std::fs::remove_file(&to);
                std::os::unix::fs::symlink(&target, &to).map_err(|e| fail("创建链接", &to, e))?;
            }
        } else {
            // **先删再写**：staging 里的文件可能是与基础镜像缓存共享 inode 的
            // 硬链接（见 link_tree），`fs::copy` 会 truncate 后就地写，
            // 那会改到别的镜像的内容——最不该引入的一类缺陷。
            let _ = std::fs::remove_file(&to);
            std::fs::copy(&from, &to).map_err(|e| fail("复制文件", &from, e))?;
        }
    }
    Ok(())
}

/// 用**硬链接**把基础 rootfs 铺进 staging（PRD L5b 磁盘侧）。
///
/// 目录照建、符号链接照造，普通文件只建硬链接——两个镜像共享同一份数据块，
/// 磁盘占用近似只有一份。前提是此后**没有任何路径就地改写**这些文件：
/// `COPY` 与合并都先 `unlink` 再落盘，`RUN` 的写入走 overlay 落在 upper 里。
/// 这条纪律一旦破了就会写坏基础镜像缓存，故 OVB 门禁直接核对基础镜像未被改动。
///
/// 硬链接失败（跨设备等）时退回按字节复制：省磁盘是优化，正确性不能让。
#[cfg(not(windows))]
pub(crate) fn link_tree(src: &Path, dst: &Path) -> Result<()> {
    let fail = |what: &str, p: &Path, e: std::io::Error| {
        WboxError::args(format!("{} '{}' 失败：{}", what, p.display(), e))
    };
    std::fs::create_dir_all(dst).map_err(|e| fail("创建目录", dst, e))?;
    for ent in std::fs::read_dir(src).map_err(|e| fail("读取目录", src, e))? {
        let ent = ent.map_err(|e| fail("枚举目录项", src, e))?;
        let from = ent.path();
        let to = dst.join(ent.file_name());
        let ft = ent.file_type().map_err(|e| fail("读取类型", &from, e))?;
        if ft.is_dir() {
            link_tree(&from, &to)?;
        } else if ft.is_symlink() {
            let target = std::fs::read_link(&from).map_err(|e| fail("读取链接", &from, e))?;
            let _ = std::fs::remove_file(&to);
            std::os::unix::fs::symlink(&target, &to).map_err(|e| fail("创建链接", &to, e))?;
        } else {
            let _ = std::fs::remove_file(&to);
            if std::fs::hard_link(&from, &to).is_err() {
                std::fs::copy(&from, &to).map_err(|e| fail("复制文件", &from, e))?;
            }
        }
    }
    Ok(())
}

/// 该路径是不是 overlay 的 whiteout（字符设备 0:0）。
///
/// rootless overlay 用它表示"下层的这个条目被删了"，**文件和目录都是这个形态**
/// （目录要挂载时带 `userxattr` 才行，见 F9.12）。注意它与 tar 层的 `.wh.`
/// 前缀**不是**一套，两处判别不能互相套用。
#[cfg(not(windows))]
fn is_whiteout(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;
    meta.file_type().is_char_device() && meta.rdev() == 0
}

/// overlay 目录是否被标记为 opaque（其下层内容应整体丢弃）。
///
/// `userxattr` 模式下标记写在 `user.overlay.opaque`。不认这个标记的话，
/// "删掉整个目录再重建"会变成"新旧内容混在一起"。
#[cfg(not(windows))]
fn is_opaque(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let name = c"user.overlay.opaque";
    let mut buf = [0u8; 4];
    let n = unsafe {
        libc::getxattr(
            c.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    n > 0 && buf[0] == b'y'
}

/// 把一次 `RUN` 在 overlay upper 里积下的改动合并回 staging。
///
/// 三类条目，与 overlay 的表示一一对应：
/// - whiteout（字符设备 0:0）→ 删掉 staging 里的同名条目；
/// - 目录 → staging 建同名目录后递归；带 opaque 标记的先清空；
/// - 其余 → **先 unlink 再落盘**，只对改动过的文件断开硬链接。
#[cfg(not(windows))]
pub(crate) fn merge_overlay_upper(upper: &Path, target: &Path) -> Result<()> {
    let fail = |what: &str, p: &Path, e: std::io::Error| {
        WboxError::args(format!("{} '{}' 失败：{}", what, p.display(), e))
    };
    if !upper.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(target).map_err(|e| fail("创建目录", target, e))?;
    for ent in std::fs::read_dir(upper).map_err(|e| fail("读取目录", upper, e))? {
        let ent = ent.map_err(|e| fail("枚举目录项", upper, e))?;
        let from = ent.path();
        let to = target.join(ent.file_name());
        let meta = std::fs::symlink_metadata(&from).map_err(|e| fail("读取元数据", &from, e))?;
        if is_whiteout(&meta) {
            let _ = std::fs::remove_file(&to);
            let _ = std::fs::remove_dir_all(&to);
        } else if meta.is_dir() {
            if is_opaque(&from) {
                let _ = std::fs::remove_dir_all(&to);
            }
            merge_overlay_upper(&from, &to)?;
        } else {
            let _ = std::fs::remove_file(&to);
            let _ = std::fs::remove_dir_all(&to);
            // 同一文件系统，rename 是搬移不是复制——不额外占磁盘
            if std::fs::rename(&from, &to).is_err() {
                if meta.file_type().is_symlink() {
                    let t = std::fs::read_link(&from).map_err(|e| fail("读取链接", &from, e))?;
                    std::os::unix::fs::symlink(&t, &to).map_err(|e| fail("创建链接", &to, e))?;
                } else {
                    std::fs::copy(&from, &to).map_err(|e| fail("复制文件", &from, e))?;
                }
            }
        }
    }
    Ok(())
}

/// 构建缓存的根目录。与镜像缓存并列，用户清理时一处就够。
fn cache_root() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| WboxError::args("无法确定用户主目录"))?;
    Ok(PathBuf::from(home).join(".wbox").join("buildcache"))
}

/// 逐步累进的缓存键。
///
/// 键必须覆盖**该步之前的全部输入**，否则会命中一个状态不同的快照——那是
/// 缓存最危险的失效方式：构建"成功"了，内容却是错的。因此每步都把上一步的
/// 键一起哈希进去（链式），而 `COPY` 还要额外把**源文件内容**算进去，
/// 只看路径的话改了文件也会命中旧层。
fn step_key(prev: &str, ins: &Instruction, context: &Path) -> Result<String> {
    let mut h = wbox_codec::Sha256::new();
    h.update(prev.as_bytes());
    h.update(format!("{:?}", ins).as_bytes());
    // 源内容变了就必须失效。目录则按"路径+内容"逐个文件累加。
    //
    // **`ADD` 必须和 `COPY` 一起列在这里**：只哈希指令文本的话，源文件改了键不变，
    // 后续步骤会命中旧快照，把**旧内容悄悄烤进镜像**——构建"成功"，内容是错的。
    let src = match ins {
        // `COPY --from=<阶段>` 的源在**那个阶段的产物里**，不在构建上下文里——
        // 拿去 `resolve_context_path` 会直接报"源不可用"。它的内容由前面那些
        // 指令决定，而那些指令已经进了累加的键链，所以这里跳过是安全的。
        Instruction::Copy {
            from_stage: Some(_),
            ..
        } => None,
        Instruction::Copy { src, .. } | Instruction::Add { src, .. } => Some(src),
        _ => None,
    };
    if let Some(src) = src {
        let from = resolve_context_path(context, src)?;
        hash_path_into(&from, &mut h)?;
    }
    Ok(wbox_codec::sha256::hex(&h.finalize()))
}

fn hash_path_into(p: &Path, h: &mut wbox_codec::Sha256) -> Result<()> {
    let md = std::fs::metadata(p)
        .map_err(|e| WboxError::args(format!("读取 '{}' 失败：{}", p.display(), e)))?;
    if md.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(p)
            .map_err(|e| WboxError::args(format!("读取目录 '{}' 失败：{}", p.display(), e)))?
            .flatten()
            .map(|e| e.path())
            .collect();
        // 目录枚举顺序不保证稳定；不排序会让同样的内容算出不同的键，
        // 缓存就永远命不中。
        entries.sort();
        for e in entries {
            h.update(e.file_name().unwrap_or_default().as_encoded_bytes());
            hash_path_into(&e, h)?;
        }
    } else {
        let data = std::fs::read(p)
            .map_err(|e| WboxError::args(format!("读取 '{}' 失败：{}", p.display(), e)))?;
        h.update(&data);
    }
    Ok(())
}

#[cfg(windows)]
fn copy_file_contents(src: &Path, dst: &Path) -> Result<()> {
    let mut input = std::fs::File::open(src)
        .map_err(|e| WboxError::args(format!("打开 COPY 源 '{}' 失败：{}", src.display(), e)))?;
    let mut output = std::fs::File::create(dst)
        .map_err(|e| WboxError::args(format!("创建 COPY 目标 '{}' 失败：{}", dst.display(), e)))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|e| WboxError::args(format!("COPY '{}' 失败：{}", src.display(), e)))?;
    Ok(())
}

fn copy_build_tree(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            WboxError::args(format!(
                "创建复制目标父目录 '{}' 失败：{}",
                parent.display(),
                e
            ))
        })?;
    }
    #[cfg(windows)]
    {
        crate::backend::copy_rootfs_tree(src, dst)
    }
    #[cfg(not(windows))]
    {
        copy_tree(src, dst)
    }
}

/// 执行构建。
pub fn run_build(opts: &BuildOptions) -> Result<u32> {
    let text = std::fs::read_to_string(&opts.dockerfile).map_err(|e| {
        WboxError::args(format!(
            "读取 Dockerfile '{}' 失败：{}",
            opts.dockerfile.display(),
            e
        ))
    })?;
    let instructions = parse_dockerfile(&text)?;

    // 目标镜像目录：与 pull 用同一套布局，run 才认得
    let target = crate::oci::ImageRef::parse(&opts.tag, None)?;
    let out_dir = crate::oci::image_dir(&target)?;
    #[cfg(windows)]
    let staging = {
        let dir = out_dir.with_file_name(format!(
            ".wbox-build-{}-{}",
            std::process::id(),
            out_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("image")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        BuildStaging(dir)
    };
    #[cfg(windows)]
    let build_dir = staging.0.clone();
    #[cfg(not(windows))]
    let build_dir = out_dir.clone();
    // 最终阶段的产物目录。多阶段构建时前面几个阶段各建到自己的临时目录，
    // 只有最后一个阶段写到这里——它才是这次 build 的输出。
    let final_build_dir = build_dir.clone();
    let mut build_dir = build_dir;
    let mut rootfs = build_dir.join("rootfs");

    // ---- 分层缓存：先算出每步的键，再找**最长的已缓存前缀** ----
    //
    // 一步步"命中就恢复"会在每个命中步都复制一次快照，白做无用功；先定位到
    // 最后一个命中点、只恢复那一次，后面的步骤照常执行。
    let mut keys: Vec<String> = Vec::with_capacity(instructions.len());
    let mut k = String::new();
    for ins in &instructions {
        k = step_key(&k, ins, &opts.context)?;
        keys.push(k.clone());
    }
    let cache = cache_root()?;
    // 只有会改动 rootfs 的步骤才值得做快照；纯配置指令（ENV/CMD/...）不动
    // 文件系统，缓存它们只是徒增磁盘占用。
    let mutating = |i: &Instruction| {
        matches!(
            i,
            Instruction::Run(_) | Instruction::Copy { .. } | Instruction::Add { .. }
        )
    };
    // 每条指令属于第几个阶段（`FROM` 开一个新阶段）。
    let mut stage_of: Vec<usize> = Vec::with_capacity(instructions.len());
    let mut stage_count = 0usize;
    for ins in &instructions {
        if matches!(ins, Instruction::From { .. }) {
            stage_count += 1;
        }
        stage_of.push(stage_count.saturating_sub(1));
    }
    let multi_stage = stage_count > 1;

    let mut resume_from = 0usize;
    if multi_stage {
        // **多阶段时先禁用前缀缓存**。缓存键是沿指令序列累加的，跨阶段复用快照
        // 会把 A 阶段的 rootfs 恢复到 B 阶段头上——那是**错的**缓存，
        // 而错的缓存比没有缓存糟得多（构建"成功"，内容是别的阶段的）。
        // 宁可不缓存；等阶段内独立键做出来再打开。
        println!(
            "wbox: 多阶段构建（{} 个阶段），本次禁用构建缓存",
            stage_count
        );
    } else {
        for idx in (0..instructions.len()).rev() {
            if mutating(&instructions[idx]) && cache.join(&keys[idx]).join("rootfs").is_dir() {
                resume_from = idx + 1;
                break;
            }
        }
    }
    // 阶段名 → 该阶段产物 rootfs，供 `COPY --from` 取用。
    let mut stage_roots: std::collections::HashMap<String, std::path::PathBuf> =
        std::collections::HashMap::new();
    // 非最终阶段的临时目录，构建结束后清掉
    let mut stage_dirs: Vec<std::path::PathBuf> = Vec::new();

    // 记住基础镜像目录：构建结束后要按它算增量层（PRD L5b）。
    let mut base_image_dir: Option<std::path::PathBuf> = None;
    // 基础 rootfs 是否以硬链接铺开。为真时 staging 与基础镜像共享 inode，
    // 所有写入路径都必须遵守"先 unlink / 走 overlay"的纪律。
    #[allow(unused_mut)]
    let mut linked_base = false;
    let mut cfg = ConfigAccum::default();
    for (idx, ins) in instructions.iter().enumerate() {
        let step = idx + 1;
        // 命中前缀内的**改动型**步骤整体跳过；配置型指令仍要重放，
        // 否则 ENV/CMD/ENTRYPOINT 不会进最终 config.json。
        if idx < resume_from && mutating(ins) {
            if idx + 1 == resume_from {
                println!("[{}/{}] CACHED（复用已缓存层）", step, instructions.len());
                let _ = std::fs::remove_dir_all(&rootfs);
                copy_build_tree(&cache.join(&keys[idx]).join("rootfs"), &rootfs)?;
            }
            continue;
        }
        match ins {
            Instruction::From {
                image: base,
                as_name,
            } => {
                // 切到本阶段自己的产物目录。最后一个阶段写最终目录，
                // 前面的各写一个临时目录（`COPY --from` 从那里取）。
                let this_stage = stage_of[idx];
                if this_stage + 1 < stage_count {
                    let dir = final_build_dir.with_file_name(format!(
                        ".wbox-stage-{}-{}",
                        std::process::id(),
                        this_stage
                    ));
                    build_dir = dir.clone();
                    stage_dirs.push(dir);
                } else {
                    build_dir = final_build_dir.clone();
                }
                rootfs = build_dir.join("rootfs");
                // 每个阶段的 config 独立：A 阶段的 ENV/CMD 不该漏进 B 阶段，
                // 最终镜像只该带最后一个阶段的配置（docker 同此语义）。
                cfg = ConfigAccum::default();
                if let Some(name) = as_name {
                    stage_roots.insert(name.clone(), rootfs.clone());
                }
                let base_ref = crate::oci::ImageRef::parse(base, None)?;
                let base_dir = crate::oci::image_dir(&base_ref)?;
                if !base_dir.join("rootfs").is_dir() {
                    return Err(WboxError::args(format!(
                        "基础镜像 '{}' 未 pull（先 `wbox image pull {}`）",
                        base, base
                    )));
                }
                match as_name {
                    Some(n) => println!("[{}/{}] FROM {} AS {}", step, instructions.len(), base, n),
                    None => println!("[{}/{}] FROM {}", step, instructions.len(), base),
                }
                // 重建输出目录：残留的上一次构建会让结果不可复现
                crate::fsutil::remove_tree(&build_dir);
                std::fs::create_dir_all(&build_dir)
                    .map_err(|e| WboxError::args(format!("创建镜像目录失败：{}", e)))?;
                // 若后面有缓存命中，这份基础 rootfs 会被命中层整体覆盖；
                // 仍然先铺一份，保证"无命中"与"部分命中"两条路径起点一致。
                // 硬链接铺基础层（PRD L5b）：两个镜像共享数据块，磁盘近似
                // 只占一份。前提是此后没有任何就地改写——`COPY` 与 overlay
                // 合并都先 unlink，`RUN` 的写入落在 overlay upper 里。
                // 三个条件缺一不可，任一不满足就退回按字节整份复制。
                #[cfg(not(windows))]
                {
                    linked_base = std::env::var_os("WBOX_NO_OVERLAY").is_none()
                        && crate::backend::rootless_overlay_available();
                }
                if linked_base {
                    #[cfg(not(windows))]
                    link_tree(&base_dir.join("rootfs"), &rootfs)?;
                } else {
                    copy_build_tree(&base_dir.join("rootfs"), &rootfs)?;
                }
                base_image_dir = Some(base_dir.clone());
                // 继承基础镜像的 config，再让后续指令覆盖
                if let Some(base_cfg) = crate::oci::config::ImageConfig::load(&base_dir)? {
                    cfg.env.extend(base_cfg.env.iter().cloned());
                    cfg.workdir = base_cfg.working_dir.clone();
                }
            }
            Instruction::Copy {
                src,
                dst,
                from_stage,
            } => {
                match from_stage {
                    Some(st) => println!(
                        "[{}/{}] COPY --from={} {} {}",
                        step,
                        instructions.len(),
                        st,
                        src,
                        dst
                    ),
                    None => println!("[{}/{}] COPY {} {}", step, instructions.len(), src, dst),
                }
                let from = match from_stage {
                    // 从某个阶段取：源在**那个阶段的 rootfs 内**。
                    // 走 `resolve_rootfs_path` 是为了复用同一份 `..` 逃逸校验——
                    // 阶段名是 Dockerfile 给的，路径却可能是拼出来的。
                    Some(st) => {
                        let root = stage_roots.get(st.as_str()).ok_or_else(|| {
                            WboxError::args(format!(
                                "COPY --from={} 引用了未定义的阶段（阶段要先用 \
                                 `FROM <镜像> AS {}` 声明，且必须在本条之前）",
                                st, st
                            ))
                        })?;
                        let p = resolve_rootfs_path(root, src)?;
                        if !p.exists() {
                            return Err(WboxError::args(format!(
                                "COPY --from={}：该阶段里没有 '{}'",
                                st, src
                            )));
                        }
                        p
                    }
                    None => resolve_context_path(&opts.context, src)?,
                };
                let to = resolve_rootfs_path(&rootfs, dst)?;
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| WboxError::args(format!("创建 COPY 目标目录失败：{}", e)))?;
                }
                if from.is_dir() {
                    copy_build_tree(&from, &to)?;
                } else {
                    #[cfg(windows)]
                    copy_file_contents(&from, &to)?;
                    #[cfg(not(windows))]
                    std::fs::copy(&from, &to)
                        .map_err(|e| WboxError::args(format!("COPY '{}' 失败：{}", src, e)))?;
                }
            }
            Instruction::Add { src, dst } => {
                println!("[{}/{}] ADD {} {}", step, instructions.len(), src, dst);
                let from = resolve_context_path(&opts.context, src)?;
                let to = resolve_rootfs_path(&rootfs, dst)?;
                if from.is_dir() {
                    // 目录：与 COPY 完全一致（docker 也不对目录做解包）
                    std::fs::create_dir_all(&to)
                        .map_err(|e| WboxError::args(format!("创建 ADD 目标目录失败：{}", e)))?;
                    copy_build_tree(&from, &to)?;
                } else if is_tar_archive(&from) {
                    // **ADD 与 COPY 的唯一区别**：本地 tar 解开到目标目录。
                    // 目标当成目录（docker 同此语义：ADD x.tar /dst 解到 /dst/）。
                    std::fs::create_dir_all(&to)
                        .map_err(|e| WboxError::args(format!("创建 ADD 目标目录失败：{}", e)))?;
                    let n = extract_tar_into(&from, &to)?;
                    println!("      （识别为 tar，已解开 {} 个条目）", n);
                } else {
                    if let Some(parent) = to.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            WboxError::args(format!("创建 ADD 目标目录失败：{}", e))
                        })?;
                    }
                    #[cfg(windows)]
                    copy_file_contents(&from, &to)?;
                    #[cfg(not(windows))]
                    std::fs::copy(&from, &to)
                        .map_err(|e| WboxError::args(format!("ADD '{}' 失败：{}", src, e)))?;
                }
            }
            Instruction::Env { key, value } => {
                cfg.env.retain(|(k, _)| k != key);
                cfg.env.push((key.clone(), value.clone()));
            }
            Instruction::Workdir(w) => cfg.workdir = Some(w.clone()),
            Instruction::Label { key, value } => {
                cfg.labels.retain(|(k, _)| k != key);
                cfg.labels.push((key.clone(), value.clone()));
            }
            Instruction::Expose(port) => {
                // 补默认协议，与 docker 一致：`EXPOSE 80` → `80/tcp`
                let norm = if port.contains('/') {
                    port.clone()
                } else {
                    format!("{}/tcp", port)
                };
                if !cfg.exposed.contains(&norm) {
                    cfg.exposed.push(norm);
                }
            }
            Instruction::User(u) => cfg.user = Some(u.clone()),
            Instruction::Arg { key, default } => {
                // 构建期变量：进 RUN 的环境，但不进镜像 config（见 to_json 的说明）
                cfg.args.retain(|(k, _)| k != key);
                cfg.args
                    .push((key.clone(), default.clone().unwrap_or_default()));
            }
            Instruction::Cmd(c) => cfg.cmd = Some(c.clone()),
            Instruction::Entrypoint(e) => cfg.entrypoint = Some(e.clone()),
            Instruction::Run(cmd) if linked_base => {
                println!("[{}/{}] RUN {}", step, instructions.len(), cmd);
                // staging 与基础镜像共享 inode，RUN 绝不能就地写：让它写进
                // 本步专属的 overlay upper，跑完再合并回来（只对改动过的文件
                // 断开硬链接）。合并失败要让整个 build 失败——半合并的 staging
                // 是错的内容，比构建失败糟得多。
                let layer = build_dir.join(format!(".wbox-step-{}", step));
                // 用 `fsutil::remove_tree` 而不是裸 `remove_dir_all`：这棵树里
                // 就有 overlayfs 建的 mode-000 `work/work`，裸删在非 root 下
                // 静默失败，`.wbox-step-*` 会留在镜像缓存旁边。
                crate::fsutil::remove_tree(&layer);
                run_step_with(&rootfs, cmd, &cfg, Some(&layer))?;
                #[cfg(not(windows))]
                merge_overlay_upper(&layer.join("upper"), &rootfs)?;
                crate::fsutil::remove_tree(&layer);
            }
            Instruction::Run(cmd) => {
                println!("[{}/{}] RUN {}", step, instructions.len(), cmd);
                run_step(&rootfs, cmd, &cfg)?;
            }
        }
        // 改动型步骤执行完就落一份快照，供下次构建复用
        if mutating(ins) && idx >= resume_from {
            let snap = cache.join(&keys[idx]);
            let _ = std::fs::remove_dir_all(&snap);
            if let Err(e) = copy_build_tree(&rootfs, &snap.join("rootfs")) {
                // 缓存写失败不该让构建失败——它只是加速手段。但要说出来，
                // 否则用户会困惑"为什么每次都不命中"。
                eprintln!("wbox: 构建缓存写入失败（不影响本次构建）：{}", e);
                let _ = std::fs::remove_dir_all(&snap);
            }
        }
    }

    #[cfg(windows)]
    {
        // staging 带构建 AppContainer SID 的临时修改 ACE，不能直接 rename 成最终
        // 镜像。重新创建目标并只复制内容/symlink，让其继承镜像缓存的干净 DACL。
        crate::fsutil::remove_tree(&out_dir);
        std::fs::create_dir_all(&out_dir)
            .map_err(|e| WboxError::args(format!("创建镜像目录失败：{}", e)))?;
        crate::backend::copy_rootfs_tree(&rootfs, &out_dir.join("rootfs"))?;
    }

    // 阶段临时目录只在构建期有用，留着会在镜像缓存旁边堆一堆半成品 rootfs
    for d in stage_dirs.iter() {
        // 阶段目录里可能嵌着 `.wbox-step-*` 的 overlay work 树，同样要补权限再删。
        crate::fsutil::remove_tree(d);
    }

    std::fs::write(out_dir.join("config.json"), cfg.to_json())
        .map_err(|e| WboxError::args(format!("写 config.json 失败：{}", e)))?;

    // 分层元数据（PRD L5b）：基础镜像留着原始压缩层时，把构建产物写成
    // **基础层 + 一个增量层**。这样 push 时基础层会被 registry 的 HEAD 判定
    // 已存在而跳过上传，只传增量。
    //
    // 做不到时（基础镜像是旧缓存 / 本身就是 build 产物 / 算增量失败）退回写
    // 空 manifest——那是诚实的：本地构建没有 registry 层信息，编一个假 digest
    // 只会误导，而 push 会据此走 flatten 路径（F9.13）。
    let layered = base_image_dir.as_ref().and_then(|b| {
        write_layered_manifest(b, &out_dir, &rootfs, &cfg)
            .ok()
            .flatten()
    });
    if layered.is_none() {
        std::fs::write(out_dir.join("manifest.json"), "{}")
            .map_err(|e| WboxError::args(format!("写 manifest.json 失败：{}", e)))?;
        std::fs::write(out_dir.join("layers.json"), "[]")
            .map_err(|e| WboxError::args(format!("写 layers.json 失败：{}", e)))?;
    }
    println!("构建完成：{}", opts.tag);
    Ok(0)
}

/// 把构建产物写成"基础层 + 增量层"的 manifest，并把基础层 blob 带过来。
///
/// 返回 `Ok(None)` 表示做不到（基础镜像没有原始层等），调用方退回空 manifest。
/// **失败一律不致命**：构建本身已经成功，分层只是让后续 push 更省，
/// 不该因为它出问题就让整个 build 失败。
fn write_layered_manifest(
    base_dir: &Path,
    out_dir: &Path,
    rootfs: &Path,
    cfg: &ConfigAccum,
) -> Result<Option<()>> {
    let Ok(base_manifest) = std::fs::read(base_dir.join("manifest.json")) else {
        return Ok(None);
    };
    let Ok(bm) = wbox_codec::json::from_slice(&base_manifest) else {
        return Ok(None);
    };
    let Some(base_layers) = bm.get("layers").and_then(|l| l.as_array()) else {
        return Ok(None);
    };
    if base_layers.is_empty() {
        return Ok(None);
    }
    // 基础层的原始 blob 必须都还在，否则 push 时无从上传
    for l in base_layers {
        let Some(d) = l.get("digest").and_then(|d| d.as_str()) else {
            return Ok(None);
        };
        if !crate::oci::blob_path(base_dir, d).is_file() {
            return Ok(None);
        }
    }

    let delta = crate::oci::push::diff_rootfs(&base_dir.join("rootfs"), rootfs)?;
    let blobs_dir = out_dir.join(crate::oci::BLOBS_DIR);
    std::fs::create_dir_all(&blobs_dir)
        .map_err(|e| WboxError::args(format!("创建 blobs 目录失败：{}", e)))?;
    // 基础层原样带过来：push 时要能上传它们（registry 多半已有，HEAD 会跳过，
    // 但本地必须备着，否则遇到空 registry 就推不上去）。
    let mut layers = Vec::new();
    for l in base_layers {
        let d = l.get("digest").and_then(|d| d.as_str()).unwrap_or_default();
        std::fs::copy(
            crate::oci::blob_path(base_dir, d),
            crate::oci::blob_path(out_dir, d),
        )
        .map_err(|e| WboxError::args(format!("复制基础层 {} 失败：{}", d, e)))?;
        layers.push(l.clone());
    }
    std::fs::write(
        crate::oci::blob_path(out_dir, &delta.digest),
        &delta.gzipped,
    )
    .map_err(|e| WboxError::args(format!("写增量层失败：{}", e)))?;
    layers.push(wbox_codec::json!({
        "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
        "digest": delta.digest,
        "size": delta.gzipped.len(),
    }));

    // config 要把增量层的 diff_id 追加进去，否则拉取方按 diff_ids 复原会对不上
    let base_diff_ids: Vec<wbox_codec::json::Value> = bm
        .get("config")
        .and_then(|_| {
            std::fs::read(base_dir.join("config.json"))
                .ok()
                .and_then(|b| wbox_codec::json::from_slice(&b).ok())
        })
        .and_then(|c| {
            c.get("rootfs")
                .and_then(|r| r.get("diff_ids"))
                .and_then(|d| d.as_array())
                .cloned()
        })
        .unwrap_or_default();
    let mut diff_ids = base_diff_ids;
    diff_ids.push(wbox_codec::json!(delta.diff_id));
    let mut config: wbox_codec::json::Value =
        wbox_codec::json::from_str(&cfg.to_json()).unwrap_or_else(|_| wbox_codec::json!({}));
    if let Some(o) = config.as_object_mut() {
        o.insert("architecture".into(), wbox_codec::json!("amd64"));
        o.insert("os".into(), wbox_codec::json!("linux"));
        o.insert(
            "rootfs".into(),
            wbox_codec::json!({"type": "layers", "diff_ids": diff_ids}),
        );
        o.insert("history".into(), wbox_codec::json!([]));
    }
    let config_bytes = wbox_codec::json::to_vec(&config);
    let config_digest = crate::oci::push::sha256_hex(&config_bytes);
    std::fs::write(out_dir.join("config.json"), &config_bytes)
        .map_err(|e| WboxError::args(format!("写 config.json 失败：{}", e)))?;

    let manifest = wbox_codec::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config_bytes.len(),
        },
        "layers": layers,
    });
    let manifest_bytes = wbox_codec::json::to_vec(&manifest);
    std::fs::write(out_dir.join("manifest.json"), &manifest_bytes)
        .map_err(|e| WboxError::args(format!("写 manifest.json 失败：{}", e)))?;
    let digests: Vec<String> = manifest["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["digest"].as_str().unwrap_or_default().to_string())
        .collect();
    std::fs::write(
        out_dir.join("layers.json"),
        wbox_codec::json::Value::from(&digests).to_string_pretty(),
    )
    .map_err(|e| WboxError::args(format!("写 layers.json 失败：{}", e)))?;
    println!(
        "wbox: 构建产物已分层：基础 {} 层 + 增量 1 层（push 时基础层可被跳过）",
        base_layers.len()
    );
    Ok(Some(()))
}

/// `wbox commit`：把容器的改动固化成一个新镜像（PRD F9.20）。
///
/// **整条链路都是复用**，没有一件新机制：
/// - 基础 rootfs 用 [`link_tree`] 硬链接铺开（F9.18），磁盘不翻倍；
/// - 容器的改动就在 overlay upper 里，用 [`merge_overlay_upper`] 合并进去
///   （F9.18 的同一份合并逻辑，whiteout/opaque 一并处理）；
/// - 元数据用 [`write_layered_manifest`] 写成"基础层 + 增量层"（F9.17），
///   于是 commit 出来的镜像 push 时基础层同样会被 `HEAD` 跳过。
///
/// 换句话说，这一格能成立是因为前面几格把机制建对了；这里只做编排。
#[cfg(not(windows))]
pub(crate) fn commit_container(container: &str, tag: &str) -> Result<u32> {
    // 分层解析与"没有 overlay 层"的措辞都在 layers 模块。静默 commit 一份与镜像
    // 完全相同的副本比报错糟得多——用户会以为改动固化了。
    let layers = crate::layers::ContainerLayers::resolve(container, "无法 commit")?;
    let base_dir = layers.image_dir().ok_or_else(|| {
        WboxError::args(format!(
            "容器 '{}' 未记录镜像路径或路径异常，无法确定基础镜像",
            container
        ))
    })?;

    let iref = crate::oci::ImageRef::parse(tag, None)?;
    let out_dir = crate::oci::image_dir(&iref)?;
    if out_dir.starts_with(base_dir) || base_dir.starts_with(&out_dir) {
        // 覆盖基础镜像会在铺硬链接的中途把 lower 抽掉，结果不可预测
        return Err(WboxError::args(format!(
            "commit 目标 '{}' 与容器的基础镜像是同一份，拒绝原地覆盖",
            tag
        )));
    }
    let rootfs = out_dir.join("rootfs");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| WboxError::args(format!("创建镜像目录失败：{}", e)))?;

    println!("wbox: commit {} → {}", container, iref.qualified_ref());
    // 铺下层（硬链接）+ 合并 upper + 去掉换根暂存目录，三件事在 layers 里，
    // 与 `wbox export` 共用同一份实现。
    layers.materialize(&rootfs)?;

    // 继承基础镜像的运行期配置：commit 出来的镜像应当能像原镜像一样跑起来
    let mut cfg = ConfigAccum::default();
    if let Some(base_cfg) = crate::oci::config::ImageConfig::load(base_dir)? {
        cfg.env = base_cfg.env.clone();
        cfg.workdir = base_cfg.working_dir.clone();
        if !base_cfg.cmd.is_empty() {
            cfg.cmd = Some(base_cfg.cmd.clone());
        }
        if !base_cfg.entrypoint.is_empty() {
            cfg.entrypoint = Some(base_cfg.entrypoint.clone());
        }
    }
    std::fs::write(out_dir.join("config.json"), cfg.to_json())
        .map_err(|e| WboxError::args(format!("写 config.json 失败：{}", e)))?;
    if write_layered_manifest(base_dir, &out_dir, &rootfs, &cfg)?.is_none() {
        std::fs::write(out_dir.join("manifest.json"), "{}")
            .map_err(|e| WboxError::args(format!("写 manifest.json 失败：{}", e)))?;
        std::fs::write(out_dir.join("layers.json"), "[]")
            .map_err(|e| WboxError::args(format!("写 layers.json 失败：{}", e)))?;
    }
    println!("commit 完成：{}", iref.qualified_ref());
    Ok(0)
}

/// 执行一条 `RUN`。
///
/// **直接复用运行期的容器路径**（同一个 backend、同一套 namespace 与限额），
/// 所以构建期与运行期的隔离强度一致，不存在"构建时能做、运行时不能"的错位。
fn run_step(rootfs: &Path, cmd: &str, cfg: &ConfigAccum) -> Result<()> {
    run_step_with(rootfs, cmd, cfg, None)
}

/// `layer_dir` 为 `Some` 时走 overlay：RUN 的写入落在该目录的 `upper/` 里，
/// 由调用方在步骤结束后合并回 staging（PRD L5b）。这样 staging 里与基础镜像
/// 共享 inode 的硬链接**不会被就地改写**，基础镜像缓存才是安全的。
fn run_step_with(
    rootfs: &Path,
    cmd: &str,
    cfg: &ConfigAccum,
    layer_dir: Option<&Path>,
) -> Result<()> {
    use crate::backend::{Backend, RunSpec};
    let spec = RunSpec {
        name: format!("wbox-build-{}", std::process::id()),
        allow_network: true, // RUN 常要装包；与 docker build 默认一致
        // 不走 overlay 时，RUN 的写入必须直落 staging rootfs——那就是构建产物。
        direct_rootfs_writes: layer_dir.is_none(),
        overlay_layer_dir: layer_dir.map(|p| p.to_path_buf()),
        workdir: rootfs.to_path_buf(),
        cmd: vec!["/bin/sh".to_string(), "-c".to_string(), cmd.to_string()],
        env: cfg.env.clone(),
        ..RunSpec::default()
    };
    #[cfg(windows)]
    crate::acl::grant_modify_recursive_for_profile(rootfs, &spec.name)?;
    let backend = build_backend();
    let prepared = backend.prepare(&spec)?;
    #[cfg(windows)]
    let registration = crate::runstate::register_with_context(
        &spec.name,
        &spec.cmd,
        "(build-step)",
        false,
        None,
        None,
    )?;
    let rc = backend.spawn(&spec, &prepared)?;
    #[cfg(windows)]
    drop(registration);
    if rc != 0 {
        return Err(WboxError::args(format!(
            "RUN 失败（退出码 {}）：{}",
            rc, cmd
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn build_backend() -> crate::backend::LinuxNativeBackend {
    crate::backend::LinuxNativeBackend(crate::backend::LinuxMode::Image)
}

#[cfg(not(target_os = "linux"))]
fn build_backend() -> crate::backend::EmuBackend {
    crate::backend::EmuBackend
}
