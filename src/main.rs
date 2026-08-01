//! wbox —— portable Windows 进程容器（MVP）
//!
//! 隔离单元 = AppContainer（令牌隔离 + Low IL）+ Job Object（资源限额 + 生命周期收割）。
//! 默认不需要管理员权限、不需要 VT-x、不需要启用任何 Windows 可选功能。
//!
//! `wbox run` 有两类目标（见 backend/mod.rs）：
//! - 本地 Windows 可执行路径 → NativeBackend（AppContainer + Job 直接运行）；
//! - 已 pull 的 OCI 镜像引用（或带 `--pull`）→ EmuBackend（wbox-linux 模拟，
//!   骨架；config.json 的 Env/Cmd/Entrypoint/WorkingDir 在此消费）。
//!
//! 退出码约定（SPEC §2 + OCI 扩展）：
//!   子进程退出码原样转发；wbox 自身错误：1=参数 2=profile 3=job 4=进程创建 5=registry/镜像。

mod backend;
// CLI 层：子命令分发、参数解析与 USAGE（纯逻辑跨平台可测）。
mod cli;
mod error;
// 带上下文链的通用错误类型（取代 anyhow，见 fault.rs 模块注释）。
mod fault;
// OCI 镜像拉取：纯 Rust 依赖，跨平台可编译（Linux 沙箱用于实测拉取逻辑）。
mod build;
mod layers;
mod oci;
mod paths;
mod platform;
mod volume;
// capability 面裁剪（PRD F9.8）。解析与求解是纯逻辑，跨平台可测；
// 落地在 backend/linux_ns.rs。
mod caps;
// compose 多容器编排子集（PRD F9.14）。含手写 YAML 子集解析器。
mod compose;
// 健康检查（PRD F9.10）：探针经 setns 跑在容器内，循环挂在 supervisor 上。
mod health;
mod portfwd;
mod restart;
// 纯 Rust Linux ELF/OCI 执行运行时（PRD F4.R1-F4.R4）。
#[allow(dead_code)]
mod runtime;
// 文件系统清理的共用动作（overlayfs 的 mode-000 work 目录要先补权限才删得掉）。
// seccomp-bpf 按 syscall 拦截（PRD F9.9）。解析与 BPF 构造可单测；落地在 linux_ns。
mod seccomp;
// 运行中容器的状态目录与发现（PRD F8.a）。跨平台：锁语义由 OS 保证。
mod runstate;
// 测试脚手架：环境变量互斥 + 自动还原（进程级全局状态在并行用例下必须串行化）。
#[cfg(test)]
mod testenv;
// 以下模块直接调用 Win32 API，仅 Windows 可编译。
#[cfg(windows)]
mod acl;
// Windows OCI filesystem broker transport。CLI 在完整 OPEN/hostfs 门禁前仍拒绝 -v。
#[cfg(windows)]
#[allow(dead_code)] // staged component; activated when emulator OPEN/hostfs gates are complete
mod broker;
#[cfg(windows)]
mod job;
#[cfg(windows)]
mod sandbox;
#[cfg(windows)]
mod token;

fn main() {
    // 错误到退出码的唯一映射点：ErrKind → 退出码见 error.rs。
    let code = match cli::dispatch(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wbox: 错误: {}", e);
            e.exit_code()
        }
    };
    // 子进程退出码按位原样转发（Windows 退出码为 u32，如 0xC000xxxx）。
    std::process::exit(code as i32);
}
