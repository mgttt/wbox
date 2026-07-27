use super::elf::AddressSpace;
use super::{Result, RuntimeError};
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

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
            let instruction = self.decode(memory)?;
            match instruction.mnemonic() {
                Mnemonic::Mov => self.mov_immediate(&instruction)?,
                Mnemonic::Syscall => {
                    self.rip = instruction.next_ip();
                    if self.registers[RAX] == SYS_EXIT {
                        return Ok(self.registers[RDI] as i32);
                    }
                    return Err(RuntimeError::new(format!(
                        "unsupported Linux syscall {}",
                        self.registers[RAX]
                    )));
                }
                mnemonic => {
                    return Err(RuntimeError::new(format!(
                        "unsupported x86-64 instruction {mnemonic:?} at {:#x}",
                        self.rip
                    )));
                }
            }
        }
        Err(RuntimeError::new("guest instruction budget exhausted"))
    }

    fn decode(&self, memory: &AddressSpace) -> Result<Instruction> {
        let mut decoder = Decoder::with_ip(
            64,
            memory.executable_window(self.rip)?,
            self.rip,
            DecoderOptions::NONE,
        );
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return Err(RuntimeError::new(format!(
                "invalid x86-64 instruction at {:#x}",
                self.rip
            )));
        }
        Ok(instruction)
    }

    fn mov_immediate(&mut self, instruction: &Instruction) -> Result<()> {
        if instruction.op_count() != 2 || instruction.op0_kind() != OpKind::Register {
            return Err(RuntimeError::new("only MOV r64, immediate is implemented"));
        }
        let register = register_index(instruction.op0_register()).ok_or_else(|| {
            RuntimeError::new(format!(
                "unsupported MOV destination {:?}",
                instruction.op0_register()
            ))
        })?;
        let value = match instruction.op1_kind() {
            OpKind::Immediate32to64 => instruction.immediate32to64() as u64,
            OpKind::Immediate64 => instruction.immediate64(),
            other => {
                return Err(RuntimeError::new(format!(
                    "unsupported MOV immediate kind {other:?}"
                )));
            }
        };
        self.registers[register] = value;
        self.rip = instruction.next_ip();
        Ok(())
    }
}

fn register_index(register: Register) -> Option<usize> {
    Some(match register {
        Register::RAX => 0,
        Register::RCX => 1,
        Register::RDX => 2,
        Register::RBX => 3,
        Register::RSP => 4,
        Register::RBP => 5,
        Register::RSI => 6,
        Register::RDI => 7,
        Register::R8 => 8,
        Register::R9 => 9,
        Register::R10 => 10,
        Register::R11 => 11,
        Register::R12 => 12,
        Register::R13 => 13,
        Register::R14 => 14,
        Register::R15 => 15,
        _ => return None,
    })
}
