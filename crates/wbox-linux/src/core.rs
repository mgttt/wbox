//! ISA-core-owned state and traps.
//!
//! This module deliberately does not depend on the Linux personality. The
//! `Machine` facade and `syscall` module translate these core events into
//! Linux ABI outcomes at the boundary.

use crate::cpu::Cpu;
use crate::mem::{Fault, Mem};

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
pub struct CoreState {
    pub cpu: Cpu,
    pub mem: Mem,
    /// `WBOX_TRACE=1`: print one register dump per instruction.
    pub trace: bool,
    /// Instruction budget (`0` means unlimited).
    pub max_insns: u64,
}

impl CoreState {
    pub(crate) fn new() -> Self {
        Self {
            cpu: Cpu::new(),
            mem: Mem::new(),
            trace: std::env::var_os("WBOX_TRACE").is_some_and(|v| v != "0"),
            max_insns: 0,
        }
    }
}
