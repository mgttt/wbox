//! `wbox-linux`：x86-64 Linux 用户态模拟器，纯 Rust 实现。
//!
//! 这个 crate 取代 `vendor/blink`（C）。分层：
//!
//! ```text
//! main.rs      CLI：解析参数、读 ELF、装配 Machine、转发退出码
//!   └── machine.rs  Machine = Cpu + Mem + Os，取指-译码-执行主循环
//!         ├── elf.rs    ELF64 加载（PT_LOAD 映射、ET_DYN 偏置、PT_INTERP）
//!         ├── proc.rs   程序装载（elf + stack + #! 脚本），execve 与启动共用
//!         ├── stack.rs  初始栈（argc/argv/envp/auxv）
//!         ├── mem.rs    guest 地址空间（稀疏页表 + 权限）
//!         ├── cpu.rs    寄存器/标志/XMM
//!         ├── exec.rs   译码与整数指令执行
//!         ├── sse.rs    SSE/SSE2 子集（libc 的字符串函数要用）
//!         ├── alu.rs    算术逻辑与精确标志位
//!         └── syscall/  Linux syscall 模拟 + fd 表 + VFS 路径翻译
//!               ├── fs.rs       fd 表、管道、合成的 /dev/*、VFS 路径翻译
//!               └── process.rs  fork/vfork/clone/execve/wait4（快照式 fork）
//! ```
//!
//! 设计取向与 blink 的差异见 `mem.rs` 与 `cpu.rs` 的模块注释：这里优先
//! 平台无关与语义正确，把性能优化留给后续里程碑。
//!
//! # 相对 blink 的已知缺口
//!
//! 这些是**尚未实现**、会明确报错而不是静默跑错的部分：
//!
//! - 真正**并发**的多进程。`fork`/`vfork`/`clone`/`execve`/`wait4` 已按
//!   blink 的**快照式 fork** 实现（见 `syscall/process.rs`）：fork 时克隆整个
//!   地址空间，子进程先跑到底，父进程再继续。`a && b`、`$(...)`、`cmd; cmd`
//!   这类顺序模式可用；两端同时存活的管道会死锁。
//! - `MAP_SHARED` 不跨进程共享，文件映射也不写回宿主：页表在 fork 时按值
//!   深拷贝，共享区域因此在父子之间断开。
//! - 线程（`clone(CLONE_VM|CLONE_THREAD)`）与真正的 futex 等待；带线程标志的
//!   `clone` 明确 `ENOSYS`，不假装成功。
//! - 信号投递（`kill`/`sigaction` 的处理器调用）；登记接口成功但不投递。
//!   因此 `pause()`/`sigsuspend()` 是确定的死锁，按 SIGKILL 终止并打印原因。
//! - socket 族、epoll/eventfd/timerfd/signalfd。
//! - x87 浮点（`fld1`/`fnstcw` 等）。SSE/SSE2 的标量与打包浮点已实现，
//!   但 glibc 的 `long double` 路径走 x87；受影响的有 `seq`、`printf "%f"`。
//! - JIT；当前纯解释执行。
//! - `dup` 出来的 fd 与原 fd 共享文件偏移但**不**共享状态标志
//!   （`O_APPEND`/`O_NONBLOCK`）；pty。

pub mod alu;
pub mod cpu;
pub mod elf;
pub mod env_payload;
pub mod exec;
pub mod machine;
pub mod mem;
pub mod proc;
pub mod sse;
pub mod stack;
pub mod syscall;

#[cfg(test)]
mod dependency_tests {
    #[test]
    fn direct_platform_dependency_requests_only_identity_and_entropy() {
        let dependency = include_str!("../Cargo.toml")
            .lines()
            .find(|line| line.starts_with("agenterm-platform ="))
            .expect("wbox-linux must declare agenterm-platform directly");
        assert!(dependency.contains("\"entropy\""));
        assert!(dependency.contains("\"file-identity\""));
        assert!(!dependency.contains("\"filesystem\""));
    }
}
