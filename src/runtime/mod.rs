//! 纯 Rust Linux guest 运行时——**进程内**入口。
//!
//! 引擎本体在 workspace 里的 `wbox-linux` crate（`crates/wbox-linux`），
//! 同时提供 lib 与 bin 两种形态：
//!
//! - **bin**（`wbox-linux.exe`）：`backend::emu::EmuBackend` 用的形态。
//!   guest 跑在独立进程里，外面套 AppContainer + Job，形成 §2.4 Q2 说的双层隔离。
//! - **lib**（本模块）：把同一个引擎嵌进 `wbox.exe`，不额外起进程。
//!
//! 这里刻意**只做转接、不重复实现**：早期本模块自带一份 ELF loader 与
//! `MOV`/`syscall` 两条指令的解释器，那是纵切骨架；引擎补齐后保留两份实现
//! 只会让两边慢慢漂移。
//!
//! 进程内形态目前**不是产品路径**：`wbox.exe` 同时是 CLI 与 supervisor，
//! 直接在它里面跑 guest 会丢掉"AppContainer 套模拟器"这层隔离。
//! 合并成单一 exe 的前提条件见 PRD §4.9 R8。

use std::fmt;

/// 进程内执行的默认指令预算。到顶按超时终止，避免 guest 死循环把
/// `wbox.exe` 一起挂住（bin 形态由 `WBOX_MAX_INSNS` 控制，默认不限）。
const DEFAULT_INSTRUCTION_BUDGET: u64 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError(String);

impl RuntimeError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeError {}

pub type Result<T> = std::result::Result<T, RuntimeError>;

/// 在本进程内加载并执行一个静态 x86-64 Linux ELF，返回其退出码。
///
/// `argv[0]` 用占位名、环境为空：这个入口是给"跑一段自包含 ELF"用的
/// （单测与将来的单 exe 路径），不承担镜像语义。带 rootfs / env / 卷的
/// 完整容器语义走 `EmuBackend`。
pub fn run_static_elf(bytes: &[u8]) -> Result<i32> {
    use wbox_linux::machine::Machine;
    use wbox_linux::syscall::Os;
    use wbox_linux::{elf, stack};

    let mut machine = Machine::new(Os::new());

    // 静态 ELF 不该有 PT_INTERP；真有的话说明调用方用错了入口，如实报错。
    let loaded = elf::load(&mut machine.mem, bytes, |interp| {
        Err(format!(
            "run_static_elf 只接受静态 ELF，但该文件要求动态链接器 '{interp}'"
        ))
    })
    .map_err(|e| RuntimeError::new(e.to_string()))?;

    machine.mem.brk = loaded.brk;
    machine.mem.brk_start = loaded.brk;

    let rsp = stack::setup(&mut machine.mem, &loaded, &[b"wbox-guest".to_vec()], &[]);
    machine.cpu.set_rsp(rsp);
    machine.cpu.rip = loaded.entry;

    machine
        .run(DEFAULT_INSTRUCTION_BUDGET)
        .map_err(|e| RuntimeError::new(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn exit_elf(code: u32) -> Vec<u8> {
        let program = [
            0x48,
            0xc7,
            0xc0,
            60,
            0,
            0,
            0, // mov rax, SYS_exit
            0x48,
            0xc7,
            0xc7, // mov rdi, code
            code as u8,
            (code >> 8) as u8,
            (code >> 16) as u8,
            (code >> 24) as u8,
            0x0f,
            0x05, // syscall
        ];
        let mut elf = vec![0u8; 0x100 + program.len()];
        elf[0..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        elf[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        elf[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());
        elf[24..32].copy_from_slice(&0x400000u64.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());

        let ph = 64;
        elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        elf[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes()); // PF_R | PF_X
        elf[ph + 8..ph + 16].copy_from_slice(&0x100u64.to_le_bytes());
        elf[ph + 16..ph + 24].copy_from_slice(&0x400000u64.to_le_bytes());
        elf[ph + 32..ph + 40].copy_from_slice(&(program.len() as u64).to_le_bytes());
        elf[ph + 40..ph + 48].copy_from_slice(&(program.len() as u64).to_le_bytes());
        elf[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        elf[0x100..].copy_from_slice(&program);
        elf
    }

    #[test]
    fn static_elf_exits_through_linux_syscall() {
        assert_eq!(run_static_elf(&exit_elf(42)).unwrap(), 42);
    }

    #[test]
    fn malformed_elf_is_rejected_without_panicking() {
        // 断言的是"被拒绝且不 panic"，以及错误文案点明了是 ELF 问题。
        // 文案本身随引擎变化（早期骨架说的是 "header"），所以只锚定 "ELF"。
        let err = run_static_elf(b"\x7fELF").unwrap_err();
        assert!(err.to_string().contains("ELF"), "{err}");
    }

    #[test]
    fn dynamic_elf_is_rejected_with_a_pointed_message() {
        // 这个入口只承诺静态 ELF；要求动态链接器时必须说清楚，
        // 而不是含糊地报"加载失败"。
        let mut elf = exit_elf(0);
        // 把唯一的 program header 从 PT_LOAD 改成 PT_INTERP
        elf[64..68].copy_from_slice(&3u32.to_le_bytes());
        let err = run_static_elf(&elf).unwrap_err().to_string();
        assert!(err.contains("静态") || err.contains("PT_LOAD"), "{err}");
    }

    /// 原生代码欠债只能减不能增，且不得逸出既有的两个根目录。
    ///
    /// **数的是 git 跟踪的文件，不是磁盘上的文件。** 早先版本走文件系统，
    /// 于是把构建产物也算了进去——`vendor/blink/config.h`（被 .gitignore 忽略）
    /// 与 `build-win32/version.h` 都是 configure/构建生成的，在任何编译过 blink
    /// 的机器上都会让这条断言变红，而欠债本身一点没变。判据数错了对象，
    /// 报出来的却像是回归。
    ///
    /// 拿不到 git 时跳过：这是一条治理性断言，没有它产品照样正确，
    /// 而在没有版本库的环境里硬凑一个近似答案只会重新引入同类误判。
    #[test]
    fn native_source_debt_cannot_expand_or_escape_legacy_roots() {
        const NATIVE_EXT: &[&str] = &["c", "cc", "cpp", "cxx", "h", "hpp", "s", "asm"];

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(out) = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "ls-files"])
            .output()
        else {
            eprintln!("跳过原生欠债断言：本环境没有 git");
            return;
        };
        if !out.status.success() {
            eprintln!("跳过原生欠债断言：这里不是 git 工作区");
            return;
        }

        let mut counts = [0usize; 2];
        let mut unexpected = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let ext = Path::new(line)
                .extension()
                .and_then(|v| v.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !NATIVE_EXT.contains(&ext.as_str()) {
                continue;
            }
            if line.starts_with("vendor/blink") {
                counts[0] += 1;
            } else if line.starts_with("tests/guest") {
                counts[1] += 1;
            } else {
                unexpected.push(line.to_string());
            }
        }
        assert!(
            unexpected.is_empty(),
            "native source escaped legacy roots: {unexpected:?}"
        );
        // 棘轮已收紧：vendor/blink 整个删除后必须保持为 0，任何形式的回流
        // （重新 vendor C 实现）都要在这里立刻变红。
        // tests/guest 的 22 个 .c 是被模拟执行的 guest 夹具，不进发布物；
        // 按 F4 计划仍应改写成 no_std Rust，届时把上界一起降下来。
        assert_eq!(counts[0], 0, "vendor/blink 已删除，不得回流 C 实现");
        assert!(
            counts[1] <= 22,
            "native debt grew: tests/guest={}",
            counts[1]
        );
    }

    /// §2.2.1 的**总棘轮**：整棵构建图里只允许第一方 crate 与平台 ABI 声明。
    ///
    /// 上面两条棘轮各盯一面（有没有 C、某几个 crate 有没有回流），这一条
    /// 直接盯**结果**：`Cargo.lock` 里除了 `wbox-*` 自己的包，只能有
    /// `libc` / `windows-sys` 及其 target 垫片。任何新依赖——不管是不是纯
    /// Rust、不管有没有列进上面的禁用名单——都会在这里变红。
    ///
    /// 判据用 `Cargo.lock` 而不是 `Cargo.toml`，因为这里要盯的正是
    /// **传递依赖**：直接依赖清白但拖进来一串传递依赖，前两条棘轮都看不见。
    /// （lock 里可能有因可选 feature 被解析器记下、实际不进构建图的条目，
    /// 所以允许名单里留了 windows 那几个 target 垫片。）
    #[test]
    fn dependency_graph_is_first_party_plus_platform_abi_only() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(lock) = std::fs::read_to_string(root.join("Cargo.lock")) else {
            eprintln!("跳过依赖图断言：读不到 Cargo.lock");
            return;
        };
        // 平台 ABI 声明：只是 extern 声明，不编译任何第三方实现代码。
        const PLATFORM_ABI: &[&str] = &[
            "libc",
            "windows-sys",
            "windows-targets",
            "windows_aarch64_gnullvm",
            "windows_aarch64_msvc",
            "windows_i686_gnu",
            "windows_i686_gnullvm",
            "windows_i686_msvc",
            "windows_x86_64_gnu",
            "windows_x86_64_gnullvm",
            "windows_x86_64_msvc",
        ];
        let mut unexpected = Vec::new();
        for line in lock.lines() {
            let Some(name) = line.strip_prefix("name = \"") else {
                continue;
            };
            let name = name.trim_end_matches('"');
            if name.starts_with("wbox") || PLATFORM_ABI.contains(&name) {
                continue;
            }
            unexpected.push(name.to_string());
        }
        assert!(
            unexpected.is_empty(),
            "构建图里出现了第一方与平台 ABI 之外的 crate：{unexpected:?}（见 PRD §2.2.1）"
        );
    }

    /// §2.2.1 **第二档**的棘轮：承载产品语义的第三方 crate 不得回流。
    ///
    /// 上一条棘轮盯的是"有没有 C"。那是必要条件不是充分条件——`serde_json`
    /// 解析 manifest、`sha2` 算 digest、`flate2` 解层、`ureq` 跑 registry
    /// 协议，全都是纯 Rust，却全都承载着镜像管理的核心语义。它们已经换成
    /// 第一方实现，这条断言防的是"顺手 `cargo add` 一个回来"。
    ///
    /// **判据是 `Cargo.toml` 而不是 `Cargo.lock`**：lock 里会出现一些因
    /// 可选 feature 而被解析器记下、实际并不进构建图的条目（`ring`、`cc`
    /// 就在里面）。盯 lock 会得到一个吓人但错误的结论。
    #[test]
    fn replaced_third_party_crates_cannot_come_back() {
        // 名字取得刻意宽松（带 `-`/`_` 两形），免得换个同类 crate 就绕过去。
        const FORBIDDEN: &[&str] = &[
            "serde_json",
            "serde-json",
            "sha2",
            "base64",
            "flate2",
            "miniz_oxide",
            "tar",
            "anyhow",
            "ureq",
            "reqwest",
            "rustls",
            "rustls-rustcrypto",
            "webpki-roots",
            "native-tls",
            "openssl",
            "ring",
        ];
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for manifest in [
            root.join("Cargo.toml"),
            root.join("crates/wbox-codec/Cargo.toml"),
            root.join("crates/wbox-http/Cargo.toml"),
            root.join("crates/wbox-linux/Cargo.toml"),
            root.join("crates/wbox-tls/Cargo.toml"),
        ] {
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            for line in text.lines() {
                let t = line.trim();
                if t.is_empty() || t.starts_with('#') || t.starts_with('[') {
                    continue;
                }
                let Some(name) = t.split(['=', ' ']).next() else {
                    continue;
                };
                assert!(
                    !FORBIDDEN.contains(&name),
                    "{} 重新引入了 {name}——它已有第一方实现，见 PRD §2.2.1 第二档",
                    manifest.display()
                );
            }
        }
    }
}
