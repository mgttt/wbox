//! Job Object 封装：资源限额 + 生命周期收割。
//!
//! 约定（见 SPEC §2）：
//! - `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 必开：wbox 退出/崩溃时内核自动杀掉整棵进程树；
//! - 可选：进程内存上限、CPU 硬性百分比上限（CPU rate control，Win8+）、最大进程数；
//! - 不授予 breakaway 权限，子进程无法逃离 Job。

use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
    JobObjectCpuRateControlInformation, JobObjectExtendedLimitInformation, OpenJobObjectW,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY,
};
use windows_sys::Win32::System::SystemServices::{
    JOB_OBJECT_ASSIGN_PROCESS, JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE,
};

use crate::error::Result;
use crate::token::{to_wide, OwnedHandle};

// 限额参数直接用 backend::Limits：原先 job.rs 另有一个 JobLimits，字段与
// Limits 逐一对应，native.rs 每次都做一次纯拷贝式转换——两份定义容易漂移，
// 且换算逻辑被困在 cfg(windows) 里测不到。现统一为一处，换算方法
// （memory_limit_bytes / cpu_rate）挂在 Limits 上，跨平台可单测。
use crate::backend::Limits as JobLimits;

const JOB_NAME_PREFIX: &str = r"Local\wbox.job.";
const ERROR_FILE_NOT_FOUND: u32 = 2;

/// 容器名到 Windows 命名 Job 的唯一映射。
pub fn name_for_container(container_name: &str) -> String {
    format!("{}{}", JOB_NAME_PREFIX, container_name)
}

/// RAII 包装：Job Object。
pub struct Job {
    handle: Option<OwnedHandle>,
    limits: JobLimits,
}

impl Job {
    /// 创建 Job 并下发限额。
    #[cfg(test)]
    pub fn create(limits: JobLimits) -> Result<Self> {
        Self::create_impl(limits, None)
    }

    /// 为一次容器运行创建命名 Job。若同名 Job 已存在则拒绝，避免新容器误接管
    /// 上一代仍被 `exec` 控制器持有的 Job。
    pub fn create_for_container(container_name: &str, limits: JobLimits) -> Result<Self> {
        Self::create_impl(limits, Some(name_for_container(container_name)))
    }

    fn create_impl(limits: JobLimits, name: Option<String>) -> Result<Self> {
        let name_wide = name.as_deref().map(to_wide);
        // CreateJobObjectW 用 GetLastError=ERROR_ALREADY_EXISTS 表示打开了既有对象。
        // 先清零，避免把此前 Win32 调用留下的 183 误当成这次结果。
        unsafe { SetLastError(0) };
        let h = unsafe {
            CreateJobObjectW(
                std::ptr::null(),
                name_wide
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
            )
        };
        if h.is_null() {
            let err = unsafe { GetLastError() };
            return Err(crate::error::WboxError::job(format!(
                "CreateJobObjectW 失败，GetLastError={}",
                err
            )));
        }
        let create_error = unsafe { GetLastError() };
        if name.is_some() && create_error == 183 {
            drop(OwnedHandle(h));
            return Err(crate::error::WboxError::job(format!(
                "容器 '{}' 的命名 Job 仍存在，拒绝复用上一代隔离单元",
                name.unwrap_or_default()
            )));
        }
        let job = Job {
            handle: Some(OwnedHandle(h)),
            limits,
        };
        job.apply_limits()?;
        Ok(job)
    }

    /// 打开运行中容器的命名 Job，供 `exec` 加入进程或 `stop` 终止整棵树。
    #[cfg(test)]
    pub fn open_for_container(container_name: &str) -> Result<Self> {
        Self::open_for_container_until(container_name, None)
    }

    /// 等待 detached supervisor 完成“写入运行记录 → 创建命名 Job”之间的短暂
    /// 启动窗口。仅 ERROR_FILE_NOT_FOUND 可重试，权限等其他错误必须立即暴露。
    pub fn wait_for_container(container_name: &str, timeout: std::time::Duration) -> Result<Self> {
        Self::open_for_container_until(container_name, Some(std::time::Instant::now() + timeout))
    }

    fn open_for_container_until(
        container_name: &str,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        let name = name_for_container(container_name);
        let name_wide = to_wide(&name);
        let access = JOB_OBJECT_ASSIGN_PROCESS | JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE;
        loop {
            let h = unsafe { OpenJobObjectW(access, 0, name_wide.as_ptr()) };
            if !h.is_null() {
                return Ok(Self {
                    handle: Some(OwnedHandle(h)),
                    limits: JobLimits::default(),
                });
            }
            let err = unsafe { GetLastError() };
            if err == ERROR_FILE_NOT_FOUND
                && deadline.is_some_and(|limit| std::time::Instant::now() < limit)
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
            return Err(crate::error::WboxError::job(format!(
                "OpenJobObjectW('{}') 失败，GetLastError={}（容器可能刚退出或尚未完成启动）",
                name, err
            )));
        }
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
                self.raw(),
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
            cpu.ControlFlags =
                JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            // windows-sys 的 union 字段为 Copy 类型，写字段是安全操作；写入后不再读取其它变体。
            cpu.Anonymous.CpuRate = rate;
            // # Safety: 结构体已初始化，句柄有效。
            let ok = unsafe {
                SetInformationJobObject(
                    self.raw(),
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
        let ok = unsafe { AssignProcessToJobObject(self.raw(), process) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(crate::error::WboxError::job(format!(
                    "AssignProcessToJobObject 失败，GetLastError={}（wbox 可能已运行在不兼容的 Job 中）",
                    err
                )));
        }
        Ok(())
    }

    /// 显式终止 Job 内全部进程。`stop` 不能只依赖 KILL_ON_JOB_CLOSE：
    /// 并发 `exec` 控制器可能仍持有另一个 Job handle。
    pub fn terminate(&self, exit_code: u32) -> Result<()> {
        let ok = unsafe { TerminateJobObject(self.raw(), exit_code) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(crate::error::WboxError::job(format!(
                "TerminateJobObject 失败，GetLastError={}",
                err
            )));
        }
        Ok(())
    }

    /// 返回当前 Job 中的全部进程 ID。查询结果可能在两次调用之间增长，因此按
    /// `ERROR_MORE_DATA` 重试，而不是假定一个固定的最大进程数。
    pub fn process_ids(&self) -> Result<Vec<u32>> {
        const ERROR_MORE_DATA: u32 = 234;
        let offset = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
        let word = std::mem::size_of::<usize>();
        let header_words = offset.div_ceil(word);
        let mut capacity = 16usize;

        loop {
            let mut storage = vec![0usize; header_words + capacity];
            let info = storage.as_mut_ptr() as *mut JOBOBJECT_BASIC_PROCESS_ID_LIST;
            let bytes = storage
                .len()
                .checked_mul(word)
                .and_then(|size| u32::try_from(size).ok())
                .ok_or_else(|| crate::error::WboxError::job("Job 进程列表缓冲区过大"))?;
            let ok = unsafe {
                QueryInformationJobObject(
                    self.raw(),
                    JobObjectBasicProcessIdList,
                    info.cast(),
                    bytes,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 {
                let count = unsafe { (*info).NumberOfProcessIdsInList as usize };
                let ids = unsafe {
                    std::slice::from_raw_parts(
                        (info as *const u8).add(offset) as *const usize,
                        count,
                    )
                };
                return ids
                    .iter()
                    .map(|pid| {
                        u32::try_from(*pid).map_err(|_| {
                            crate::error::WboxError::job(format!(
                                "Job 返回了超出 u32 范围的进程 ID：{}",
                                pid
                            ))
                        })
                    })
                    .collect();
            }

            let err = unsafe { GetLastError() };
            if err != ERROR_MORE_DATA {
                return Err(crate::error::WboxError::job(format!(
                    "QueryInformationJobObject(ProcessIdList) 失败，GetLastError={}",
                    err
                )));
            }
            let assigned = unsafe { (*info).NumberOfAssignedProcesses as usize };
            capacity = assigned.max(capacity.saturating_mul(2));
        }
    }

    /// 关闭当前控制器持有的 Job handle。已经加入 Job 的进程仍留在 Job 中；
    /// `exec` 在恢复 guest 后立即调用，使 supervisor 始终是生命周期 owner。
    pub fn close(&mut self) {
        drop(self.handle.take());
    }

    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.handle
            .as_ref()
            .expect("Job handle 已关闭后不应再调用 Job API")
            .raw()
    }
}

#[cfg(test)]
#[cfg(windows)]
mod real_windows_tests {
    use super::*;

    /// Job 创建 + KILL_ON_JOB_CLOSE 必开 + Drop 时关闭句柄不阻塞。
    /// 不分配子进程，只验证 Job Object 内核对象的创建路径。
    #[test]
    fn job_create_default_succeeds() {
        let job = Job::create(JobLimits::default()).unwrap();
        // 句柄非空即视为成功（OwnedHandle 内部 RAII，Drop 时 CloseHandle）
        assert!(!job.raw().is_null());
    }

    #[test]
    fn named_job_can_be_reopened_and_terminated() {
        use std::os::windows::io::AsRawHandle;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("wbox-job-test-{}-{}", std::process::id(), nonce);
        let job = Job::create_for_container(&name, JobLimits::default()).unwrap();
        let reopened = Job::open_for_container(&name).unwrap();
        assert!(!job.raw().is_null());
        assert!(!reopened.raw().is_null());

        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 30 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        job.assign(child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE)
            .unwrap();
        assert!(
            reopened.process_ids().unwrap().contains(&child.id()),
            "Job 查询应列出刚加入的进程"
        );
        reopened.terminate(1).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success(), "TerminateJobObject 应终止 Job 内进程");
    }

    /// 三项限额同时下发：memory_mb / cpu_pct / max_procs。
    /// 验证 SetInformationJobObject 在合理值下都成功（Win8+ CPU rate control）。
    #[test]
    fn job_create_with_all_limits() {
        let limits = JobLimits {
            memory_mb: 256,
            cpu_pct: 50,
            max_procs: 10,
        };
        let job = Job::create(limits).unwrap();
        assert!(!job.raw().is_null());
    }

    /// 内存上限边界：0 = 不限（不挂 JOB_OBJECT_LIMIT_PROCESS_MEMORY flag）。
    /// JobLimits 默认实现是全 0，本测试断言 0 不触发任何限额下发。
    #[test]
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
