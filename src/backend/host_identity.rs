//! Linux host identity facts shared by namespace and limit policy.

use crate::error::{Result, WboxError};

pub(super) fn current_posix() -> Result<agenterm_platform::user_identity::PosixCredentials> {
    agenterm_platform::user_identity::current_user_identity()
        .map_err(|error| WboxError::spawn(format!("读取宿主用户身份失败：{error}")))?
        .posix_credentials()
        .ok_or_else(|| WboxError::spawn("Linux 后端未得到 POSIX 用户身份"))
}
