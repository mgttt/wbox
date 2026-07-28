//! Linux 宿主上跑 Windows 程序（`PRD.md` F6 / `docs/architecture.md` §3.4）。
//!
//! **定位是"执行器变体"，不是新后端。** 隔离仍然全部由
//! [`super::linux::LinuxNativeBackend`] 提供（user/mount/pid/net namespace、
//! cgroup 限额、`PR_SET_PDEATHSIG` 收割），本模块只回答一个问题：
//! *这条命令要不要在前面插一个 wine 加载器*。
//!
//! 这样做的直接好处是**语义自动对齐**——`--memory`/`--max-procs`/
//! `--allow-network` 对 Windows 程序与对 Linux 程序是同一套实现，不会出现
//! "wine 那条路忘了限额"这类分叉。PRD F5/F6 的一致性要求因此不需要额外维护。
//!
//! # 范围（刻意收窄）
//!
//! 只做 CLI/TUI，不碰 GUI/DirectX/COM——范围见 PRD F6：那三块正是 wine 三十年
//! 工程量的主体。wbox **集成 wine 而不是取代它**：wine 负责 Win32 语义，
//! wbox 负责隔离与资源管控。

use crate::error::{Result, WboxError};
use std::path::{Path, PathBuf};

/// 判断一个文件是不是 PE（Windows 可执行）。
///
/// 判据就是 PE 格式本身：`MZ` 魔数 → 偏移 0x3C 处的 `e_lfanew` → 该处是
/// `PE\0\0`。只看魔数不够——`MZ` 开头的 DOS 程序、甚至某些文本文件都可能撞上，
/// 而 wbox 会据此决定要不要塞一个 wine 进去，误判的代价是"用 wine 去跑一个
/// Linux 程序"，报错会非常难懂。
///
/// 读不到或格式不符一律返回 false：那就当普通 ELF 走原来的路，由内核自己
/// 去报"Exec format error"，比我们猜错强。
pub fn is_pe(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut mz = [0u8; 2];
    if f.read_exact(&mut mz).is_err() || &mz != b"MZ" {
        return false;
    }
    if f.seek(SeekFrom::Start(0x3C)).is_err() {
        return false;
    }
    let mut off = [0u8; 4];
    if f.read_exact(&mut off).is_err() {
        return false;
    }
    let e_lfanew = u32::from_le_bytes(off) as u64;
    // 明显越界的偏移直接否掉，避免对超大值做无谓 seek
    if !(0x40..=0x1000_0000).contains(&e_lfanew) {
        return false;
    }
    if f.seek(SeekFrom::Start(e_lfanew)).is_err() {
        return false;
    }
    let mut sig = [0u8; 4];
    f.read_exact(&mut sig).is_ok() && &sig == b"PE\0\0"
}

/// wine 加载器的候选位置，按优先级。
///
/// `wine64` 与 `wine` 都列上：Debian/Ubuntu 打包成
/// `/usr/lib/wine/wine64`（不在 PATH 上，本仓开发容器正是如此），
/// 而多数发行版把 `wine` 放进 PATH。
const WINE_CANDIDATES: &[&str] = &[
    "/usr/lib/wine/wine64",
    "/usr/lib/wine/wine",
    "/usr/lib64/wine/wine64",
    "/opt/wine-stable/bin/wine",
];

/// 定位 wine 加载器。
///
/// 顺序：`WBOX_WINE` 显式指定 → `PATH` 上的 `wine`/`wine64` → 常见安装位置。
/// 找不到就**明确报错并说清怎么装**，不静默降级成"直接 exec 这个 PE"——
/// 后者会得到内核的 `Exec format error`，用户根本看不出是缺 wine。
pub fn find_wine(explicit: Option<&str>, path_env: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let p = PathBuf::from(p);
        if is_executable(&p) {
            return Ok(p);
        }
        return Err(WboxError::spawn(format!(
            "WBOX_WINE 指向的 '{}' 不是可执行文件",
            p.display()
        )));
    }
    if let Some(paths) = path_env {
        for dir in paths.split(':').filter(|d| !d.is_empty()) {
            for name in ["wine64", "wine"] {
                let c = Path::new(dir).join(name);
                if is_executable(&c) {
                    return Ok(c);
                }
            }
        }
    }
    for c in WINE_CANDIDATES {
        let c = PathBuf::from(c);
        if is_executable(&c) {
            return Ok(c);
        }
    }
    Err(WboxError::spawn(
        "目标是 Windows 程序（PE），但本机找不到 wine。\
         wbox 在 Linux 上**集成 wine** 来跑 Windows 程序，不自带实现。\
         请先安装（Debian/Ubuntu: `sudo apt install wine64`），\
         或用 WBOX_WINE=/path/to/wine 指定",
    ))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// 取 wine 版本号（`wine --version` 的首行，如 `wine-9.0`）。
///
/// 只在 `-V` 时调用：wine 版本差异是 Windows 程序行为差异的头号来源，
/// 而那类差异**不属 wbox 缺陷**（docs/architecture.md §3.4）。把版本打出来，
/// 用户报问题时一眼能看出该找谁。
///
/// 取不到不致命（返回 `None`）：某些打包把 `--version` 交给 wineserver，
/// 或需要先初始化 prefix。为此不该让整条命令失败。
pub fn version(wine: &Path) -> Option<String> {
    let out = std::process::Command::new(wine)
        .arg("--version")
        .env("WINEDEBUG", "-all")
        .output()
        .ok()?;
    let text = if out.stdout.is_empty() {
        out.stderr
    } else {
        out.stdout
    };
    let s = String::from_utf8_lossy(&text);
    let first = s.lines().next()?.trim();
    (!first.is_empty()).then(|| first.to_string())
}

/// wine 需要一个**可写**的 prefix（首次运行会在里面铺一整套假 C: 盘）。
///
/// # 为什么按容器分，而不是共用一个
///
/// 曾经所有容器共用 `~/.wbox/wineprefix`。它与**宿主**是隔离的（不碰用户
/// 自己的 `~/.wine`），但**容器之间不隔离**：两个容器先后跑 Windows 程序，
/// 会互相看到对方对注册表、C 盘布局、已装组件的改动。wbox 的卖点就是隔离，
/// 这条与默认约束（PRD §2.2）直接冲突。
///
/// 现在每个容器一个 prefix，且**放在该容器的状态目录内**
/// （`~/.wbox/run/<name>/wineprefix`）。选这个位置不是随意的：容器记录被
/// `rm`（或前台容器退出）时，`runstate::purge_dir` 会整棵删掉状态目录，
/// prefix 自然跟着走——**不需要新增一条清理路径**，也就不会出现"清理逻辑
/// 漏了某种产物"的老问题。
///
/// 代价要说清：新 prefix 首次运行时 wine 要 bootstrap（铺假 C: 盘），有秒级
/// 开销。想跨运行复用就给容器起同一个 `--name`，或用 `WINEPREFIX` 显式指定。
pub fn container_prefix(container: &str) -> Result<PathBuf> {
    Ok(crate::runstate::dir_for(container)?.join("wineprefix"))
}

/// 确保 prefix 目录存在（wine 首次运行会在里面铺假 C: 盘，目录不存在会失败）。
///
/// `WINEPREFIX` 已在环境里就尊重用户的选择——显式指定优先于 wbox 的默认值，
/// 这也是需要跨容器共享 prefix 时的出口。
pub fn prepare_prefix(container: &str) -> Result<PathBuf> {
    let p = match std::env::var_os("WINEPREFIX") {
        Some(v) => PathBuf::from(v),
        None => container_prefix(container)?,
    };
    std::fs::create_dir_all(&p)
        .map_err(|e| WboxError::spawn(format!("创建 WINEPREFIX '{}' 失败：{}", p.display(), e)))?;
    Ok(p)
}

/// 给 guest 环境补上 wine 必需的变量。**不覆盖用户已显式给出的同名变量**
/// （`--env` 的优先级高于 wbox 的默认值）。
///
/// - `WINEPREFIX`：见 [`default_prefix`]；
/// - `HOME`：wine 内部仍会用到（临时文件等），指到 prefix 里，
///   避免既泄漏宿主 HOME 又让 wine 无处落脚；
/// - `WINEDEBUG=-all`：wine 默认往 stderr 打一堆 fixme/err，会淹掉 guest 的
///   真实输出。`-V` 时不加，方便排查 wine 自身的问题。
pub fn augment_env(env: &mut Vec<(String, String)>, prefix: &Path, verbose: bool) {
    let has = |env: &Vec<(String, String)>, k: &str| env.iter().any(|(n, _)| n == k);
    if !has(env, "WINEPREFIX") {
        env.push(("WINEPREFIX".to_string(), prefix.display().to_string()));
    }
    if !has(env, "HOME") {
        env.push(("HOME".to_string(), prefix.display().to_string()));
    }
    if !verbose && !has(env, "WINEDEBUG") {
        env.push(("WINEDEBUG".to_string(), "-all".to_string()));
    }
}

/// 若 `cmd[0]` 是 PE，就地把 wine 加载器插到最前面并补齐环境；否则什么都不做。
///
/// 返回 `-V` 该打印的若干行（键值对）。把"要不要用 wine + 怎么用 + 打印什么"
/// 收成**一次调用**，是为了让调用方（`linux.rs`）不必关心 wine 的存在：
/// 非 Linux 宿主上有一个同名空实现（见 `mod.rs` 的 cfg 选择），调用方因此
/// 一个 `cfg` 都不用写。此前那段内联 cfg 逼得三个绑定都挂 `#[allow(unused_mut)]`
/// ——属性本身就是"抽象漏了"的信号。
pub fn wrap_if_pe(
    cmd: &mut Vec<String>,
    env: &mut Vec<(String, String)>,
    verbose: bool,
    container: &str,
) -> Result<Option<Vec<(&'static str, String)>>> {
    if cmd.is_empty() || !is_pe(Path::new(&cmd[0])) {
        return Ok(None);
    }
    let loader = find_wine(
        std::env::var("WBOX_WINE").ok().as_deref(),
        std::env::var("PATH").ok().as_deref(),
    )?;
    let prefix = prepare_prefix(container)?;
    augment_env(env, &prefix, verbose);
    cmd.insert(0, loader.display().to_string());

    // 版本要打出来：wine 版本差异是 Windows 程序行为差异的头号来源，而那类
    // 差异不属 wbox 缺陷（docs/architecture.md §3.4）。只在 verbose 时取，
    // 免得每次运行都多 fork 一个 wine。
    let mut lines = Vec::new();
    if verbose {
        let ver = version(&loader).unwrap_or_else(|| "版本未知".to_string());
        lines.push((
            "执行器",
            format!("wine（目标是 PE）：{}［{}］", loader.display(), ver),
        ));
        lines.push(("WINEPREFIX", prefix.display().to_string()));
    }
    Ok(Some(lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wbox-wine-{}-{}", std::process::id(), tag));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// PE 判定必须看到 `PE\0\0` 才算数，只有 `MZ` 不够。
    #[test]
    fn is_pe_requires_full_signature() {
        let d = tmp("pe");
        // 真 PE 的最小骨架：MZ + e_lfanew=0x40 + PE\0\0
        let mut pe = vec![0u8; 0x44];
        pe[0] = b'M';
        pe[1] = b'Z';
        pe[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        pe[0x40..0x44].copy_from_slice(b"PE\0\0");
        let p = d.join("real.exe");
        std::fs::write(&p, &pe).unwrap();
        assert!(is_pe(&p), "合法 PE 骨架应被认出");

        // 只有 MZ（老式 DOS 程序）：不是 PE
        let mut dos = pe.clone();
        dos[0x40..0x44].copy_from_slice(b"\0\0\0\0");
        let p2 = d.join("dos.exe");
        std::fs::write(&p2, &dos).unwrap();
        assert!(!is_pe(&p2), "只有 MZ 没有 PE 签名，不该判为 PE");

        // ELF：绝不能误判（否则会拿 wine 去跑 Linux 程序）
        let p3 = d.join("elf");
        std::fs::write(&p3, b"\x7fELF\x02\x01\x01\x00rest").unwrap();
        assert!(!is_pe(&p3), "ELF 不能被判为 PE");

        // 不存在的文件、空文件
        assert!(!is_pe(&d.join("nope")));
        let p4 = d.join("empty");
        std::fs::write(&p4, b"").unwrap();
        assert!(!is_pe(&p4));

        // e_lfanew 越界：不能 panic，也不能判为 PE
        let mut bad = pe.clone();
        bad[0x3C..0x40].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let p5 = d.join("bad-offset.exe");
        std::fs::write(&p5, &bad).unwrap();
        assert!(!is_pe(&p5), "越界 e_lfanew 应安全否掉");

        let _ = std::fs::remove_dir_all(&d);
    }

    /// 找不到 wine 时必须明确报错并告诉用户怎么装，不能静默降级。
    #[test]
    fn find_wine_errors_with_actionable_message() {
        let e = find_wine(None, Some("/nonexistent-dir-a:/nonexistent-dir-b"));
        // 本机若真装了 wine（开发容器就装了），这里会 Ok——那也说明查找可用
        match e {
            Ok(p) => assert!(is_executable(&p), "返回的 wine 必须可执行：{}", p.display()),
            Err(err) => {
                let m = format!("{}", err);
                assert!(m.contains("wine"), "{}", m);
                assert!(
                    m.contains("WBOX_WINE") || m.contains("apt install"),
                    "要给出解法：{}",
                    m
                );
            }
        }
    }

    /// `WBOX_WINE` 指错时必须报错，而不是悄悄回退去搜 PATH——
    /// 用户显式指定了就说明他想用那一个。
    #[test]
    fn explicit_wine_must_exist() {
        let err = find_wine(Some("/definitely/not/here/wine"), None).unwrap_err();
        assert!(format!("{}", err).contains("WBOX_WINE"));
    }

    /// PATH 上的 wine 优先于内置候选路径。
    #[test]
    fn path_wine_takes_priority() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("path");
        let fake = d.join("wine64");
        std::fs::write(&fake, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let got = find_wine(None, Some(&d.display().to_string())).unwrap();
        assert_eq!(got, fake);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 用户已给的变量不被覆盖；`-V` 时不塞 WINEDEBUG（否则没法排查 wine）。
    #[test]
    fn augment_env_respects_user_values() {
        let prefix = Path::new("/tmp/wp");
        let mut env = vec![("WINEPREFIX".to_string(), "/mine".to_string())];
        augment_env(&mut env, prefix, false);
        assert_eq!(
            env.iter().filter(|(k, _)| k == "WINEPREFIX").count(),
            1,
            "不应重复塞入"
        );
        assert_eq!(
            env.iter().find(|(k, _)| k == "WINEPREFIX").unwrap().1,
            "/mine"
        );
        assert!(env.iter().any(|(k, v)| k == "HOME" && v == "/tmp/wp"));
        assert!(env.iter().any(|(k, v)| k == "WINEDEBUG" && v == "-all"));

        let mut verbose_env = Vec::new();
        augment_env(&mut verbose_env, prefix, true);
        assert!(
            !verbose_env.iter().any(|(k, _)| k == "WINEDEBUG"),
            "-V 时不该压掉 wine 的诊断输出"
        );
    }
}

#[cfg(test)]
mod prefix_tests {
    use super::*;
    use crate::testenv::TempHome;

    /// 核心属性：**不同容器拿到不同 prefix**。共用一个的话，两个容器会互相
    /// 看到对方对注册表/C 盘的改动——这正是 L2 要消除的问题。
    #[test]
    fn each_container_gets_its_own_prefix() {
        let _home = TempHome::new("wine-iso");
        let a = container_prefix("alpha").unwrap();
        let b = container_prefix("beta").unwrap();
        assert_ne!(a, b, "两个容器不能共用 prefix");
        assert!(a.ends_with("wineprefix"));
    }

    /// prefix 必须落在**该容器的状态目录内**——清理才能搭 purge_dir 的车，
    /// 不必新增一条清理路径。这条断言就是在钉住那个设计决定。
    #[test]
    fn prefix_lives_inside_container_state_dir() {
        let _home = TempHome::new("wine-loc");
        let dir = crate::runstate::dir_for("c1").unwrap();
        assert_eq!(container_prefix("c1").unwrap(), dir.join("wineprefix"));
    }

    /// 容器记录被清理时，prefix 应当跟着消失（不留垃圾、也不泄漏到下个同名容器）。
    #[test]
    fn prefix_is_removed_with_the_container_record() {
        let _home = TempHome::new("wine-clean");
        let p = prepare_prefix("gone").unwrap();
        std::fs::write(p.join("marker"), b"x").unwrap();
        assert!(p.exists());
        // 造一条"已退出"的记录再 rm，走的是与真实清理相同的路径
        let dir = crate::runstate::dir_for("gone").unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"gone","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}"#,
        )
        .unwrap();
        crate::runstate::remove("gone").unwrap();
        assert!(!p.exists(), "容器记录删了，wineprefix 也该没了");
    }

    /// 用户显式给的 `WINEPREFIX` 优先——需要跨容器共享时的出口。
    #[test]
    fn explicit_env_prefix_wins() {
        let mut home = TempHome::new("wine-env");
        let custom = home.dir.join("mine");
        home.env().set("WINEPREFIX", &custom);
        assert_eq!(prepare_prefix("whatever").unwrap(), custom);
    }
}
