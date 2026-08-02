//! Windows containment product policy over `agenterm-platform` native mechanisms.
//!
//! The shared crate owns Job Object handles, exact assignment, native limits,
//! membership, member snapshots, and termination. wbox owns the object name,
//! retry window, required kill-on-close posture, limit units, and lifecycle use.

use std::os::windows::io::BorrowedHandle;

use agenterm_platform::{
    process_containment::{
        ProcessContainment, ProcessContainmentError, ProcessContainmentErrorKind,
        ProcessContainmentLimits, ProcessContainmentOptions,
    },
    process_reference::ProcessReference,
};

use crate::{backend::Limits as JobLimits, error::Result};

const JOB_NAME_PREFIX: &str = r"Local\wbox.job.";

/// Container identity to the exact native containment-object name.
pub fn name_for_container(container_name: &str) -> String {
    format!("{JOB_NAME_PREFIX}{container_name}")
}

pub struct Job {
    containment: ProcessContainment,
}

impl Job {
    #[cfg(test)]
    pub fn create(limits: JobLimits) -> Result<Self> {
        Self::create_impl(limits, None)
    }

    /// Create one exclusive named containment generation for a container.
    pub fn create_for_container(container_name: &str, limits: JobLimits) -> Result<Self> {
        Self::create_impl(limits, Some(name_for_container(container_name)))
    }

    fn create_impl(limits: JobLimits, name: Option<String>) -> Result<Self> {
        let options = containment_options(limits)?;
        match ProcessContainment::create(name.as_deref(), options) {
            Ok(containment) => Ok(Self { containment }),
            Err(error) if error.kind() == ProcessContainmentErrorKind::AlreadyExists => {
                Err(crate::error::WboxError::job(format!(
                    "容器 '{}' 的命名 Job 仍存在，拒绝复用上一代隔离单元",
                    name.unwrap_or_default()
                )))
            }
            Err(error) => Err(job_error("创建 Windows Job", error)),
        }
    }

    #[cfg(test)]
    pub fn open_for_container(container_name: &str) -> Result<Self> {
        Self::open_for_container_until(container_name, None)
    }

    /// Wait through the supervisor's record-to-Job publication window.
    pub fn wait_for_container(container_name: &str, timeout: std::time::Duration) -> Result<Self> {
        Self::open_for_container_until(container_name, Some(std::time::Instant::now() + timeout))
    }

    fn open_for_container_until(
        container_name: &str,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        let name = name_for_container(container_name);
        loop {
            match ProcessContainment::open(&name) {
                Ok(containment) => return Ok(Self { containment }),
                Err(error)
                    if error.kind() == ProcessContainmentErrorKind::NotFound
                        && deadline.is_some_and(|limit| std::time::Instant::now() < limit) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => {
                    return Err(crate::error::WboxError::job(format!(
                        "打开容器 Job '{}' 失败（容器可能刚退出或尚未完成启动）：{}",
                        name, error
                    )));
                }
            }
        }
    }

    /// Assign an already-open exact process object; never reopen it by PID.
    pub fn assign(&self, process: BorrowedHandle<'_>) -> Result<()> {
        let process = ProcessReference::duplicate_from(process).map_err(|error| {
            crate::error::WboxError::job(format!("保留待分配进程对象失败：{error}"))
        })?;
        self.containment
            .assign(&process)
            .map_err(|error| job_error("把进程分配进 Windows Job", error))
    }

    pub fn contains(&self, process: &ProcessReference) -> Result<bool> {
        self.containment
            .contains(process)
            .map_err(|error| job_error("读取 Windows Job 成员身份", error))
    }

    pub fn terminate(&self, exit_code: u32) -> Result<()> {
        self.containment
            .terminate(exit_code)
            .map_err(|error| job_error("终止 Windows Job", error))
    }

    pub fn process_ids(&self) -> Result<Vec<u32>> {
        self.containment
            .process_ids()
            .map_err(|error| job_error("枚举 Windows Job 进程", error))
    }

    /// Release this controller. The supervisor remains the product lifecycle owner.
    pub fn close(&mut self) {
        self.containment.close();
    }
}

fn containment_options(limits: JobLimits) -> Result<ProcessContainmentOptions> {
    Ok(ProcessContainmentOptions {
        terminate_on_last_close: true,
        allow_breakaway: false,
        limits: ProcessContainmentLimits {
            memory_bytes: limits.memory_limit_bytes()?.map(|bytes| bytes as u64),
            cpu_rate_hundredths: limits.cpu_rate(),
            active_processes: (limits.max_procs > 0).then_some(limits.max_procs),
        },
    })
}

fn job_error(context: &str, error: ProcessContainmentError) -> crate::error::WboxError {
    crate::error::WboxError::job(format!("{context}失败：{error}"))
}

#[cfg(test)]
mod real_windows_tests {
    use super::*;
    use std::os::windows::io::AsHandle as _;

    fn unique_name() -> String {
        format!(
            "wbox-job-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn job_create_default_succeeds() {
        assert!(Job::create(JobLimits::default())
            .unwrap()
            .process_ids()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn named_job_can_be_reopened_and_terminated() {
        let name = unique_name();
        let job = Job::create_for_container(&name, JobLimits::default()).unwrap();
        let reopened = Job::open_for_container(&name).unwrap();
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 30 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        job.assign(child.as_handle()).unwrap();
        assert!(reopened.process_ids().unwrap().contains(&child.id()));
        reopened.terminate(1).unwrap();
        assert!(!child.wait().unwrap().success());
    }

    #[test]
    fn job_create_with_all_limits() {
        Job::create(JobLimits {
            memory_mb: 256,
            cpu_pct: 50,
            max_procs: 10,
        })
        .unwrap();
    }

    #[test]
    fn zero_limits_are_absent_and_product_units_are_normalized() {
        assert_eq!(
            containment_options(JobLimits::default()).unwrap().limits,
            ProcessContainmentLimits::default()
        );
        assert_eq!(
            containment_options(JobLimits {
                memory_mb: 128,
                cpu_pct: 50,
                max_procs: 10,
            })
            .unwrap()
            .limits,
            ProcessContainmentLimits {
                memory_bytes: Some(128 * 1024 * 1024),
                cpu_rate_hundredths: Some(5_000),
                active_processes: Some(10),
            }
        );
    }

    #[test]
    fn job_huge_memory_limit_is_caught_as_overflow() {
        let result = Job::create(JobLimits {
            memory_mb: u64::MAX,
            cpu_pct: 0,
            max_procs: 0,
        });
        let error = match result {
            Ok(_) => panic!("memory conversion must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("溢出"));
    }
}
