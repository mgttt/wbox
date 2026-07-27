use super::elf::AddressSpace;
use super::{Result, RuntimeError};

const RAX: usize = 0;
const RDI: usize = 7;
const SYS_EXIT: u64 = 60;

pub(super) struct Cpu {
    registers: [u64; 16],
    rip: u64,
}

impl Cpu {
    pub(super) fn new(entry: u64) -> Self {
        Self {
            registers: [0; 16],
            rip: entry,
        }
    }

    pub(super) fn run(mut self, memory: &AddressSpace, budget: u64) -> Result<i32> {
        for _ in 0..budget {
            let prefix = memory.read_executable(self.rip, 2)?;
            match (prefix[0], prefix[1]) {
                (0x48, 0xc7) => self.mov_register_imm32(memory)?,
                (0x48, 0xb8..=0xbf) => self.mov_register_imm64(memory)?,
                (0x0f, 0x05) => {
                    self.rip += 2;
                    if self.registers[RAX] == SYS_EXIT {
                        return Ok(self.registers[RDI] as i32);
                    }
                    return Err(RuntimeError::new(format!(
                        "unsupported Linux syscall {}",
                        self.registers[RAX]
                    )));
                }
                (first, second) => {
                    return Err(RuntimeError::new(format!(
                        "unsupported x86-64 instruction {first:02x} {second:02x} at {:#x}",
                        self.rip
                    )));
                }
            }
        }
        Err(RuntimeError::new("guest instruction budget exhausted"))
    }

    fn mov_register_imm32(&mut self, memory: &AddressSpace) -> Result<()> {
        let instruction = memory.read_executable(self.rip, 7)?;
        let modrm = instruction[2];
        if modrm & 0xf8 != 0xc0 {
            return Err(RuntimeError::new(
                "only MOV r64, sign-extended imm32 is supported",
            ));
        }
        let register = (modrm & 7) as usize;
        let immediate = i32::from_le_bytes(instruction[3..7].try_into().unwrap());
        self.registers[register] = immediate as i64 as u64;
        self.rip += 7;
        Ok(())
    }

    fn mov_register_imm64(&mut self, memory: &AddressSpace) -> Result<()> {
        let instruction = memory.read_executable(self.rip, 10)?;
        let register = (instruction[1] - 0xb8) as usize;
        self.registers[register] = u64::from_le_bytes(instruction[2..10].try_into().unwrap());
        self.rip += 10;
        Ok(())
    }
}
