//! Windows rootfs authorization policy over shared host ACL mechanisms.
//!
//! `agenterm-platform` owns SID validation, DACL merging, bounded access masks,
//! and no-follow tree traversal. wbox owns which principal receives access:
//! shared image roots are readable by all AppContainers, while private roots
//! are modifiable only by the selected profile SID.

use std::path::Path;

use agenterm_platform::directory_access::{
    grant_directory_tree_access, DirectoryPrincipal, DirectoryTreeAccess,
    WellKnownDirectoryPrincipal,
};

use crate::error::{Result, WboxError};

/// Give every AppContainer read/execute access to one shared immutable rootfs.
pub fn grant_read_recursive(root: &Path) -> Result<()> {
    grant_directory_tree_access(
        root,
        DirectoryPrincipal::WellKnown(WellKnownDirectoryPrincipal::AllApplicationPackages),
        DirectoryTreeAccess::ReadExecute,
    )
    .map(|_| ())
    .map_err(|error| {
        WboxError::registry(format!(
            "rootfs 目录 '{}' 授予 AppContainer 读取权失败：{}",
            root.display(),
            error
        ))
    })
}

/// Give only the deterministic profile SID bounded content-modification access.
pub fn grant_modify_recursive_for_profile(root: &Path, profile_name: &str) -> Result<()> {
    let sid = crate::token::AppContainerProfile::derived_sid(profile_name)?;
    grant_directory_tree_access(
        root,
        DirectoryPrincipal::WindowsSid(sid.as_bytes()),
        DirectoryTreeAccess::ModifyContents,
    )
    .map(|_| ())
    .map_err(|error| {
        WboxError::profile(format!(
            "私有目录 '{}' 向 profile '{}' 授予修改权失败：{}",
            root.display(),
            profile_name,
            error
        ))
    })
}
