//! ISA-core-owned state and traps.
//!
//! This module deliberately does not depend on the Linux personality. The
//! `Machine` facade and `syscall` module translate these core events into
//! Linux ABI outcomes at the boundary.

use crate::cpu::Cpu;
use crate::mem::{Fault, Mem, MemResult};

/// Memory operations required by the ISA core.
///
/// Linux mapping policy, file-backed mappings and `brk` remain outside this
/// contract. A provider only needs to supply executable fetches, checked data
/// access, and the small setup surface used to load code fixtures.
pub trait AddressSpace: Clone {
    fn map(&mut self, addr: u64, len: u64, prot: u8);
    fn write_raw(&mut self, addr: u64, buf: &[u8]);
    fn fetch_u8(&self, addr: u64) -> MemResult<u8>;
    fn read(&self, addr: u64, buf: &mut [u8]) -> MemResult<()>;
    fn write(&mut self, addr: u64, buf: &[u8]) -> MemResult<()>;
    fn read_u8(&self, addr: u64) -> MemResult<u8>;
    fn write_u8(&mut self, addr: u64, value: u8) -> MemResult<()>;
    fn read_u32(&self, addr: u64) -> MemResult<u32>;
    fn write_u32(&mut self, addr: u64, value: u32) -> MemResult<()>;
    fn read_sized(&self, addr: u64, size: u8) -> MemResult<u64>;
    fn write_sized(&mut self, addr: u64, size: u8, value: u64) -> MemResult<()>;
}

impl AddressSpace for Mem {
    fn map(&mut self, addr: u64, len: u64, prot: u8) {
        Self::map(self, addr, len, prot)
    }

    fn write_raw(&mut self, addr: u64, buf: &[u8]) {
        Self::write_raw(self, addr, buf)
    }

    fn fetch_u8(&self, addr: u64) -> MemResult<u8> {
        Self::fetch_u8(self, addr)
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> MemResult<()> {
        Self::read(self, addr, buf)
    }

    fn write(&mut self, addr: u64, buf: &[u8]) -> MemResult<()> {
        Self::write(self, addr, buf)
    }

    fn read_u8(&self, addr: u64) -> MemResult<u8> {
        Self::read_u8(self, addr)
    }

    fn write_u8(&mut self, addr: u64, value: u8) -> MemResult<()> {
        Self::write_u8(self, addr, value)
    }

    fn read_u32(&self, addr: u64) -> MemResult<u32> {
        Self::read_u32(self, addr)
    }

    fn write_u32(&mut self, addr: u64, value: u32) -> MemResult<()> {
        Self::write_u32(self, addr, value)
    }

    fn read_sized(&self, addr: u64, size: u8) -> MemResult<u64> {
        Self::read_sized(self, addr, size)
    }

    fn write_sized(&mut self, addr: u64, size: u8, value: u64) -> MemResult<()> {
        Self::write_sized(self, addr, size, value)
    }
}

/// ISA core execution events.
#[derive(Debug, Clone)]
pub enum CoreException {
    /// #PF: guest memory access fault.
    Fault(Fault),
    /// #UD: an instruction could not be decoded or is not implemented.
    Undefined { rip: u64, bytes: Vec<u8> },
    /// #DE: division by zero or quotient overflow.
    DivideError { rip: u64 },
    /// #BP: `int3`.
    Breakpoint { rip: u64 },
    /// Guest executed HLT; the Linux facade maps it to exit.
    Halt,
    /// Core hands a Linux syscall to the outer personality.
    Syscall { ret_rip: u64 },
}

pub type CoreResult<T> = Result<T, CoreException>;

impl From<Fault> for CoreException {
    fn from(x: Fault) -> Self {
        Self::Fault(x)
    }
}

/// ISA-core-owned execution state.
#[derive(Clone)]
pub struct CoreState<A: AddressSpace = Mem> {
    pub cpu: Cpu,
    pub mem: A,
    /// `WBOX_TRACE=1`: print one register dump per instruction.
    pub trace: bool,
    /// Instruction budget (`0` means unlimited).
    pub max_insns: u64,
}

impl CoreState<Mem> {
    /// Construct a fresh core with an empty address space and the default
    /// execution controls.
    pub fn new() -> Self {
        Self::with_memory(Mem::new())
    }
}

impl<A: AddressSpace> CoreState<A> {
    /// Construct a core around a provider-owned address space.
    pub fn with_memory(mem: A) -> Self {
        Self {
            cpu: Cpu::new(),
            mem,
            trace: std::env::var_os("WBOX_TRACE").is_some_and(|v| v != "0"),
            max_insns: 0,
        }
    }
}
