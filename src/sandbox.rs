//! 进程启动编排：AppContainer attribute-list 路径。
//!
//! 关键取舍（对应 SPEC §2-3）：
//! 采用 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` + `CreateProcessW`
//! （EXTENDED_STARTUPINFO_PRESENT），**而不是** `CreateProcessAsUserW`：
//! - attribute-list 路径由内核在创建时派生 AppContainer 子令牌，
//!   对当前用户**不需要 SeAssignPrimaryTokenPrivilege**，普通用户可用；
//! - CreateProcessAsUser/CreateProcessWithToken 需要该特权（或服务上下文），
//!   与"portable、默认非管理员"的定位冲突。
//!   代价：SECURITY_CAPABILITIES 路径无法显式指定完整性级别——
//!   但 AppContainer 派生令牌的 IL 恒为 Low（内核强制），满足 SPEC 的 Low IL 要求。
//!
//! 流程：挂起创建 → AssignProcessToJobObject → 恢复主线程 → 等待 → 转发退出码。

use std::os::windows::io::{BorrowedHandle, RawHandle};
use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
use windows_sys::Win32::Security::{SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTUPINFOEXW,
};

use crate::error::Result;
use crate::token::{to_wide, AppContainerProfile, CapabilitySid, OwnedHandle};

/// 启动子进程并等待退出，返回子进程退出码。
///
/// - `profile`：AppContainer profile（提供 SID）
/// - `capabilities`：授予的 capability 列表
/// - `cmdline`：完整命令行（含程序名与参数）
/// - `workdir`：容器工作目录（"镜像根"）
/// - `job`：创建后立即把子进程分配进该 Job
/// - `env`：子进程的**完整显式环境**（白名单，见 backend/env.rs）；
///   经 lpEnvironment 传递，不再继承 wbox 宿主环境（H6）
#[cfg_attr(not(test), allow(dead_code))]
pub fn run_container(
    profile: &AppContainerProfile,
    capabilities: &[CapabilitySid],
    cmdline: &str,
    workdir: &str,
    job: &mut crate::job::Job,
    env: &[(String, String)],
) -> Result<u32> {
    run_container_with_handles(profile, capabilities, cmdline, workdir, job, env, &[])
}

/// 与 [`run_container`] 相同，并只向子进程继承 `handles` 中列出的句柄。
///
/// 句柄会在 `CreateProcessW` 的短暂窗口内设置 `HANDLE_FLAG_INHERIT`，随后按原值
/// 恢复；`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 保证父进程的其他可继承句柄不泄漏。
#[cfg_attr(not(test), allow(dead_code))]
pub fn run_container_with_handles(
    profile: &AppContainerProfile,
    capabilities: &[CapabilitySid],
    cmdline: &str,
    workdir: &str,
    job: &mut crate::job::Job,
    env: &[(String, String)],
    handles: &[HANDLE],
) -> Result<u32> {
    run_container_with_handles_and_created_hook(
        profile,
        capabilities,
        cmdline,
        workdir,
        job,
        env,
        handles,
        |_, _| Ok(()),
    )
}

/// 与 [`run_container_with_handles`] 相同，并在子进程已加入 Job、但主线程仍挂起时
/// 调用一次 `on_created`。回调失败会终止并等待尚未执行用户代码的子进程。
#[allow(clippy::too_many_arguments)] // 与 run_container_with_handles 参数保持一致，另加一个 hook
#[cfg_attr(not(test), allow(dead_code))]
pub fn run_container_with_handles_and_created_hook<F>(
    profile: &AppContainerProfile,
    capabilities: &[CapabilitySid],
    cmdline: &str,
    workdir: &str,
    job: &mut crate::job::Job,
    env: &[(String, String)],
    handles: &[HANDLE],
    on_created: F,
) -> Result<u32>
where
    F: FnOnce(HANDLE, &mut crate::job::Job) -> Result<()>,
{
    run_container_with_lifecycle_hooks_and_handles(
        profile,
        capabilities,
        cmdline,
        workdir,
        job,
        env,
        handles,
        on_created,
        |_| {},
    )
}

/// 与 [`run_container`] 相同，但在子进程已经加入 Job 且成功恢复后调用一次
/// `on_started`。Windows `exec` 用它释放 runstate 的短期操作锁：锁必须覆盖
/// “确认同一代容器 → OpenJobObject → 挂起创建并入 Job”，但不能覆盖等待阶段。
pub fn run_container_with_start_hook<F>(
    profile: &AppContainerProfile,
    capabilities: &[CapabilitySid],
    cmdline: &str,
    workdir: &str,
    job: &mut crate::job::Job,
    env: &[(String, String)],
    on_started: F,
) -> Result<u32>
where
    F: FnOnce(&mut crate::job::Job),
{
    run_container_with_lifecycle_hooks_and_handles(
        profile,
        capabilities,
        cmdline,
        workdir,
        job,
        env,
        &[],
        |_, _| Ok(()),
        on_started,
    )
}

#[allow(clippy::too_many_arguments)] // 保持两个公开启动入口的参数顺序一致
fn run_container_with_lifecycle_hooks_and_handles<C, S>(
    profile: &AppContainerProfile,
    capabilities: &[CapabilitySid],
    cmdline: &str,
    workdir: &str,
    job: &mut crate::job::Job,
    env: &[(String, String)],
    handles: &[HANDLE],
    on_created: C,
    on_started: S,
) -> Result<u32>
where
    C: FnOnce(HANDLE, &mut crate::job::Job) -> Result<()>,
    S: FnOnce(&mut crate::job::Job),
{
    let mut cmd_wide = to_wide(cmdline); // CreateProcessW 要求可写缓冲区
    let workdir_wide = to_wide(workdir);
    let mut inherited_handles = handles.to_vec();
    inherited_handles.sort_unstable_by_key(|handle| *handle as usize);
    inherited_handles.dedup();
    // ---- capability 属性数组（借用，生命周期覆盖整个创建过程）----
    let mut cap_attrs: Vec<SID_AND_ATTRIBUTES> = capabilities
        .iter()
        .map(CapabilitySid::enabled_attributes)
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
    let attribute_count = if inherited_handles.is_empty() { 1 } else { 2 };
    let mut attr_list_size: usize = 0;
    // # Safety: 第一次调用只查询所需大小，传 null 列表指针，预期失败。
    unsafe {
        InitializeProcThreadAttributeList(
            std::ptr::null_mut(),
            attribute_count,
            0,
            &mut attr_list_size,
        );
    }
    if attr_list_size == 0 {
        let err = unsafe { GetLastError() };
        return Err(crate::error::WboxError::spawn(format!(
            "InitializeProcThreadAttributeList(查询大小) 失败，GetLastError={}",
            err
        )));
    }
    // 用 u64 对齐的缓冲区承载 attribute list。
    let mut attr_buf = vec![0u64; attr_list_size.div_ceil(8)];
    let attr_list: LPPROC_THREAD_ATTRIBUTE_LIST =
        attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
    // # Safety: attr_buf 大小/对齐满足要求，attr_list_size 来自上一步查询。
    let ok = unsafe {
        InitializeProcThreadAttributeList(attr_list, attribute_count, 0, &mut attr_list_size)
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(crate::error::WboxError::spawn(format!(
            "InitializeProcThreadAttributeList 失败，GetLastError={}",
            err
        )));
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
        return Err(crate::error::WboxError::spawn(format!(
            "UpdateProcThreadAttribute(SECURITY_CAPABILITIES) 失败，GetLastError={}",
            err
        )));
    }

    if !inherited_handles.is_empty() {
        // # Safety: attribute list 容量为 2；数组在 CreateProcessW 返回前保持有效；
        // 每个句柄会由下方 platform transaction 在创建窗口内临时设为 inheritable。
        let ok = unsafe {
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherited_handles.as_mut_ptr() as *const core::ffi::c_void,
                inherited_handles.len() * std::mem::size_of::<HANDLE>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(crate::error::WboxError::spawn(format!(
                "UpdateProcThreadAttribute(HANDLE_LIST) 失败，GetLastError={}",
                err
            )));
        }
    }

    // ---- STARTUPINFOEXW：stdio 直接继承当前控制台 ----
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.lpAttributeList = attr_list;

    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // CREATE_SUSPENDED：先创建挂起的进程，分配进 Job 后再放行，
    // 防止子进程在入 Job 之前执行代码（逃逸窗口）。
    let flags = CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;

    // 显式环境块（"KEY=VAL\0...\0\0" 的 UTF-16 形式）：白名单环境
    // 直接交给子进程，杜绝宿主环境（含 WBOX_REGISTRY_* 等机密）直通。
    let env_wide = build_env_block(env);

    // # Safety:
    // - cmd_wide 为可写 NUL 结尾缓冲区（CreateProcessW 会就地修改）；
    // - si/pi 为有效栈上结构；attribute list 在调用期间有效；
    // - env_wide 为合法的双 NUL 结尾环境块，调用期间存活；
    //   stdout/stdin/stderr 继承。
    let borrowed_handles = inherited_handles
        .iter()
        .map(|handle| unsafe { BorrowedHandle::borrow_raw(*handle as RawHandle) })
        .collect::<Vec<_>>();
    let spawn_result =
        agenterm_platform::process_spawn::with_inheritable_handles(&borrowed_handles, || unsafe {
            let ok = CreateProcessW(
                std::ptr::null(), // 从命令行解析程序名
                cmd_wide.as_mut_ptr(),
                std::ptr::null(), // 进程安全属性（默认）
                std::ptr::null(), // 线程安全属性（默认）
                i32::from(!inherited_handles.is_empty()),
                flags,
                env_wide.as_ptr() as *const core::ffi::c_void, // 显式环境块
                workdir_wide.as_ptr(),
                &si.StartupInfo as *const _,
                &mut pi,
            );
            // The transaction restores source HANDLE flags after this callback and those
            // Win32 calls may replace the thread's last-error value. Capture CreateProcessW's
            // error in its own API boundary instead of reading it after restoration.
            let error = if ok == 0 { GetLastError() } else { 0 };
            (ok, error)
        });
    let (ok, create_error) = match spawn_result {
        Ok(result) => result,
        Err(error) => {
            // The callback may have created a suspended child before restoring a source
            // handle failed. Take ownership of both returned handles and reap that child;
            // it has not entered the Job yet, so KILL_ON_JOB_CLOSE cannot cover this path.
            if !pi.hProcess.is_null() {
                let process = OwnedHandle(pi.hProcess);
                let _thread = OwnedHandle(pi.hThread);
                // # Safety: CreateProcessW populated a live process handle and the primary
                // thread is still suspended, so no guest code has run.
                unsafe { TerminateProcess(process.raw(), 1) };
                let _ = wait_for_process_exit(process.raw());
            }
            return Err(crate::error::WboxError::spawn(format!(
                "准备显式继承 HANDLE 失败：{error}"
            )));
        }
    };
    if ok == 0 {
        let err = create_error;
        let hint = match err {
            2 => "（找不到程序；请确认命令在 --workdir 或 PATH 中可见）",
            203 => "（环境变量未找到；请确认传入的环境块格式正确）",
            _ => "",
        };
        return Err(crate::error::WboxError::spawn(format!(
            "CreateProcessW 失败，GetLastError={}{}",
            err, hint
        )));
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
    if let Err(e) = on_created(process.raw(), job) {
        // 子进程仍挂起；等待终止完成，避免 broker 初始化失败后留下孤儿。
        unsafe { TerminateProcess(process.raw(), 1) };
        let _ = wait_for_process_exit(process.raw());
        return Err(e);
    }
    // # Safety: 线程句柄有效，线程处于挂起状态（CREATE_SUSPENDED）。
    let prev = unsafe { ResumeThread(thread.raw()) };
    if prev == u32::MAX {
        let err = unsafe { GetLastError() };
        // 进程已入 Job，KILL_ON_JOB_CLOSE 会负责收割；直接报错即可。
        return Err(crate::error::WboxError::spawn(format!(
            "ResumeThread 失败，GetLastError={}",
            err
        )));
    }
    on_started(job);

    // ---- 等待退出并转发退出码 ----
    wait_for_process_exit(process.raw())?;
    let mut code: u32 = 0;
    // # Safety: 进程句柄有效且进程已退出，code 为有效输出指针。
    let ok = unsafe { GetExitCodeProcess(process.raw(), &mut code) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(crate::error::WboxError::spawn(format!(
            "GetExitCodeProcess 失败，GetLastError={}",
            err
        )));
    }
    Ok(code)
}

fn wait_for_process_exit(process: HANDLE) -> Result<()> {
    use std::os::windows::io::{BorrowedHandle, RawHandle};

    let process = unsafe { BorrowedHandle::borrow_raw(process as RawHandle) };
    match agenterm_platform::process_reference::wait_handle(process, None).map_err(|error| {
        crate::error::WboxError::spawn(format!("等待 sandbox 进程退出失败：{error}"))
    })? {
        agenterm_platform::process_reference::ProcessWait::Exited => Ok(()),
        agenterm_platform::process_reference::ProcessWait::TimedOut => {
            Err(crate::error::WboxError::spawn("sandbox 无限等待意外超时"))
        }
    }
}

/// 构造 CreateProcessW lpEnvironment 所需的 UTF-16 环境块
/// （`KEY=VAL\0...KEY=VAL\0\0`）。非法键（空/含 '='）防御性跳过。
fn build_env_block(env: &[(String, String)]) -> Vec<u16> {
    let mut s = String::new();
    let mut has_entry = false;
    for (k, v) in env {
        if k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
            continue;
        }
        has_entry = true;
        s.push_str(k);
        s.push('=');
        s.push_str(v);
        s.push('\0');
    }
    // 非空块已有最后一项的 NUL；空块也必须显式形成两个 NUL。
    if !has_entry {
        s.push('\0');
    }
    s.push('\0');
    s.encode_utf16().collect()
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

fn push_cmdline_arg(out: &mut String, arg: &str, force_quote: bool) {
    let quote =
        force_quote || arg.is_empty() || arg.chars().any(|c| c == ' ' || c == '\t' || c == '"');
    if !quote {
        out.push_str(arg);
        return;
    }

    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        for _ in 0..backslashes {
            out.push('\\');
        }
        if c == '"' {
            // A quote needs 2n+1 backslashes: n preserve the original
            // backslashes and n+1 escape the quote.
            for _ in 0..backslashes {
                out.push('\\');
            }
            out.push('\\');
        }
        out.push(c);
        backslashes = 0;
    }
    // Before the closing quote, 2n backslashes decode to n literal ones.
    for _ in 0..backslashes {
        out.push('\\');
        out.push('\\');
    }
    out.push('"');
}

/// 组装传给 CreateProcessW 的命令行，按 Windows CRT /
/// CommandLineToArgvW 规则无损编码每个参数。
pub fn build_cmdline(cmd: &[String]) -> Result<String> {
    if cmd.is_empty() {
        return Err(crate::error::WboxError::args(
            "缺少要执行的命令（-- <CMD> [ARGS...]）",
        ));
    }
    if cmd.iter().any(|arg| arg.contains('\0')) {
        return Err(crate::error::WboxError::args("命令或参数不能包含 NUL 字符"));
    }
    let mut s = String::new();
    push_cmdline_arg(&mut s, &cmd[0], true);
    for arg in &cmd[1..] {
        s.push(' ');
        push_cmdline_arg(&mut s, arg, false);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- build_cmdline：程序名总是加引号，其余参数按需加引号 ----

    #[test]
    fn build_cmdline_empty_is_error() {
        assert!(build_cmdline(&[]).is_err());
    }

    #[test]
    fn build_cmdline_single_program_quoted() {
        // 即使程序名无空格也加引号（与 cmd.exe 风格一致）
        assert_eq!(
            build_cmdline(&["cmd.exe".to_string()]).unwrap(),
            r#""cmd.exe""#
        );
    }

    #[test]
    fn build_cmdline_program_with_spaces_in_name() {
        // 程序名本身含空格：当前实现只在外围加引号，不转义内部字符
        // （记录现状：用户传 "C:\Program Files\app.exe" 时直接得 '"C:\Program Files\app.exe"'）
        let s = build_cmdline(&["C:\\Program Files\\app.exe".to_string()]).unwrap();
        assert_eq!(s, r#""C:\Program Files\app.exe""#);
    }

    #[test]
    fn build_cmdline_plain_args_unquoted() {
        let s = build_cmdline(&[
            "cmd.exe".to_string(),
            "/c".to_string(),
            "echo".to_string(),
            "hello".to_string(),
        ])
        .unwrap();
        assert_eq!(s, r#""cmd.exe" /c echo hello"#);
    }

    #[test]
    fn build_cmdline_arg_with_space_is_quoted() {
        let s = build_cmdline(&[
            "cmd.exe".to_string(),
            "/c".to_string(),
            "echo hello world".to_string(),
        ])
        .unwrap();
        assert_eq!(s, r#""cmd.exe" /c "echo hello world""#);
    }

    #[test]
    fn build_cmdline_arg_with_tab_is_quoted() {
        let s = build_cmdline(&["cmd.exe".to_string(), "a\tb".to_string()]).unwrap();
        assert_eq!(s, r#""cmd.exe" "a	b""#);
    }

    #[test]
    fn build_cmdline_empty_arg_is_quoted() {
        // 空参数必须加引号，���则 CommandLineToArgvW 会丢失它
        let s = build_cmdline(&["cmd.exe".to_string(), "".to_string(), "x".to_string()]).unwrap();
        assert_eq!(s, r#""cmd.exe" "" x"#);
    }

    #[test]
    fn build_cmdline_embedded_quote_is_escaped() {
        // 内嵌引号 → \" 转义 + 整体加引号
        let s = build_cmdline(&["cmd.exe".to_string(), r#"he said "hi""#.to_string()]).unwrap();
        assert_eq!(s, r#""cmd.exe" "he said \"hi\"""#);
    }

    #[test]
    fn build_cmdline_doubles_trailing_backslashes_in_quoted_arg() {
        let s =
            build_cmdline(&["cmd.exe".to_string(), r"C:\path with space\".to_string()]).unwrap();
        assert_eq!(s, r#""cmd.exe" "C:\path with space\\""#);
    }

    #[test]
    fn build_cmdline_escapes_backslash_quote_sequence() {
        let s = build_cmdline(&["cmd.exe".to_string(), r#"x\"y"#.to_string()]).unwrap();
        assert_eq!(s, r#""cmd.exe" "x\\\"y""#);
    }

    #[test]
    fn build_cmdline_rejects_nul() {
        let err = build_cmdline(&["cmd.exe".to_string(), "a\0b".to_string()]).unwrap_err();
        assert!(format!("{err}").contains("NUL"));
    }

    #[test]
    fn build_cmdline_roundtrips_through_command_line_to_argv_w() {
        use windows_sys::Win32::Foundation::LocalFree;

        #[link(name = "shell32")]
        extern "system" {
            fn CommandLineToArgvW(cmdline: *const u16, argc: *mut i32) -> *mut *mut u16;
        }

        let cmd = vec![
            "cmd.exe".to_string(),
            "plain".to_string(),
            "".to_string(),
            "two words".to_string(),
            r"C:\path with space\".to_string(),
            r#"x\"y"#.to_string(),
            r#"say "hello""#.to_string(),
            "制表\t符".to_string(),
        ];
        let line = build_cmdline(&cmd).unwrap();
        let wide = to_wide(&line);
        let mut argc = 0;
        // Safety: wide is NUL-terminated; argv is released with LocalFree.
        let argv = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) };
        assert!(!argv.is_null());
        assert_eq!(argc as usize, cmd.len());
        let mut parsed = Vec::new();
        for i in 0..argc as usize {
            // Safety: CommandLineToArgvW returns argc valid NUL-terminated strings.
            let p = unsafe { *argv.add(i) };
            let mut len = 0;
            while unsafe { *p.add(len) } != 0 {
                len += 1;
            }
            let slice = unsafe { std::slice::from_raw_parts(p, len) };
            parsed.push(String::from_utf16(slice).unwrap());
        }
        unsafe {
            LocalFree(argv.cast());
        }
        assert_eq!(parsed, cmd);
    }

    // ---- build_env_block：CreateProcessW lpEnvironment 的双 NUL 结尾 UTF-16 块 ----

    fn env_block_to_string(env: &[(String, String)]) -> String {
        // 把 UTF-16 环境块转回字符串便于断言（NUL 用显式标记表示）
        let wide = build_env_block(env);
        let mut out = String::new();
        for &u in &wide {
            if u == 0 {
                out.push_str("<NUL>");
            } else {
                out.push(char::from_u32(u as u32).unwrap_or('?'));
            }
        }
        out
    }

    #[test]
    fn env_block_basic_key_value_pairs() {
        let s = env_block_to_string(&[
            ("FOO".to_string(), "bar".to_string()),
            ("BAZ".to_string(), "qux".to_string()),
        ]);
        assert_eq!(s, "FOO=bar<NUL>BAZ=qux<NUL><NUL>");
    }

    #[test]
    fn env_block_double_nul_terminator() {
        // 空输入也必须有双 NUL 结尾（CreateProcessW 据此判断环境块结束）
        let wide = build_env_block(&[]);
        assert_eq!(wide, vec![0u16, 0u16]);
    }

    #[test]
    fn env_block_empty_key_is_dropped() {
        // 空键无法构成 "KEY=VAL"，防御性跳过
        let s = env_block_to_string(&[("".to_string(), "v".to_string())]);
        assert_eq!(s, "<NUL><NUL>");
    }

    #[test]
    fn env_block_key_with_equals_is_dropped() {
        // 键含 '=' 会污染解析，跳过
        let s = env_block_to_string(&[("K=E".to_string(), "v".to_string())]);
        assert_eq!(s, "<NUL><NUL>");
    }

    #[test]
    fn env_block_key_with_nul_is_dropped() {
        let s = env_block_to_string(&[("K\0X".to_string(), "v".to_string())]);
        assert_eq!(s, "<NUL><NUL>");
    }

    #[test]
    fn env_block_value_with_nul_is_dropped() {
        let s = env_block_to_string(&[("K".to_string(), "v\0x".to_string())]);
        assert_eq!(s, "<NUL><NUL>");
    }

    #[test]
    fn env_block_value_with_equals_is_kept() {
        // 值含 '=' 合法（只看第一个 = 分割 KEY/VAL）
        let s = env_block_to_string(&[("PATH".to_string(), "a=b;c".to_string())]);
        assert_eq!(s, "PATH=a=b;c<NUL><NUL>");
    }

    #[test]
    fn env_block_mixed_valid_and_invalid_only_keeps_valid() {
        let s = env_block_to_string(&[
            ("".to_string(), "drop".to_string()),
            ("GOOD".to_string(), "keep".to_string()),
            ("K=E".to_string(), "drop".to_string()),
            ("ALSO_GOOD".to_string(), "v=1".to_string()),
        ]);
        assert_eq!(s, "GOOD=keep<NUL>ALSO_GOOD=v=1<NUL><NUL>");
    }

    #[test]
    fn env_block_unicode_value_round_trip() {
        // UTF-16 环境块：非 ASCII 字符直接 encode_utf16
        let s = env_block_to_string(&[("NAME".to_string(), "中文".to_string())]);
        assert_eq!(s, "NAME=中文<NUL><NUL>");
    }
}

/// 真机 Win32 集成测试：run_container 完整路径（AppContainer + Job + CreateProcess）。
/// 需要真 Windows + 系统自带无害可执行（hostname.exe / write.exe 等）。
#[cfg(test)]
#[cfg(windows)]
mod real_windows_tests {
    use super::*;
    use crate::backend::Limits;
    use crate::job::Job;
    use crate::token::{to_wide, AppContainerProfile, CapabilitySid, OwnedHandle};
    use windows_sys::Win32::Foundation::{
        DuplicateHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, SetEvent, WaitForSingleObject,
    };

    fn create_profile(name: &str, caps: &[CapabilitySid]) -> AppContainerProfile {
        AppContainerProfile::create(name, caps)
            .unwrap_or_else(|e| panic!("创建 AppContainer profile 失败：{}", e))
    }
    /// 找一个系统自带的无害可执行：hostname.exe 最稳（打印主机名 + rc=0）。
    fn hostname_exe() -> String {
        for candidate in [
            r"C:\Windows\System32\hostname.exe",
            r"C:\Windows\system32\hostname.exe",
        ] {
            if std::path::Path::new(candidate).is_file() {
                return candidate.to_string();
            }
        }
        // 兜底：从 SystemRoot 环境变量拼
        if let Ok(root) = std::env::var("SystemRoot") {
            let p = format!(r"{}\System32\hostname.exe", root);
            if std::path::Path::new(&p).is_file() {
                return p;
            }
        }
        panic!("Windows 系统目录中未找到 hostname.exe");
    }

    fn curl_exe() -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = std::path::Path::new(&root)
            .join("System32")
            .join("curl.exe");
        assert!(path.is_file(), "Windows 系统目录中未找到 curl.exe");
        path.to_string_lossy().into_owned()
    }

    fn unique_name(label: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("wboxtest_{}_{}_{}", std::process::id(), n, label)
    }

    fn minimal_process_env() -> Vec<(String, String)> {
        crate::backend::env::build_child_env(
            &[],
            &[],
            false,
            crate::backend::env::GuestFlavor::Windows,
        )
    }

    #[test]
    fn read_only_acl_child_probe() {
        let Ok(root) = std::env::var("WBOX_TEST_READ_ONLY_ROOT") else {
            return;
        };
        let root = std::path::Path::new(&root);
        assert_eq!(
            std::fs::read(root.join("canary.txt")).unwrap(),
            b"read-only-canary"
        );

        for path in [root.join("canary.txt"), root.join("created.txt")] {
            let error = std::fs::write(&path, b"must-not-write").unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "RX ACE 下写入 '{}' 应被拒绝：{}",
                path.display(),
                error
            );
        }
    }

    /// RX ACE 必须提供真正的“可读、不可写”语义，不能只检查 ACL API 返回成功。
    #[test]
    fn read_only_acl_allows_reads_and_denies_writes() {
        let base = std::env::temp_dir().join(unique_name("readonly_acl"));
        let exec_dir = base.join("exec");
        let read_only_dir = base.join("read-only");
        std::fs::create_dir_all(&exec_dir).unwrap();
        std::fs::create_dir_all(&read_only_dir).unwrap();
        std::fs::write(read_only_dir.join("canary.txt"), b"read-only-canary").unwrap();

        let child = exec_dir.join("wbox-read-only-probe.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
        crate::acl::grant_read_recursive(&exec_dir).unwrap();
        crate::acl::grant_read_recursive(&read_only_dir).unwrap();

        let profile = create_profile(&unique_name("readonly_profile"), &[]);
        let mut job = Job::create(Limits::default()).unwrap();
        let cmdline = build_cmdline(&[
            child.to_string_lossy().into_owned(),
            "--exact".to_string(),
            "sandbox::real_windows_tests::read_only_acl_child_probe".to_string(),
            "--nocapture".to_string(),
        ])
        .unwrap();
        let mut env = minimal_process_env();
        env.push((
            "WBOX_TEST_READ_ONLY_ROOT".to_string(),
            read_only_dir.to_string_lossy().into_owned(),
        ));
        let rc = run_container(
            &profile,
            &[],
            &cmdline,
            &exec_dir.to_string_lossy(),
            &mut job,
            &env,
        )
        .unwrap();
        assert_eq!(rc, 0, "AppContainer RX ACE 行为探针失败");

        drop(profile);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn network_server_capability_child_probe() {
        let Ok(expected) = std::env::var("WBOX_TEST_LISTEN_ALLOWED") else {
            return;
        };
        let expected = expected == "1";
        let result = std::net::TcpListener::bind(("0.0.0.0", 0));
        assert_eq!(
            result.is_ok(),
            expected,
            "TcpListener bind capability result differed: {:?}",
            result.err()
        );
    }

    /// Winsock bind 本身不是 server capability 判据：流量过滤发生在连接路径。
    #[test]
    fn tcp_listener_bind_succeeds_with_or_without_server_capabilities() {
        let base = std::env::temp_dir().join(unique_name("server_caps"));
        let exec_dir = base.join("exec");
        std::fs::create_dir_all(&exec_dir).unwrap();
        let child = exec_dir.join("wbox-server-capability-probe.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
        crate::acl::grant_read_recursive(&exec_dir).unwrap();
        let cmdline = build_cmdline(&[
            child.to_string_lossy().into_owned(),
            "--exact".to_string(),
            "sandbox::real_windows_tests::network_server_capability_child_probe".to_string(),
            "--nocapture".to_string(),
        ])
        .unwrap();
        let workdir = exec_dir.to_string_lossy().into_owned();

        let run_case = |label: &str, caps: Vec<CapabilitySid>, expected: bool| {
            let profile = create_profile(&unique_name(label), &caps);
            let mut job = Job::create(Limits::default()).unwrap();
            let mut env = minimal_process_env();
            env.push((
                "WBOX_TEST_LISTEN_ALLOWED".to_string(),
                if expected { "1" } else { "0" }.to_string(),
            ));
            let rc = run_container(&profile, &caps, &cmdline, &workdir, &mut job, &env).unwrap();
            assert_eq!(rc, 0, "server capability case '{label}' failed");
        };

        run_case("server_none", Vec::new(), true);
        run_case(
            "server_client",
            vec![CapabilitySid::internet_client().unwrap()],
            true,
        );
        run_case(
            "server_internet",
            vec![CapabilitySid::internet_client_server().unwrap()],
            true,
        );
        run_case(
            "server_private",
            vec![CapabilitySid::private_network_client_server().unwrap()],
            true,
        );

        let _ = std::fs::remove_dir_all(base);
    }

    #[repr(C)]
    struct NtUnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }

    #[repr(C)]
    struct NtObjectAttributes {
        length: u32,
        root_directory: HANDLE,
        object_name: *mut NtUnicodeString,
        attributes: u32,
        security_descriptor: *mut core::ffi::c_void,
        security_quality_of_service: *mut core::ffi::c_void,
    }

    #[repr(C)]
    struct NtIoStatusBlock {
        status: isize,
        information: usize,
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut HANDLE,
            desired_access: u32,
            object_attributes: *mut NtObjectAttributes,
            io_status_block: *mut NtIoStatusBlock,
            allocation_size: *mut i64,
            file_attributes: u32,
            share_access: u32,
            create_disposition: u32,
            create_options: u32,
            ea_buffer: *mut core::ffi::c_void,
            ea_length: u32,
        ) -> i32;
    }

    fn nt_open_relative(root: HANDLE, name: &str) -> i32 {
        const OBJ_CASE_INSENSITIVE: u32 = 0x40;
        const FILE_READ_DATA: u32 = 0x0001;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const SYNCHRONIZE: u32 = 0x0010_0000;
        const FILE_OPEN: u32 = 1;
        const FILE_NON_DIRECTORY_FILE: u32 = 0x0040;
        const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0020;
        const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

        let mut name_wide: Vec<u16> = name.encode_utf16().collect();
        let mut unicode = NtUnicodeString {
            length: (name_wide.len() * 2) as u16,
            maximum_length: (name_wide.len() * 2) as u16,
            buffer: name_wide.as_mut_ptr(),
        };
        let mut attributes = NtObjectAttributes {
            length: std::mem::size_of::<NtObjectAttributes>() as u32,
            root_directory: root,
            object_name: &mut unicode,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: std::ptr::null_mut(),
            security_quality_of_service: std::ptr::null_mut(),
        };
        let mut io = NtIoStatusBlock {
            status: 0,
            information: 0,
        };
        let mut opened = std::ptr::null_mut();
        // # Safety: 所有 NT 结构及 UTF-16 缓冲区覆盖调用；root 是继承目录句柄。
        let status = unsafe {
            NtCreateFile(
                &mut opened,
                FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                &mut attributes,
                &mut io,
                std::ptr::null_mut(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
                FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
                0,
            )
        };
        if status >= 0 {
            drop(OwnedHandle(opened));
        }
        status
    }

    /// 由 `inherited_directory_handle_does_not_bypass_child_dacl` 复制并降权启动。
    #[test]
    fn inherited_directory_handle_child_probe() {
        let Ok(raw) = std::env::var("WBOX_TEST_ROOT_HANDLE") else {
            return;
        };
        let root = raw.parse::<usize>().unwrap() as HANDLE;
        let status = nt_open_relative(root, "canary.txt");
        const STATUS_ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;
        assert_eq!(
            status, STATUS_ACCESS_DENIED,
            "目录 HANDLE 不应绕过 canary 子对象 DACL；NTSTATUS=0x{:08X}",
            status as u32
        );
    }

    /// 架构探针：AppContainer 即使精确继承了目录根 HANDLE，打开后代时仍会执行
    /// 后代对象的 DACL 检查。该事实决定 Windows OCI volume 不能只靠 HANDLE，
    /// 必须走 broker 或有回滚能力的 profile 专属 ACL。
    #[test]
    fn inherited_directory_handle_does_not_bypass_child_dacl() {
        let base = std::env::temp_dir().join(unique_name("handle_probe"));
        let exec_dir = base.join("exec");
        let volume_dir = base.join("volume");
        std::fs::create_dir_all(&exec_dir).unwrap();
        std::fs::create_dir_all(&volume_dir).unwrap();
        std::fs::write(volume_dir.join("canary.txt"), b"secret").unwrap();

        let child = exec_dir.join("wbox-handle-probe.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
        crate::acl::grant_read_recursive(&exec_dir).unwrap();

        let root_wide = to_wide(&volume_dir.to_string_lossy());
        const GENERIC_READ: u32 = 0x8000_0000;
        // # Safety: 路径为 NUL 结尾；目录以 reparse-point 本体方式打开。
        let root = unsafe {
            CreateFileW(
                root_wide.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        assert!(!root.is_null(), "打开 volume 根目录失败");
        let root = OwnedHandle(root);

        let profile = create_profile(&unique_name("handle_profile"), &[]);
        let mut job = Job::create(Limits::default()).unwrap();
        let cmdline = build_cmdline(&[
            child.to_string_lossy().into_owned(),
            "--exact".to_string(),
            "sandbox::real_windows_tests::inherited_directory_handle_child_probe".to_string(),
            "--nocapture".to_string(),
        ])
        .unwrap();
        let mut env = minimal_process_env();
        env.push((
            "WBOX_TEST_ROOT_HANDLE".to_string(),
            (root.raw() as usize).to_string(),
        ));
        let rc = run_container_with_handles(
            &profile,
            &[],
            &cmdline,
            &exec_dir.to_string_lossy(),
            &mut job,
            &env,
            &[root.raw()],
        )
        .unwrap();
        assert_eq!(rc, 0, "AppContainer HANDLE/DACL 子探针失败");

        drop(profile);
        drop(root);
        let _ = std::fs::remove_dir_all(base);
    }

    /// 完整链路：在 AppContainer + Job 内起 hostname.exe，期望 rc=0。
    /// 这条链覆盖 token/sandbox/job 三个模块的真实协作路径。
    #[test]
    fn run_container_executes_hostname_in_sandbox() {
        let exe = hostname_exe();

        let profile = create_profile(&unique_name("rc"), &[]);
        let caps: Vec<CapabilitySid> = Vec::new();
        let mut job = Job::create(Limits::default()).unwrap();
        let workdir = std::path::Path::new(&exe)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let env = minimal_process_env();

        // build_cmdline 把 exe 路径加引号
        // from_ref 而非 clone：无谓的 String 复制（clippy cloned_ref_to_slice_refs）
        let cmdline = build_cmdline(std::slice::from_ref(&exe)).unwrap();
        let rc = run_container(&profile, &caps, &cmdline, &workdir, &mut job, &env).unwrap();
        assert_eq!(rc, 0, "hostname.exe 在 AppContainer 内应返回 0");
    }

    /// broker 接入探针：在 guest 恢复前向目标进程动态复制内核对象。
    #[test]
    fn created_hook_can_duplicate_handle_into_suspended_child() {
        let exe = hostname_exe();
        let profile = create_profile(&unique_name("created_hook"), &[]);
        let mut job = Job::create(Limits::default()).unwrap();
        let workdir = std::path::Path::new(&exe)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let env = minimal_process_env();
        let cmdline = build_cmdline(std::slice::from_ref(&exe)).unwrap();
        let source = unsafe { OwnedHandle(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null())) };
        assert!(!source.raw().is_null(), "CreateEventW 失败");
        let mut roundtrip = None;

        let rc = run_container_with_handles_and_created_hook(
            &profile,
            &[],
            &cmdline,
            &workdir,
            &mut job,
            &env,
            &[],
            |child_process, _| {
                let current = unsafe { GetCurrentProcess() };
                let mut child_handle = std::ptr::null_mut();
                if unsafe {
                    DuplicateHandle(
                        current,
                        source.raw(),
                        child_process,
                        &mut child_handle,
                        0,
                        0,
                        DUPLICATE_SAME_ACCESS,
                    )
                } == 0
                {
                    return Err(crate::error::WboxError::spawn(format!(
                        "DuplicateHandle(注入子进程) 失败，GetLastError={}",
                        unsafe { GetLastError() }
                    )));
                }
                let mut copied_back = std::ptr::null_mut();
                if unsafe {
                    DuplicateHandle(
                        child_process,
                        child_handle,
                        current,
                        &mut copied_back,
                        0,
                        0,
                        DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE,
                    )
                } == 0
                {
                    return Err(crate::error::WboxError::spawn(format!(
                        "DuplicateHandle(从子进程取回) 失败，GetLastError={}",
                        unsafe { GetLastError() }
                    )));
                }
                roundtrip = Some(OwnedHandle(copied_back));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(rc, 0, "动态 HANDLE 注入不应影响 guest 启动");

        let roundtrip = roundtrip.expect("created hook 必须返回事件 HANDLE");
        assert_ne!(unsafe { SetEvent(source.raw()) }, 0, "SetEvent 失败");
        assert_eq!(
            unsafe { WaitForSingleObject(roundtrip.raw(), 0) },
            0,
            "复制往返后的 HANDLE 应引用同一个事件对象"
        );
    }

    #[test]
    fn created_hook_failure_reaps_suspended_child() {
        let exe = hostname_exe();
        let profile = create_profile(&unique_name("created_hook_fail"), &[]);
        let mut job = Job::create(Limits::default()).unwrap();
        let workdir = std::path::Path::new(&exe)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let env = minimal_process_env();
        let cmdline = build_cmdline(std::slice::from_ref(&exe)).unwrap();

        let err = run_container_with_handles_and_created_hook(
            &profile,
            &[],
            &cmdline,
            &workdir,
            &mut job,
            &env,
            &[],
            |_, _| Err(crate::error::WboxError::spawn("broker setup probe failed")),
        )
        .unwrap_err();
        assert!(
            format!("{}", err).contains("broker setup probe failed"),
            "{}",
            err
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if job.process_ids().unwrap().is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "created hook 失败后不能在 Job 内留下挂起进程"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// 缺省（空 capabilities）状态下子进程无 INTERNET_CLIENT，AppContainer
    /// 拒绝网络。这里只验证链路启动不因 caps=[] 失败（网络拒绝是后续 socket
    /// 调用的事，不影响 hostname 启动）。
    #[test]
    fn run_container_with_empty_caps_starts_process() {
        let exe = hostname_exe();
        let profile = create_profile(&unique_name("nc"), &[]);
        let mut job = Job::create(Limits {
            memory_mb: 0,
            cpu_pct: 0,
            max_procs: 0,
        })
        .unwrap();
        let workdir = std::path::Path::new(&exe)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let env = minimal_process_env();
        let cmdline = build_cmdline(&[exe]).unwrap();
        let rc = run_container(&profile, &[], &cmdline, &workdir, &mut job, &env).unwrap();
        assert_eq!(rc, 0);
    }

    /// Public-network behavior probe. Ignored by default because it deliberately
    /// depends on a public endpoint; the host preflight separates endpoint or
    /// policy failures from AppContainer capability wiring failures.
    #[test]
    #[ignore = "requires host access to http://1.1.1.1"]
    fn internet_client_controls_numeric_ip_access() {
        const URL: &str = "http://1.1.1.1/cdn-cgi/trace";
        let exe = curl_exe();
        let args = ["-sS", "--connect-timeout", "8", "--output", "-", URL];

        let host = std::process::Command::new(&exe)
            .args(args)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("无法执行宿主 curl.exe");
        assert!(
            host.success(),
            "宿主无法访问测试端点，不能判定 AppContainer 网络策略: {host}"
        );

        let workdir = std::path::Path::new(&exe)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let env = minimal_process_env();
        let cmd = std::iter::once(exe.clone())
            .chain(args.into_iter().map(str::to_string))
            .collect::<Vec<_>>();
        let cmdline = build_cmdline(&cmd).unwrap();

        let denied_caps = Vec::new();
        let denied_profile = create_profile(&unique_name("net_deny"), &denied_caps);
        let mut denied_job = Job::create(Limits::default()).unwrap();
        let denied = run_container(
            &denied_profile,
            &denied_caps,
            &cmdline,
            &workdir,
            &mut denied_job,
            &env,
        )
        .unwrap();
        assert_ne!(
            denied, 0,
            "默认 AppContainer 不应访问公网数值 IP；curl rc={denied}"
        );

        let allowed_caps = vec![CapabilitySid::internet_client().unwrap()];
        let allowed_profile = create_profile(&unique_name("net_allow"), &allowed_caps);
        let mut allowed_job = Job::create(Limits::default()).unwrap();
        let allowed = run_container(
            &allowed_profile,
            &allowed_caps,
            &cmdline,
            &workdir,
            &mut allowed_job,
            &env,
        )
        .unwrap();
        assert_eq!(
            allowed, 0,
            "INTERNET_CLIENT 应允许访问宿主已证实可达的公网数值 IP；curl rc={allowed}"
        );
    }

    /// A same-host private address still crosses AppContainer loopback
    /// isolation, so it cannot validate PRIVATE_NETWORK_CLIENT_SERVER.
    #[test]
    #[ignore = "requires a Windows Private network adapter"]
    fn private_network_capability_does_not_bypass_same_host_isolation() {
        use std::io::{Read, Write};
        use std::net::{IpAddr, TcpListener, UdpSocket};

        let route = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
        route.connect(("1.1.1.1", 80)).unwrap();
        let private_ip = match route.local_addr().unwrap().ip() {
            IpAddr::V4(ip) if !ip.is_loopback() => ip,
            other => panic!("未找到可用的非 loopback IPv4 地址：{other}"),
        };
        let exe = curl_exe();
        let workdir = std::path::Path::new(&exe)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let env = minimal_process_env();

        let run_case = |label: &str, caps: Vec<CapabilitySid>| {
            let listener = TcpListener::bind((private_ip, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let server = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut request = [0u8; 1024];
                            let _ = stream.read(&mut request);
                            stream
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nPRIVATE_OK",
                                )
                                .unwrap();
                            return true;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if std::time::Instant::now() >= deadline {
                                return false;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                        Err(error) => panic!("private endpoint accept failed: {error}"),
                    }
                }
            });

            let url = format!("http://{private_ip}:{port}/");
            let cmdline = build_cmdline(&[
                exe.clone(),
                "-sS".to_string(),
                "--noproxy".to_string(),
                "*".to_string(),
                "--connect-timeout".to_string(),
                "2".to_string(),
                "--max-time".to_string(),
                "3".to_string(),
                url,
            ])
            .unwrap();
            let profile = create_profile(&unique_name(label), &caps);
            let mut job = Job::create(Limits::default()).unwrap();
            let rc = run_container(&profile, &caps, &cmdline, &workdir, &mut job, &env).unwrap();
            (rc, server.join().unwrap())
        };

        let (denied_rc, denied_reached) = run_case("private_none", Vec::new());
        assert_ne!(denied_rc, 0);
        assert!(!denied_reached);

        let private_caps = vec![CapabilitySid::private_network_client_server().unwrap()];
        let (allowed_rc, allowed_reached) = run_case("private_allowed", private_caps);
        assert_ne!(allowed_rc, 0);
        assert!(!allowed_reached);
    }

    /// External peer gate for PRIVATE_NETWORK_CLIENT_SERVER. Set the endpoint
    /// to an HTTP service on a different machine in a Windows-Private network.
    #[test]
    #[ignore = "requires WBOX_TEST_PRIVATE_ENDPOINT on an external private-network peer"]
    fn private_network_capability_controls_external_endpoint() {
        let endpoint = std::env::var("WBOX_TEST_PRIVATE_ENDPOINT")
            .expect("set WBOX_TEST_PRIVATE_ENDPOINT=http://PRIVATE-PEER:PORT/");
        let exe = curl_exe();
        let args = [
            "-sS",
            "--noproxy",
            "*",
            "--connect-timeout",
            "3",
            "--max-time",
            "5",
            endpoint.as_str(),
        ];
        let host = std::process::Command::new(&exe)
            .args(args)
            .status()
            .expect("无法执行宿主 curl.exe");
        assert!(
            host.success(),
            "宿主无法访问 private peer，不能裁决 capability"
        );

        let workdir = std::path::Path::new(&exe)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let env = minimal_process_env();
        let command = std::iter::once(exe)
            .chain(args.into_iter().map(str::to_string))
            .collect::<Vec<_>>();
        let cmdline = build_cmdline(&command).unwrap();

        let denied_caps = Vec::new();
        let denied_profile = create_profile(&unique_name("private_external_deny"), &denied_caps);
        let mut denied_job = Job::create(Limits::default()).unwrap();
        let denied = run_container(
            &denied_profile,
            &denied_caps,
            &cmdline,
            &workdir,
            &mut denied_job,
            &env,
        )
        .unwrap();
        assert_ne!(denied, 0, "无 capability 不应访问 private peer");

        let allowed_caps = vec![CapabilitySid::private_network_client_server().unwrap()];
        let allowed_profile = create_profile(&unique_name("private_external_allow"), &allowed_caps);
        let mut allowed_job = Job::create(Limits::default()).unwrap();
        let allowed = run_container(
            &allowed_profile,
            &allowed_caps,
            &cmdline,
            &workdir,
            &mut allowed_job,
            &env,
        )
        .unwrap();
        assert_eq!(
            allowed, 0,
            "PRIVATE_NETWORK_CLIENT_SERVER 应允许访问 private peer"
        );
    }

    /// 命令行为空 → build_cmdline 报 args 错误（不调 CreateProcess）。
    /// 这条不是真机测试，但它钉死了 run_container 入口的前置条件，
    /// 放这里和上面的真机测试组成"完整链路 + 入口防御"对。
    #[test]
    fn build_cmdline_empty_propagates_as_args_error() {
        let res = build_cmdline(&[]);
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        // error.rs 里 args 类退出码 1，错误信息含\"缺少要执行的命令\"
        assert!(
            msg.contains("缺少"),
            "应报\"缺少要执行的命令\"，实得：{}",
            msg
        );
    }

    /// workdir 不存在 → CreateProcessW 在 lpCurrentDirectory 阶段失败，
    /// 报 spawn 错误。用 workdir = 显然不存在的路径。
    #[test]
    fn run_container_missing_workdir_is_spawn_error() {
        let exe = hostname_exe();
        let profile = create_profile(&unique_name("mw"), &[]);
        let mut job = Job::create(Limits::default()).unwrap();
        let cmdline = build_cmdline(&[exe]).unwrap();
        // 一个绝对不存在的路径
        let bogus = r"C:\wbox-definitely-does-not-exist-xyz-12345";
        let err = run_container(&profile, &[], &cmdline, bogus, &mut job, &[])
            .expect_err("应报 spawn 错误");
        let msg = format!("{}", err);
        assert!(
            msg.contains("CreateProcessW") || msg.contains("GetLastError"),
            "应指向 CreateProcessW 失败，实得：{}",
            msg
        );
    }
}
