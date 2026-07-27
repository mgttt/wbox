//! `wbox build`：Dockerfile 子集构建（`PRD.md` F9.3）。
//!
//! 只实现自用场景够用的子集：`FROM` / `RUN` / `COPY` / `ENV` / `WORKDIR` /
//! `CMD` / `ENTRYPOINT` / `LABEL` / `EXPOSE` / `USER` / `ARG`。
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
    From(String),
    Run(String),
    /// `COPY <src> <dst>`：src 相对构建上下文，dst 是容器内绝对路径
    Copy { src: String, dst: String },
    Env { key: String, value: String },
    Workdir(String),
    Cmd(Vec<String>),
    Entrypoint(Vec<String>),
    /// `LABEL k=v`：写进镜像 config 的 `Labels`。
    Label { key: String, value: String },
    /// `EXPOSE 80` / `EXPOSE 80/tcp`：**纯声明**，不会真的发布端口。
    Expose(String),
    /// `USER 1000[:1000]`：镜像声明的默认身份。
    User(String),
    /// `ARG k[=默认值]`：构建期变量。
    Arg { key: String, default: Option<String> },
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
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line.as_str(), ""));
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
                out.push(Instruction::From(rest.to_string()));
            }
            "RUN" => {
                need("命令")?;
                out.push(Instruction::Run(rest.to_string()));
            }
            "COPY" => {
                let mut it = rest.split_whitespace();
                let (Some(src), Some(dst)) = (it.next(), it.next()) else {
                    return Err(WboxError::args("COPY 需要 <src> <dst> 两个参数"));
                };
                if it.next().is_some() {
                    return Err(WboxError::args("COPY 暂只支持单个 src（多源未实现）"));
                }
                out.push(Instruction::Copy {
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
                     CMD/ENTRYPOINT/LABEL/EXPOSE/USER/ARG）；\
                     静默跳过会产出一个看似构建成功、实则少做了事的镜像",
                    other
                )));
            }
        }
    }
    match out.first() {
        Some(Instruction::From(_)) => Ok(out),
        Some(_) => Err(WboxError::args("Dockerfile 的第一条指令必须是 FROM")),
        None => Err(WboxError::args("Dockerfile 为空")),
    }
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
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(t) {
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
    let canon = joined.canonicalize().map_err(|e| {
        WboxError::args(format!("COPY 源 '{}' 不可用：{}", src, e))
    })?;
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
/// 两处在用：build 的 `COPY` 目标，以及 `wbox cp` 的容器端路径。两边面对的
/// 是同一类输入（用户给的容器内路径），所以共用同一份逃逸校验——分头写两份
/// 迟早会有一份漏掉 `..`。
pub fn resolve_rootfs_path(rootfs: &Path, dst: &str) -> Result<PathBuf> {
    if !dst.starts_with('/') {
        return Err(WboxError::args(format!(
            "容器内路径 '{}' 必须以 / 开头（Dockerfile 的 COPY 目标同此要求）",
            dst
        )));
    }
    // 逐段消解 `..`，不依赖 canonicalize——目标通常还不存在
    let mut out = PathBuf::from(rootfs);
    let mut depth = 0usize;
    for comp in Path::new(dst).components() {
        match comp {
            std::path::Component::Normal(c) => {
                out.push(c);
                depth += 1;
            }
            std::path::Component::ParentDir => {
                if depth == 0 {
                    return Err(WboxError::args(format!(
                        "容器内路径 '{}' 用 '..' 逃出了 rootfs",
                        dst
                    )));
                }
                out.pop();
                depth -= 1;
            }
            _ => {}
        }
    }
    Ok(out)
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
            Instruction::Arg { key: "BUILD_ID".into(), default: Some("42".into()) }
        );
        assert_eq!(
            got[6],
            Instruction::Arg { key: "NO_DEFAULT".into(), default: None }
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
        let v: serde_json::Value = serde_json::from_str(&cfg.to_json()).unwrap();
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
        assert_eq!(got[0], Instruction::From("alpine:3.20".into()));
        assert_eq!(
            got[1],
            Instruction::Env { key: "FOO".into(), value: "bar".into() }
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
            Instruction::Cmd(vec!["/bin/sh".into(), "-c".into(), "echo a && echo b".into()])
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
        assert!(parse_dockerfile("FROM x\nCOPY only-one").is_err(), "COPY 缺参数");
        assert!(parse_dockerfile("FROM x\nENV noequals").is_err(), "ENV 需 K=V");
        assert!(parse_dockerfile("FROM x\nRUN cmd \\").is_err(), "续行未收尾");
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
        assert!(resolve_rootfs_path(root, "/../escape").is_err(), "越界的 .. 要拒绝");
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
        let mut cfg = serde_json::Map::new();
        cfg.insert("Env".into(), serde_json::json!(env));
        if let Some(w) = &self.workdir {
            cfg.insert("WorkingDir".into(), serde_json::json!(w));
        }
        if let Some(c) = &self.cmd {
            cfg.insert("Cmd".into(), serde_json::json!(c));
        }
        if let Some(e) = &self.entrypoint {
            cfg.insert("Entrypoint".into(), serde_json::json!(e));
        }
        if !self.labels.is_empty() {
            let map: serde_json::Map<String, serde_json::Value> = self
                .labels
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                .collect();
            cfg.insert("Labels".into(), serde_json::Value::Object(map));
        }
        if !self.exposed.is_empty() {
            // OCI/docker 的形状是 {"80/tcp": {}}，值是个空对象
            let map: serde_json::Map<String, serde_json::Value> = self
                .exposed
                .iter()
                .map(|p| (p.clone(), serde_json::json!({})))
                .collect();
            cfg.insert("ExposedPorts".into(), serde_json::Value::Object(map));
        }
        if let Some(u) = &self.user {
            cfg.insert("User".into(), serde_json::json!(u));
        }
        // ARG 刻意不写进 config：构建参数常带凭证（token、密码），
        // 落进镜像等于随镜像一起发出去。docker 也不把 ARG 写进 config。
        serde_json::json!({ "config": cfg }).to_string()
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
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::FileTypeExt;
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
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(format!("{:?}", ins).as_bytes());
    if let Instruction::Copy { src, .. } = ins {
        // 源内容变了就必须失效。目录则按"路径+内容"逐个文件累加。
        let from = resolve_context_path(context, src)?;
        hash_path_into(&from, &mut h)?;
    }
    Ok(format!("{:x}", h.finalize()))
}

fn hash_path_into(p: &Path, h: &mut sha2::Sha256) -> Result<()> {
    use sha2::Digest;
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
    let mut output = std::fs::File::create(dst).map_err(|e| {
        WboxError::args(format!("创建 COPY 目标 '{}' 失败：{}", dst.display(), e))
    })?;
    std::io::copy(&mut input, &mut output)
        .map_err(|e| WboxError::args(format!("COPY '{}' 失败：{}", src.display(), e)))?;
    Ok(())
}

fn copy_build_tree(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            WboxError::args(format!("创建复制目标父目录 '{}' 失败：{}", parent.display(), e))
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
    let rootfs = build_dir.join("rootfs");

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
        matches!(i, Instruction::Run(_) | Instruction::Copy { .. })
    };
    let mut resume_from = 0usize;
    for idx in (0..instructions.len()).rev() {
        if mutating(&instructions[idx]) && cache.join(&keys[idx]).join("rootfs").is_dir() {
            resume_from = idx + 1;
            break;
        }
    }

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
            Instruction::From(base) => {
                let base_ref = crate::oci::ImageRef::parse(base, None)?;
                let base_dir = crate::oci::image_dir(&base_ref)?;
                if !base_dir.join("rootfs").is_dir() {
                    return Err(WboxError::args(format!(
                        "基础镜像 '{}' 未 pull（先 `wbox image pull {}`）",
                        base, base
                    )));
                }
                println!("[{}/{}] FROM {}", step, instructions.len(), base);
                // 重建输出目录：残留的上一次构建会让结果不可复现
                let _ = std::fs::remove_dir_all(&build_dir);
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
            Instruction::Copy { src, dst } => {
                println!("[{}/{}] COPY {} {}", step, instructions.len(), src, dst);
                let from = resolve_context_path(&opts.context, src)?;
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
                let _ = std::fs::remove_dir_all(&layer);
                run_step_with(&rootfs, cmd, &cfg, Some(&layer))?;
                #[cfg(not(windows))]
                merge_overlay_upper(&layer.join("upper"), &rootfs)?;
                let _ = std::fs::remove_dir_all(&layer);
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
        let _ = std::fs::remove_dir_all(&out_dir);
        std::fs::create_dir_all(&out_dir)
            .map_err(|e| WboxError::args(format!("创建镜像目录失败：{}", e)))?;
        crate::backend::copy_rootfs_tree(&rootfs, &out_dir.join("rootfs"))?;
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
    let layered = base_image_dir
        .as_ref()
        .and_then(|b| write_layered_manifest(b, &out_dir, &rootfs, &cfg).ok().flatten());
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
    let Ok(bm) = serde_json::from_slice::<serde_json::Value>(&base_manifest) else {
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
    std::fs::write(crate::oci::blob_path(out_dir, &delta.digest), &delta.gzipped)
        .map_err(|e| WboxError::args(format!("写增量层失败：{}", e)))?;
    layers.push(serde_json::json!({
        "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
        "digest": delta.digest,
        "size": delta.gzipped.len(),
    }));

    // config 要把增量层的 diff_id 追加进去，否则拉取方按 diff_ids 复原会对不上
    let base_diff_ids: Vec<serde_json::Value> = bm
        .get("config")
        .and_then(|_| {
            std::fs::read(base_dir.join("config.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        })
        .and_then(|c| {
            c.get("rootfs")
                .and_then(|r| r.get("diff_ids"))
                .and_then(|d| d.as_array())
                .cloned()
        })
        .unwrap_or_default();
    let mut diff_ids = base_diff_ids;
    diff_ids.push(serde_json::json!(delta.diff_id));
    let mut config: serde_json::Value = serde_json::from_str(&cfg.to_json())
        .unwrap_or_else(|_| serde_json::json!({}));
    if let Some(o) = config.as_object_mut() {
        o.insert("architecture".into(), serde_json::json!("amd64"));
        o.insert("os".into(), serde_json::json!("linux"));
        o.insert(
            "rootfs".into(),
            serde_json::json!({"type": "layers", "diff_ids": diff_ids}),
        );
        o.insert("history".into(), serde_json::json!([]));
    }
    let config_bytes = serde_json::to_vec(&config)
        .map_err(|e| WboxError::args(format!("序列化 config 失败：{}", e)))?;
    let config_digest = crate::oci::push::sha256_hex(&config_bytes);
    std::fs::write(out_dir.join("config.json"), &config_bytes)
        .map_err(|e| WboxError::args(format!("写 config.json 失败：{}", e)))?;

    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config_bytes.len(),
        },
        "layers": layers,
    });
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|e| WboxError::args(format!("序列化 manifest 失败：{}", e)))?;
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
        serde_json::to_string_pretty(&digests).unwrap_or_else(|_| "[]".into()),
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
fn build_backend() -> crate::backend::BlinkBackend {
    crate::backend::BlinkBackend
}
