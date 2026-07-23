//! 进程启动编排：AppContainer attribute-list 路径。
//!
//! 关键取舍（对应 SPEC §2-3）：
//! 采用 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` + `CreateProcessW`
//! （EXTENDED_STARTUPINFO_PRESENT），**而不是** `CreateProcessAsUserW`：
//! - attribute-list 路径由内核在创建时派生 AppContainer 子令牌，
//!   对当前用户**不需要 SeAssignPrimaryTokenPrivilege**，普通用户可用；
//! - CreateProcessAsUser/CreateProcessWithToken 需要该特权（或服务上下文），
//!   与"portable、默认非管理员"的定位冲突。
//! 代价：SECURITY_CAPABILITIES 路径无法显式指定完整性级别——
//! 但 AppContainer 派生令牌的 IL 恒为 Low（内核强制），满足 SPEC 的 Low IL 要求。
//!
//! 流程：挂起创建 → AssignProcessToJobObject → 恢复主线程 → 等待 → 转发退出码。

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Security::{SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, TerminateProcess,
    InitializeProcThreadAttributeList, ResumeThread, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    STARTUPINFOEXW, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
};

use crate::error::{ErrKind, Result};
use crate::token::{to_wide, AppContainerProfile, CapabilitySid, OwnedHandle};

/// 启动子进程并等待退出，返回子进程退出码。
///
/// - `profile`：AppContainer profile（提供 SID）
/// - `capabilities`：授予的 capability 列表
/// - `cmdline`：完整命令行（含程序名与参数）
/// - `workdir`：容器工作目录（"镜像根"）
/// - `job`：创建后立即把子进程分配进该 Job
pub fn run_container(
    profile: &AppContainerProfile,
    capabilities: &[CapabilitySid],
    cmdline: &str,
    workdir: &str,
    job: &crate::job::Job,
) -> Result<u32> {
    let mut cmd_wide = to_wide(cmdline); // CreateProcessW 要求可写缓冲区
    let workdir_wide = to_wide(workdir);

    // ---- capability 属性数组（借用，生命周期覆盖整个创建过程）----
    let mut cap_attrs: Vec<SID_AND_ATTRIBUTES> = capabilities
        .iter()
        .map(|c| SID_AND_ATTRIBUTES {
            Sid: c.sid,
            Attributes: 0,
        })
        .collect();

    let mut sec_caps = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid(),
        Capabilities: if cap_attrs.is_empty() {
            std::ptr::null_mut()
        } else {
            cap_attrs.as_mut_ptr()
        },
        CapabilityCount: cap_attrs.len() as u32,
        Reserved: 0,
    };

    // ---- 初始化 attribute list（两次调用：先取大小，再填充）----
    let mut attr_list_size: usize = 0;
    // # Safety: 第一次调用只查询所需大小，传 null 列表指针，预期失败。
    unsafe {
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_list_size);
    }
    if attr_list_size == 0 {
        let err = unsafe { GetLastError() };
        return Err(crate::error::WboxError::new(
            ErrKind::Spawn,
            anyhow::anyhow!("InitializeProcThreadAttributeList(查询大小) 失败，GetLastError={}", err),
        ));
    }
    // 用 u64 对齐的缓冲区承载 attribute list。
    let mut attr_buf = vec![0u64; (attr_list_size + 7) / 8];
    let attr_list: LPPROC_THREAD_ATTRIBUTE_LIST =
        attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
    // # Safety: attr_buf 大小/对齐满足要求，attr_list_size 来自上一步查询。
    let ok = unsafe { InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_list_size) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(crate::error::WboxError::new(
            ErrKind::Spawn,
            anyhow::anyhow!("InitializeProcThreadAttributeList 失败，GetLastError={}", err),
        ));
    }
    // RAII：离开作用域时销毁 attribute list。
    let _attr_guard = AttrListGuard(attr_list);

    // # Safety:
    // - attr_list 已初始化且容量 >= 1 个属性；
    // - sec_caps 及其引用的 SID / capability 数组在 CreateProcessW 返回前保持有效
    //   （它们都在本函数栈上/由调用方持有的 profile 中）。
    let ok = unsafe {
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            &mut sec_caps as *mut _ as *const core::ffi::c_void,
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(crate::error::WboxError::new(
            ErrKind::Spawn,
            anyhow::anyhow!("UpdateProcThreadAttribute(SECURITY_CAPABILITIES) 失败，GetLastError={}", err),
        ));
    }

    // ---- STARTUPINFOEXW：stdio 直接继承当前控制台 ----
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.lpAttributeList = attr_list;

    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // CREATE_SUSPENDED：先创建挂起的进程，分配进 Job 后再放行，
    // 防止子进程在入 Job 之前执行代码（逃逸窗口）。
    let flags = CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;

    // # Safety:
    // - cmd_wide 为可写 NUL 结尾缓冲区（CreateProcessW 会就地修改）；
    // - si/pi 为有效栈上结构；attribute list 在调用期间有效；
    // - 环境块传 null（继承当前进程环境），stdout/stdin/stderr 继承。
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(), // 从命令行解析程序名
            cmd_wide.as_mut_ptr(),
            std::ptr::null(), // 进程安全属性（默认）
            std::ptr::null(), // 线程安全属性（默认）
            0,                // 不继承额外句柄（stdio 走控制台自动继承）
            flags,
            std::ptr::null(), // 继承当前环境
            workdir_wide.as_ptr(),
            &si.StartupInfo as *const _,
            &mut pi,
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(crate::error::WboxError::new(
            ErrKind::Spawn,
            anyhow::anyhow!(
                "CreateProcessW 失败，GetLastError={}（2=找不到程序；请确认命令在 --workdir 或 PATH 中可见）",
                err
            ),
        ));
    }
    let process = OwnedHandle(pi.hProcess);
    let thread = OwnedHandle(pi.hThread);

    // ---- 立即入 Job，然后放行主线程 ----
    if let Err(e) = job.assign(process.raw()) {
        // 子进程是 CREATE_SUSPENDED 且未入 Job：若直接返回，KILL_ON_JOB_CLOSE
        // 收割不到它，会留下一个永久挂起的孤儿进程。先主动终止再返回错误。
        // # Safety: 进程句柄有效；子进程尚未执行用户代码，终止是安全的。
        unsafe { TerminateProcess(process.raw(), 1) };
        return Err(e);
    }
    // # Safety: 线程句柄有效，线程处于挂起状态（CREATE_SUSPENDED）。
    let prev = unsafe { ResumeThread(thread.raw()) };
    if prev == u32::MAX {
        let err = unsafe { GetLastError() };
        // 进程已入 Job，KILL_ON_JOB_CLOSE 会负责收割；直接报错即可。
        return Err(crate::error::WboxError::new(
            ErrKind::Spawn,
            anyhow::anyhow!("ResumeThread 失败，GetLastError={}", err),
        ));
    }

    // ---- 等待退出并转发退出码 ----
    // # Safety: 进程句柄有效，INFINITE 等待进程退出。
    unsafe { WaitForSingleObject(process.raw(), u32::MAX /* INFINITE */) };
    let mut code: u32 = 0;
    // # Safety: 进程句柄有效且进程已退出，code 为有效输出指针。
    let ok = unsafe { GetExitCodeProcess(process.raw(), &mut code) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(crate::error::WboxError::new(
            ErrKind::Spawn,
            anyhow::anyhow!("GetExitCodeProcess 失败，GetLastError={}", err),
        ));
    }
    Ok(code)
}

/// attribute list 的 RAII 销毁器。
struct AttrListGuard(LPPROC_THREAD_ATTRIBUTE_LIST);

impl Drop for AttrListGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // # Safety: 列表由 InitializeProcThreadAttributeList 初始化，只销毁一次；
            // 底层内存由调用处的 attr_buf 持有，此处仅做内核侧清理。
            unsafe { DeleteProcThreadAttributeList(self.0) };
        }
    }
}

/// 组装传给 CreateProcessW 的命令行：程序名加引号，其余参数原样拼接。
///
/// 注意（已知限制）：参数转义为简化规则（仅处理空格/制表符/内嵌引号，
/// 未完整实现 CommandLineToArgvW 的反斜杠规则——例如以 `\` 结尾的参数
/// 或 `\\\"` 序列）。对含复杂反斜杠+引号组合的参数可能与标准解析结果不同；
/// 常见路径/参数场景可用，v1 接受该简化。
pub fn build_cmdline(cmd: &[String]) -> Result<String> {
    if cmd.is_empty() {
        return Err(crate::error::WboxError::args("缺少要执行的命令（-- <CMD> [ARGS...]）"));
    }
    let mut s = String::new();
    s.push('"');
    s.push_str(&cmd[0]);
    s.push('"');
    for arg in &cmd[1..] {
        s.push(' ');
        // 含空格或引号的参数加引号并转义内嵌引号（简化版 CommandLineToArgvW 规则）。
        if arg.contains(' ') || arg.contains('"') || arg.contains('\t') || arg.is_empty() {
            s.push('"');
            s.push_str(&arg.replace('"', "\\\""));
            s.push('"');
        } else {
            s.push_str(arg);
        }
    }
    Ok(s)
}
