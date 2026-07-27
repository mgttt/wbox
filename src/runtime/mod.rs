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

    #[test]
    fn native_source_debt_cannot_expand_or_escape_legacy_roots() {
        fn visit(root: &Path, dir: &Path, counts: &mut [usize; 2], unexpected: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    if entry.file_name() != "target" && entry.file_name() != ".git" {
                        visit(root, &path, counts, unexpected);
                    }
                    continue;
                }
                let extension = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !matches!(
                    extension.as_str(),
                    "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "s" | "asm"
                ) {
                    continue;
                }
                let relative = path.strip_prefix(root).unwrap();
                if relative.starts_with("vendor/blink") {
                    counts[0] += 1;
                } else if relative.starts_with("tests/guest") {
                    counts[1] += 1;
                } else {
                    unexpected.push(relative.display().to_string());
                }
            }
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut counts = [0; 2];
        let mut unexpected = Vec::new();
        visit(root, root, &mut counts, &mut unexpected);
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
