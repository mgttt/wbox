//! Pure Rust Linux guest runtime.
//!
//! The first vertical slice loads a static x86-64 ELF and executes an exit
//! syscall. CPU, memory, VFS, and syscall coverage grow behind this boundary.

mod cpu;
mod elf;

use std::fmt;

use cpu::Cpu;
use elf::{AddressSpace, ElfImage};

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

pub fn run_static_elf(bytes: &[u8]) -> Result<i32> {
    let image = ElfImage::parse(bytes)?;
    let memory = AddressSpace::load(&image)?;
    Cpu::new(image.entry).run(&memory, DEFAULT_INSTRUCTION_BUDGET)
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
            0xc7,
            code as u8,
            (code >> 8) as u8,
            (code >> 16) as u8,
            (code >> 24) as u8, // mov rdi, code
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
        let err = run_static_elf(b"\x7fELF").unwrap_err();
        assert!(err.to_string().contains("header"));
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
        assert!(
            counts[0] <= 452 && counts[1] <= 22,
            "native debt grew: vendor/blink={}, tests/guest={}",
            counts[0],
            counts[1]
        );
    }
}
