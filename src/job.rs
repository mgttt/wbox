//! Job Object 封装：资源限额 + 生命周期收割。
//!
//! 约定（见 SPEC §2）：
//! - `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 必开：wbox 退出/崩溃时内核自动杀掉整棵进程树；
//! - 可选：进程内存上限、CPU 硬性百分比上限（CPU rate control，Win8+）、最大进程数；
//! - 不授予 breakaway 权限，子进程无法逃离 Job。

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectCpuRateControlInformation,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
};

use crate::error::Result;
use crate::token::OwnedHandle;

// 限额参数直接用 backend::Limits：原先 job.rs 另有一个 JobLimits，字段与
// Limits 逐一对应，native.rs 每次都做一次纯拷贝式转换——两份定义容易漂移，
// 且换算逻辑被困在 cfg(windows) 里测不到。现统一为一处，换算方法
// （memory_limit_bytes / cpu_rate）挂在 Limits 上，跨平台可单测。
use crate::backend::Limits as JobLimits;

/// RAII 包装：匿名 Job Object。
pub struct Job {
    handle: OwnedHandle,
    limits: JobLimits,
}

impl Job {
    /// 创建 Job 并下发限额。
    pub fn create(limits: JobLimits) -> Result<Self> {
        // # Safety: 传 null 安全属性创建匿名 Job；失败返回 null。
        let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if h.is_null() {
            let err = unsafe { GetLastError() };
            return Err(crate::error::WboxError::job(format!("CreateJobObjectW 失败，GetLastError={}", err)));
        }
        let job = Job {
            handle: OwnedHandle(h),
            limits,
        };
        job.apply_limits()?;
        Ok(job)
    }

    /// 汇总并下发所有限额到内核。
    fn apply_limits(&self) -> Result<()> {
        // ---- 基础限额 + 内存（Extended 结构覆盖 Basic）----
        let mut basic: JOBOBJECT_BASIC_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        // KILL_ON_JOB_CLOSE 必开；当前进程本身若已在 Job 中且带
        // SILENT_BREAKAWAY 之类限制时会失败，此时给出可读错误。
        basic.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if self.limits.max_procs > 0 {
            basic.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            basic.ActiveProcessLimit = self.limits.max_procs;
        }

        let mut ext: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        ext.BasicLimitInformation = basic;
        // MB→字节换算（含溢出检查）在 backend::Limits::memory_limit_bytes，
        // 那里跨平台可测；此处只负责下发。
        if let Some(bytes) = self.limits.memory_limit_bytes()? {
            ext.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            ext.ProcessMemoryLimit = bytes;
        }
        // # Safety: 结构体已完整初始化，句柄有效，尺寸与信息类匹配。
        let ok = unsafe {
            SetInformationJobObject(
                self.handle.raw(),
                JobObjectExtendedLimitInformation,
                &ext as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(crate::error::WboxError::job(format!(
                    "SetInformationJobObject(ExtendedLimit) 失败，GetLastError={}（可能父进程所在 Job 不允许嵌套限制）",
                    err
                )));
        }

        // ---- CPU rate control（硬性上限，单位 = 百分比 × 100）----
        // CpuRate 语义（百分比 × 100）在 backend::Limits::cpu_rate，跨平台可测。
        if let Some(rate) = self.limits.cpu_rate() {
            let mut cpu: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { std::mem::zeroed() };
            cpu.ControlFlags = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE
                | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            // windows-sys 的 union 字段为 Copy 类型，写字段是安全操作；写入后不再读取其它变体。
            cpu.Anonymous.CpuRate = rate;
            // # Safety: 结构体已初始化，句柄有效。
            let ok = unsafe {
                SetInformationJobObject(
                    self.handle.raw(),
                    JobObjectCpuRateControlInformation,
                    &cpu as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                let err = unsafe { GetLastError() };
                return Err(crate::error::WboxError::job(format!(
                        "SetInformationJobObject(CpuRateControl) 失败，GetLastError={}（需要 Windows 8+）",
                        err
                    )));
            }
        }
        Ok(())
    }

    /// 把进程句柄分配进 Job。
    pub fn assign(&self, process: windows_sys::Win32::Foundation::HANDLE) -> Result<()> {
        // # Safety: 两个句柄均有效；目标进程需未退出且未属于其它不兼容 Job。
        let ok = unsafe { AssignProcessToJobObject(self.handle.raw(), process) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(crate::error::WboxError::job(format!(
                    "AssignProcessToJobObject 失败，GetLastError={}（wbox 可能已运行在不兼容的 Job 中）",
                    err
                )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg(windows)]
mod real_windows_tests {
    use super::*;

    /// Job 创建 + KILL_ON_JOB_CLOSE 必开 + Drop 时关闭句柄不阻塞。
    /// 不分配子进程，只验证 Job Object 内核对象的创建路径。
    #[test]
    #[ignore = "DIAGNOSE: 真机 Win32 Job Object 创建"]
    fn job_create_default_succeeds() {
        let job = Job::create(JobLimits::default()).unwrap();
        // 句柄非空即视为成功（OwnedHandle 内部 RAII，Drop 时 CloseHandle）
        assert!(!job.handle.raw().is_null());
    }

    /// 三项限额同时下发：memory_mb / cpu_pct / max_procs。
    /// 验证 SetInformationJobObject 在合理值下都成功（Win8+ CPU rate control）。
    #[test]
    #[ignore = "DIAGNOSE: 真机 Win32 Job 三项限额下发"]
    fn job_create_with_all_limits() {
        let limits = JobLimits {
            memory_mb: 256,
            cpu_pct: 50,
            max_procs: 10,
        };
        let job = Job::create(limits).unwrap();
        assert!(!job.handle.raw().is_null());
    }

    /// 内存上限边界：0 = 不限（不挂 JOB_OBJECT_LIMIT_PROCESS_MEMORY flag）。
    /// JobLimits 默认实现是全 0，本测试断言 0 不触发任何限额下发。
    #[test]
    #[ignore = "DIAGNOSE: 真机 Win32 Job memory_mb=0 不挂限额"]
    fn job_zero_limits_means_unlimited() {
        // 不 panic 即可（apply_limits 内部对 0 跳过对应 flag）
        let _job = Job::create(JobLimits {
            memory_mb: 0,
            cpu_pct: 0,
            max_procs: 0,
        })
        .unwrap();
    }

    /// memory_mb 上限的算术溢出保护：apply_limits 内 checked_mul(1024*1024)。
    /// 极大值（如 u64::MAX）应走溢出错误分支而非静默 wrap。
    #[test]
    #[ignore = "DIAGNOSE: 真机 Win32 Job memory_mb 极���值溢出保护"]
    fn job_huge_memory_limit_is_caught_as_overflow() {
        let limits = JobLimits {
            memory_mb: u64::MAX,
            cpu_pct: 0,
            max_procs: 0,
        };
        let err = Job::create(limits).err().expect("应报溢出错误");
        let msg = format!("{}", err);
        // anyhow context "内存上限溢出" 来自源码，断言它出现在错误链里
        assert!(
            msg.contains("溢出") || msg.contains("memory") || msg.contains("内存"),
            "应报溢出，实得：{}",
            msg
        );
    }
}
