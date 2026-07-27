//! rootfs 目录的 ACL 授权（Windows）。
//!
//! 背景（双层隔离衔接）：NativeBackend 以 AppContainer 令牌拉起
//! wbox-linux.exe 时，子进程令牌带上 AppContainer SID，完整性级别 Low。
//! `%USERPROFILE%\.wbox\images\...\rootfs` 默认从用户目录继承 ACL，
//! **不含 AppContainer 可读的 ACE**，子进程将读不到 rootfs。
//! 因此在 pull 完成后，对 rootfs 递归授予
//! "ALL APPLICATION PACKAGES"（`S-1-15-2-1`，所有 AppContainer 共用的
//! 内置组）读取/执行权限，使任何容器 profile 都能读取镜像内容
//! （rootfs 本属共享只读内容；容器名级 SID 授权留作后续收紧项）。
//!
//! 实现：ConvertStringSidToSidW + EXPLICIT_ACCESS_W + SetEntriesInAclW
//! （合并现有 DACL）+ SetNamedSecurityInfoW。目录带
//! SUB_CONTAINERS_AND_OBJECTS_INHERIT 使新文件自动继承；
//! 已存在的子孙项则递归逐项授权。

use std::path::Path;

use windows_sys::Win32::Foundation::{GetLastError, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW,
    EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, PSECURITY_DESCRIPTOR, PSID,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};

use crate::error::{ErrKind, KindExt, Result, WboxError};
use crate::token::to_wide;
use anyhow::Context;

/// "ALL APPLICATION PACKAGES" 知名 SID（S-1-15-2-1）。
const ALL_APP_PACKAGES_SID: &str = "S-1-15-2-1";
/// GENERIC_READ | GENERIC_EXECUTE（windows-sys 0.59 未在启用的 feature 集内导出常量，
/// 按 Win32 头文件值定义：只读 + 遍历/执行，rootfs 只需读）。
const ACCESS_READ_EXECUTE: u32 = 0x8000_0000 | 0x2000_0000;
/// GENERIC_ALL。私有运行副本只向该容器 SID 授权，guest 需要完整创建、改写、
/// rename 与删除语义；共享镜像缓存仍只有读取 ACE。
const ACCESS_MODIFY: u32 = 0x1000_0000;

/// 递归地为 `root` 及其全部子孙文件/目录添加 ALL APPLICATION PACKAGES 读取 ACE。
pub fn grant_read_recursive(root: &Path) -> Result<()> {
    if !root.is_dir() {
        return Err(WboxError::registry(format!(
            "rootfs 目录 '{}' 不存在，无法授权",
            root.display()
        )));
    }
    let mut count = 0u32;
    grant_tree(root, ALL_APP_PACKAGES_SID, ACCESS_READ_EXECUTE, &mut count)?;
    Ok(())
}

/// 仅向指定 AppContainer profile 的确定性 SID 授予私有 rootfs 修改权。
pub fn grant_modify_recursive_for_profile(root: &Path, profile_name: &str) -> Result<()> {
    if !root.is_dir() {
        return Err(WboxError::profile(format!(
            "私有 rootfs 目录 '{}' 不存在，无法授权",
            root.display()
        )));
    }
    let sid = crate::token::AppContainerProfile::derived_sid_string(profile_name)?;
    let mut count = 0u32;
    grant_tree(root, &sid, ACCESS_MODIFY, &mut count)?;
    Ok(())
}

/// 递归遍历并授权。单项失败即整体报错（授权不全 = 运行时莫名 EACCES）。
fn grant_tree(path: &Path, sid: &str, access: u32, count: &mut u32) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("读取 '{}' 元数据失败", path.display()))
        .ctx(ErrKind::Registry)?;
    if is_reparse_point(&metadata) {
        // 绝不跟随 symlink/junction，否则 ACL 遍历可越出 rootfs。
        return Ok(());
    }
    let is_dir = metadata.is_dir();
    grant_ace(path, is_dir, sid, access)?;
    *count += 1;
    if is_dir {
        let entries = std::fs::read_dir(path)
            .with_context(|| format!("遍历 '{}' 失败", path.display()))
            .ctx(ErrKind::Registry)?;
        for entry in entries {
            let entry = entry.context("读取目录项失败").ctx(ErrKind::Registry)?;
            grant_tree(&entry.path(), sid, access, count)?;
        }
    }
    Ok(())
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// 对单个路径授予 ACE（目录带继承标志，文件不继承）。
fn grant_ace(path: &Path, is_dir: bool, sid_text: &str, access: u32) -> Result<()> {
    // 1. 从字符串构造 trustee SID
    let sid_str = to_wide(sid_text);
    let mut sid: PSID = std::ptr::null_mut();
    // # Safety: sid_str 为 NUL 结尾宽字符串；sid 为有效输出指针，
    // 成功后由 LocalFree 释放（见函数末尾 guard）。
    let ok = unsafe { ConvertStringSidToSidW(sid_str.as_ptr(), &mut sid) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(WboxError::registry(format!(
            "ConvertStringSidToSidW({}) 失败，GetLastError={}",
            sid_text, err
        )));
    }
    let _sid_guard = LocalGuard(sid as HLOCAL);

    // 2. 读取现有 DACL（保留之，在其上追加而非替换）
    let path_w = to_wide(&path.to_string_lossy());
    let mut old_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // # Safety: path_w 为 NUL 结尾宽字符串；old_dacl/sd 为有效输出指针，
    // sd 成功时由 LocalFree 释放。
    let ret = unsafe {
        GetNamedSecurityInfoW(
            path_w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_dacl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if ret != 0 {
        return Err(WboxError::registry(format!("GetNamedSecurityInfoW('{}') 失败，错误码={}", path.display(), ret)));
    }
    let _sd_guard = LocalGuard(sd as HLOCAL);

    // 3. 构造 EXPLICIT_ACCESS（TRUSTEE_IS_SID：ptstrName 直接放 SID 指针）
    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: 0, // NO_MULTIPLE_TRUSTEE
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_UNKNOWN,
        ptstrName: sid as *mut u16,
    };
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if is_dir {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: trustee,
    };

    // 4. 合并新旧 DACL 并写回
    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    // # Safety: ea 指向单个有效 EXPLICIT_ACCESS_W；old_dacl 来自上一步（可为 null）；
    // new_dacl 为有效输出指针，成功后由 LocalFree 释放。
    let ret = unsafe { SetEntriesInAclW(1, &ea, old_dacl, &mut new_dacl) };
    if ret != 0 {
        return Err(WboxError::registry(format!("SetEntriesInAclW('{}') 失败，错误码={}", path.display(), ret)));
    }
    let _acl_guard = LocalGuard(new_dacl as HLOCAL);

    // # Safety: path_w 仍有效；new_dacl 为合法 DACL；仅更新 DACL 部分。
    let ret = unsafe {
        SetNamedSecurityInfoW(
            path_w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null_mut(),
        )
    };
    if ret != 0 {
        return Err(WboxError::registry(format!("SetNamedSecurityInfoW('{}') 失败，错误码={}", path.display(), ret)));
    }
    Ok(())
}

/// LocalAlloc/LocalFree 分配内存（SID、SECURITY_DESCRIPTOR、ACL）的 RAII 释放器。
struct LocalGuard(HLOCAL);

impl Drop for LocalGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // # Safety: 句柄来自 ConvertStringSidToSidW/GetNamedSecurityInfoW/
            // SetEntriesInAclW 的 LocalAlloc 分配，仅释放一次。
            unsafe { LocalFree(self.0) };
        }
    }
}
