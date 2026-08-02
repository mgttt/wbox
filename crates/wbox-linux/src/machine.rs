//! `Machine`：CPU + 地址空间 + OS 状态，以及执行异常的定义。

use crate::cpu::Cpu;
use crate::mem::{Fault, Mem};
use crate::syscall::Os;

/// 中断执行的事件。名字沿用 x86 的异常，`Exit` 是 guest 主动退出。
#[derive(Debug, Clone)]
pub enum Exception {
    /// #PF：访存越权或未映射。
    Fault(Fault),
    /// #UD：解码不出来或未实现的指令。带上原始字节，方便照着 objdump 补。
    Undefined { rip: u64, bytes: Vec<u8> },
    /// #DE：除零或商溢出。
    DivideError { rip: u64 },
    /// #BP：`int3`。
    Breakpoint { rip: u64 },
    /// guest 调用了 `exit`/`exit_group`。
    Exit(i32),
    /// guest 被信号打断且默认动作是终止（退出码 128+signo，与 shell 一致）。
    Killed { signo: i32 },
    /// x86 core 将 Linux syscall 交给外层 personality 处理。
    Syscall { ret_rip: u64 },
}

impl std::fmt::Display for Exception {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Exception::Fault(x) => write!(f, "{x}"),
            Exception::Undefined { rip, bytes } => {
                write!(f, "unsupported instruction at {rip:#x}:")?;
                for b in bytes {
                    write!(f, " {b:02x}")?;
                }
                Ok(())
            }
            Exception::DivideError { rip } => write!(f, "divide error at {rip:#x}"),
            Exception::Breakpoint { rip } => write!(f, "breakpoint at {rip:#x}"),
            Exception::Exit(c) => write!(f, "exit({c})"),
            Exception::Killed { signo } => write!(f, "killed by signal {signo}"),
            Exception::Syscall { ret_rip } => write!(f, "syscall trap at {ret_rip:#x}"),
        }
    }
}

impl From<Fault> for Exception {
    fn from(x: Fault) -> Self {
        Exception::Fault(x)
    }
}

pub type ExecResult<T> = Result<T, Exception>;

/// ISA-core-owned execution state.
///
/// This is the first concrete ownership boundary for W21: the Linux
/// personality remains in [`Machine`], while CPU registers, address space and
/// execution controls can move to an independent core without changing guest
/// ABI code.
#[derive(Clone)]
pub struct CoreState {
    pub cpu: Cpu,
    pub mem: Mem,
    /// `WBOX_TRACE=1`：每条指令打一行寄存器转储到 stderr。
    pub trace: bool,
    /// 指令数上限（0 = 不限）。fork 子进程继承同一个预算。
    pub max_insns: u64,
}

impl CoreState {
    fn new() -> Self {
        Self {
            cpu: Cpu::new(),
            mem: Mem::new(),
            trace: std::env::var_os("WBOX_TRACE").is_some_and(|v| v != "0"),
            max_insns: 0,
        }
    }
}

pub struct Machine {
    pub core: CoreState,
    pub os: Os,
}

impl std::ops::Deref for Machine {
    type Target = CoreState;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl std::ops::DerefMut for Machine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

impl Machine {
    pub fn new(os: Os) -> Self {
        Machine {
            core: CoreState::new(),
            os,
        }
    }

    /// 执行一条 guest 指令，并让 Linux personality 处理 core trap。
    ///
    /// `exec.rs` 只实现 x86 指令语义；syscall 的编号、参数和返回值仍由
    /// Linux ABI dispatcher 所有。这个 facade 保持既有调用者的 `step()`
    /// 语义，同时给未来独立 x86 core 留出 `step_core()` 边界。
    pub fn step(&mut self) -> ExecResult<()> {
        match self.core.step_core() {
            Err(Exception::Syscall { ret_rip }) => crate::syscall::dispatch(self, ret_rip),
            result => result,
        }
    }

    /// 一直执行到 guest 退出或出错，返回退出码。
    ///
    /// `max_insns` 是安全阀（0 = 不限）：单测和 `WBOX_MAX_INSNS` 用它保证
    /// 死循环的 guest 不会把测试挂住。
    pub fn run(&mut self, max_insns: u64) -> ExecResult<i32> {
        self.max_insns = max_insns;
        loop {
            if max_insns != 0 && self.cpu.icount >= max_insns {
                return Err(Exception::Killed { signo: 24 }); // SIGXCPU
            }
            if self.trace {
                eprintln!("{}", self.cpu.dump());
            }
            match self.step() {
                Ok(()) => {}
                Err(Exception::Exit(code)) => return Ok(code),
                Err(e) => return Err(e),
            }
        }
    }
}
