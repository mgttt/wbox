//! `wbox-linux`：x86-64 Linux 用户态模拟器，纯 Rust 实现。
//!
//! 这个 crate 取代 `vendor/blink`（C）。分层：
//!
//! ```text
//! main.rs      CLI：解析参数、读 ELF、装配 Machine、转发退出码
//!   └── machine.rs  Machine = Cpu + Mem + Os，取指-译码-执行主循环
//!         ├── elf.rs    ELF64 加载（PT_LOAD 映射、ET_DYN 偏置、PT_INTERP）
//!         ├── stack.rs  初始栈（argc/argv/envp/auxv）
//!         ├── mem.rs    guest 地址空间（稀疏页表 + 权限）
//!         ├── cpu.rs    寄存器/标志/XMM
//!         ├── exec.rs   译码与整数指令执行
//!         ├── sse.rs    SSE/SSE2 子集（libc 的字符串函数要用）
//!         ├── alu.rs    算术逻辑与精确标志位
//!         └── syscall/  Linux syscall 模拟 + fd 表 + VFS 路径翻译
//! ```
//!
//! 设计取向与 blink 的差异见 `mem.rs` 与 `cpu.rs` 的模块注释：这里优先
//! 平台无关与语义正确，把性能优化留给后续里程碑。
//!
//! # 相对 blink 的已知缺口
//!
//! 这些是**尚未实现**、会明确报错而不是静默跑错的部分：
//!
//! - `fork`/`clone`/`execve`/`wait4`：多进程。blink 用快照式 fork 实现，
//!   本 crate 暂返回 `ENOSYS`；影响 shell 的管道与后台任务。
//! - 线程（`clone(CLONE_THREAD)`）与真正的 futex 等待。
//! - 信号投递（`kill`/`sigaction` 的处理器调用）；登记接口成功但不投递。
//! - socket 族、epoll/eventfd/timerfd/signalfd。
//! - x87 浮点（`fld1`/`fnstcw` 等）。SSE/SSE2 的标量与打包浮点已实现，
//!   但 glibc 的 `long double` 路径走 x87；受影响的有 `seq`、`printf "%f"`。
//! - JIT；当前纯解释执行。
//! - `MAP_SHARED` 文件映射的写回、pty。

pub mod alu;
pub mod cpu;
pub mod elf;
pub mod exec;
pub mod machine;
pub mod mem;
pub mod sse;
pub mod stack;
pub mod syscall;
