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
use wbox_linux::proc;
use wbox_linux::syscall::Os;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 不得透传给 guest 的内部控制键（PRD §F7.2）。
const INTERNAL_ENV_PREFIXES: [&str; 2] = ["WBOX_", "BLINK_"];

fn usage() -> String {
    format!(
        "wbox-linux {VERSION} —— x86-64 Linux 用户态模拟器（纯 Rust）

This is the internal Linux ELF runtime of wbox. It is not the container CLI:
Use wbox.exe for containers, images and isolation. Running guests directly
through this executable bypasses AppContainer/Job isolation.

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
    let mut m = Machine::new(Os::new());

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

    // 装载走的是和 `execve` 完全同一份代码（ELF、`#!` 脚本、PT_INTERP 都在里面），
    // 这样"直接启动"和"guest 自己 exec 出来"的程序不可能有行为差异。
    let program = proc::load_into(&mut m.mem, &m.os.vfs, prog, &argv, &envp)
        .map_err(|e| e.msg)?;

    m.os.exe = m.os.vfs.guest_abs(prog);
    m.mem.brk = program.loaded.brk;
    m.mem.brk_start = program.loaded.brk;
    m.cpu.set_rsp(program.rsp);
    m.cpu.rip = program.loaded.entry;

    let max = std::env::var("WBOX_MAX_INSNS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    match m.run(max) {
        Ok(code) => Ok(code),
        Err(e) => Err(fatal_report(&m, &e)),
    }
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
