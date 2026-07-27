//! 程序装载：把一个 guest 路径变成"可以跳过去执行"的地址空间。
//!
//! 这一层是 `main.rs`（进程启动）与 `syscall::execve`（进程替换）的公共部分。
//! 单独拆出来的理由是 `execve` 有一个硬要求：**失败时原地址空间必须完好**。
//! 所以调用方总是先装到一个新的 `Mem` 里，成功了才换过去；这里只负责
//! "装到给定的 `Mem`"，不碰调用方的当前状态。
//!
//! `#!` 脚本按内核的 `binfmt_script` 处理：取解释器 + 最多一个参数，把原
//! 路径插到 argv[1]，然后递归装解释器。

use crate::elf::{self, Loaded};
use crate::mem::Mem;
use crate::stack;
use crate::syscall::fs::Vfs;

/// `#!` 的递归上限。Linux 的 `BINPRM_MAX_RECURSION` 是 4，照抄。
const MAX_SCRIPT_RECURSION: u32 = 4;

/// 装载失败。既带 `-errno`（`execve` 要回给 guest）也带人话（CLI 要打印）。
#[derive(Debug)]
pub struct LoadFail {
    pub errno: i64,
    pub msg: String,
}

impl std::fmt::Display for LoadFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)
    }
}

/// 装载结果：跳转所需的三个量。
pub struct Program {
    pub loaded: Loaded,
    /// 初始栈指针（已按 16 字节对齐）。
    pub rsp: u64,
}

/// 把 `prog`（**guest 视角**的路径）装进 `mem`，并铺好初始栈。
///
/// `argv[0]` 由调用方给定，不强制等于 `prog`——`execve` 允许两者不同，
/// busybox 的 applet 分派正是靠这个。
pub fn load_into(
    mem: &mut Mem,
    vfs: &Vfs,
    prog: &str,
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
) -> Result<Program, LoadFail> {
    load_depth(mem, vfs, prog, argv, envp, 0)
}

fn load_depth(
    mem: &mut Mem,
    vfs: &Vfs,
    prog: &str,
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
    depth: u32,
) -> Result<Program, LoadFail> {
    if depth >= MAX_SCRIPT_RECURSION {
        return Err(LoadFail {
            errno: -crate::syscall::ELOOP,
            msg: format!("'{prog}' 的 #! 解释器链太深"),
        });
    }

    // 程序本身也要走 VFS 与越界检查：容器里的 execve 不能拿绝对宿主路径
    // 把 rootfs 之外的东西跑起来。
    let host = vfs.host_path_confined(prog).ok_or_else(|| LoadFail {
        errno: -crate::syscall::EACCES,
        msg: format!("'{prog}' 越出了 rootfs"),
    })?;
    let image = std::fs::read(&host).map_err(|e| LoadFail {
        errno: crate::syscall::host_err(&e),
        msg: format!("读取 '{}'（guest '{}'）失败：{}", host.display(), prog, e),
    })?;

    if image.starts_with(b"#!") {
        let (interp, extra) = parse_shebang(prog, &image)?;
        // 内核的拼装顺序：解释器, [#! 行的那个参数], 脚本路径, 原 argv[1..]
        let mut new_argv: Vec<Vec<u8>> = vec![interp.as_bytes().to_vec()];
        if let Some(x) = extra {
            new_argv.push(x.into_bytes());
        }
        new_argv.push(prog.as_bytes().to_vec());
        new_argv.extend(argv.iter().skip(1).cloned());
        return load_depth(mem, vfs, &interp, &new_argv, envp, depth + 1);
    }

    let loaded = elf::load(mem, &image, |interp| {
        // PT_INTERP 走同一套前缀，否则动态链接器会命中宿主的 /lib64/ld-linux
        // 而不是 rootfs 里的那个。
        let hp = vfs.host_path_confined(interp).ok_or_else(|| {
            format!("动态链接器 '{interp}' 越出了 rootfs")
        })?;
        std::fs::read(&hp).map_err(|e| {
            format!("读取动态链接器 '{}'（guest '{interp}'）失败：{e}", hp.display())
        })
    })
    .map_err(|e| LoadFail {
        errno: -crate::syscall::ENOEXEC,
        msg: format!("加载 '{prog}' 失败：{e}"),
    })?;

    let rsp = stack::setup(mem, &loaded, argv, envp);
    Ok(Program { loaded, rsp })
}

/// 解析 `#!` 行：返回（解释器, 可选的单个参数）。
fn parse_shebang(prog: &str, image: &[u8]) -> Result<(String, Option<String>), LoadFail> {
    let end = image.iter().position(|&b| b == b'\n').unwrap_or(image.len());
    let line = String::from_utf8_lossy(&image[2..end]).trim().to_string();
    let mut it = line.splitn(2, char::is_whitespace);
    let interp = it.next().unwrap_or("").to_string();
    if interp.is_empty() {
        return Err(LoadFail {
            errno: -crate::syscall::ENOEXEC,
            msg: format!("'{prog}' 的 #! 行是空的"),
        });
    }
    // Linux 只把 #! 行拆成「解释器 + 最多一个参数」，剩下的整体算一个参数。
    let extra = it
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok((interp, extra))
}
