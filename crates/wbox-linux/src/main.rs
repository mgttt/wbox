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
  -s                 打印每次 syscall（等价于 WBOX_STRACE=1）
  -e                 诊断输出到 stderr（本实现一直如此，接受以兼容旧命令行）

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
    // 只吃**前导**选项，第一个非选项参数就是 guest 程序，它之后的一律原样
    // 交给 guest ——**不能**继续解析，否则 guest 自己的 `--version` 会被我们
    // 吃掉。程序名本身以 `-` 开头时用 `--` 显式终止选项。
    let mut strace = false;
    let mut prog: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--version" => {
                println!("wbox-linux {VERSION}");
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                print!("{}", usage());
                return ExitCode::SUCCESS;
            }
            // -s/-e 是被取代的 blink 的命令行拼写，保留以免驱动它的脚本失效。
            // -s = 打印 syscall；-e = 诊断走 stderr（我们一直如此，接受即忽略）。
            "-s" => strace = true,
            "-e" => {}
            "--" => {
                prog = args.next();
                rest = args.collect();
                break;
            }
            _ => {
                prog = Some(a);
                rest = args.collect();
                break;
            }
        }
    }

    let Some(prog) = prog else {
        eprint!("{}", usage());
        return ExitCode::from(2);
    };

    match run(&prog, &rest, strace) {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("wbox-linux: {e}");
            ExitCode::from(127)
        }
    }
}

/// 装配并运行 guest，返回退出码。
fn run(prog: &str, argv_rest: &[String], strace: bool) -> Result<i32, String> {
    let mut os = Os::new();
    // 命令行的 -s 与 WBOX_STRACE=1 是或的关系：任一给出就开。
    os.strace |= strace;
    let mut m = Machine::new(os);

    let mut argv: Vec<Vec<u8>> = vec![prog.as_bytes().to_vec()];
    argv.extend(argv_rest.iter().map(|s| s.as_bytes().to_vec()));

    let envp: Vec<Vec<u8>> = match std::env::var(wbox_linux::env_payload::ENV_NAME) {
        Ok(payload) => wbox_linux::env_payload::decode(&payload)?
            .into_iter()
            .map(|(key, value)| format!("{key}={value}").into_bytes())
            .collect(),
        Err(_) => std::env::vars_os()
            .filter_map(|(k, v)| {
                let k = k.to_string_lossy().into_owned();
                if INTERNAL_ENV_PREFIXES.iter().any(|p| k.starts_with(p)) {
                    return None;
                }
                Some(format!("{k}={}", v.to_string_lossy()).into_bytes())
            })
            .collect(),
    };

    // 装载走的是和 `execve` 完全同一份代码（ELF、`#!` 脚本、PT_INTERP 都在里面），
    // 这样"直接启动"和"guest 自己 exec 出来"的程序不可能有行为差异。
    let program = proc::load_into(&mut m.mem, &m.os.vfs, prog, &argv, &envp).map_err(|e| e.msg)?;

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
