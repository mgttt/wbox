//! `wbox-linux`：在宿主上执行 Linux x86-64 ELF 程序。
//!
//! 用法与被取代的 blink 版保持一致，`src/backend/blink.rs` 不需要改：
//!
//! ```text
//! wbox-linux [--version] [--] <guest-程序> [参数...]
//! ```
//!
//! guest 的 `/` 由 `WBOX_PREFIX`（兼容名 `BLINK_PREFIX`）指定；不设则直通宿主根。

use std::process::ExitCode;
use wbox_linux::machine::{Exception, Machine};
use wbox_linux::syscall::Os;
use wbox_linux::{elf, stack};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 不得透传给 guest 的内部控制键（PRD §F7.2）。
const INTERNAL_ENV_PREFIXES: [&str; 2] = ["WBOX_", "BLINK_"];

fn usage() -> String {
    format!(
        "wbox-linux {VERSION} —— x86-64 Linux 用户态模拟器（纯 Rust）

用法：
  wbox-linux [选项] [--] <程序> [参数...]

选项：
  --version          打印版本后退出
  --help             打印本帮助后退出

环境变量：
  WBOX_PREFIX=<目录> guest 的 / 映射到的宿主目录（兼容名 BLINK_PREFIX）
  WBOX_STRACE=1      打印每次 syscall
  WBOX_TRACE=1       打印每条指令的寄存器状态（极慢，只用于定位）
  WBOX_MAX_INSNS=N   指令数上限，超出按 SIGXCPU 终止（0 = 不限）
"
    )
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    // 第一个参数决定一切：我们支持的选项都是终结性的（打印后退出），
    // 其余情况第一个非选项参数就是 guest 程序，后面全部原样交给 guest
    // ——**不能**继续解析，否则 guest 自己的 `--version` 会被我们吃掉。
    let first = args.next();
    let (prog, rest): (Option<String>, Vec<String>) = match first.as_deref() {
        None => {
            eprint!("{}", usage());
            return ExitCode::from(2);
        }
        Some("--version") => {
            println!("wbox-linux {VERSION}");
            return ExitCode::SUCCESS;
        }
        Some("--help") | Some("-h") => {
            print!("{}", usage());
            return ExitCode::SUCCESS;
        }
        // `--` 显式终止选项：后面第一个是程序名（程序名本身可能以 - 开头）
        Some("--") => (args.next(), args.collect()),
        Some(_) => (first, args.collect()),
    };

    let Some(prog) = prog else {
        eprint!("{}", usage());
        return ExitCode::from(2);
    };

    match run(&prog, &rest) {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("wbox-linux: {e}");
            ExitCode::from(127)
        }
    }
}

/// 装配并运行 guest，返回退出码。
fn run(prog: &str, argv_rest: &[String]) -> Result<i32, String> {
    let os = Os::new();

    // guest 路径 -> 宿主路径。程序本身也走 VFS，这样 rootfs 里的
    // /bin/sh 才能按 guest 视角指定。
    let host = os.vfs.host_path(prog);
    let image = std::fs::read(&host)
        .map_err(|e| format!("读取 '{}'（guest '{}'）失败：{}", host.display(), prog, e))?;

    // `#!` 脚本：由加载器之外处理，与内核的 binfmt_script 一致。
    if image.starts_with(b"#!") {
        return run_script(prog, &image, argv_rest);
    }

    let mut m = Machine::new(os);

    // PT_INTERP 的解析要走同一套 VFS 前缀，否则动态链接器会命中宿主的
    // /lib64/ld-linux 而不是 rootfs 里的那个。
    let prefix_vfs = wbox_linux::syscall::fs::Vfs::from_env();
    let loaded = elf::load(&mut m.mem, &image, |interp| {
        let hp = prefix_vfs.host_path(interp);
        std::fs::read(&hp).map_err(|e| {
            format!(
                "读取动态链接器 '{}'（guest '{}'）失败：{}",
                hp.display(),
                interp,
                e
            )
        })
    })
    .map_err(|e| format!("加载 '{prog}' 失败：{e}"))?;

    m.mem.brk = loaded.brk;
    m.mem.brk_start = loaded.brk;

    let mut argv: Vec<Vec<u8>> = vec![prog.as_bytes().to_vec()];
    argv.extend(argv_rest.iter().map(|s| s.as_bytes().to_vec()));

    let envp: Vec<Vec<u8>> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let k = k.to_string_lossy().into_owned();
            if INTERNAL_ENV_PREFIXES.iter().any(|p| k.starts_with(p)) {
                return None;
            }
            Some(format!("{k}={}", v.to_string_lossy()).into_bytes())
        })
        .collect();

    let rsp = stack::setup(&mut m.mem, &loaded, &argv, &envp);
    m.cpu.set_rsp(rsp);
    m.cpu.rip = loaded.entry;

    let max = std::env::var("WBOX_MAX_INSNS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    match m.run(max) {
        Ok(code) => Ok(code),
        Err(e) => Err(fatal_report(&m, &e)),
    }
}

/// `#!` 脚本：取出解释器与可选的单个参数，改成执行解释器。
fn run_script(path: &str, image: &[u8], argv_rest: &[String]) -> Result<i32, String> {
    let line_end = image.iter().position(|&b| b == b'\n').unwrap_or(image.len());
    let line = String::from_utf8_lossy(&image[2..line_end]);
    let line = line.trim().to_string();
    if line.is_empty() {
        return Err(format!("'{path}' 的 #! 行是空的"));
    }
    // Linux 只把 #! 行拆成「解释器 + 最多一个参数」，剩下的整体算一个参数。
    let mut it = line.splitn(2, char::is_whitespace);
    let interp = it.next().unwrap().to_string();
    let mut new_argv: Vec<String> = Vec::new();
    if let Some(arg) = it.next() {
        let arg = arg.trim();
        if !arg.is_empty() {
            new_argv.push(arg.to_string());
        }
    }
    new_argv.push(path.to_string());
    new_argv.extend(argv_rest.iter().cloned());
    run(&interp, &new_argv)
}

/// 崩溃报告。内容对齐 blink 的 `fatal host exception`，沿用既有排查习惯。
fn fatal_report(m: &Machine, e: &Exception) -> String {
    let mut s = format!(
        "fatal: {e}\n  guest pid={} icount={}\n  {}",
        m.os.pid,
        m.cpu.icount,
        m.cpu.dump()
    );
    if let Exception::Undefined { .. } = e {
        s.push_str("\n  这条指令模拟器还没实现。请把上面的字节序列连同复现命令提交 issue。");
    }
    s
}
