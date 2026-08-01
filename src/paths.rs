//! Product-owned paths derived from shared host filesystem conventions.

use std::path::PathBuf;

pub(crate) fn root() -> Result<PathBuf, agenterm_platform::filesystem::FilesystemError> {
    agenterm_platform::filesystem::user_home_directory().map(|home| home.join(".wbox"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn managed_root_keeps_the_product_directory_name_in_wbox() {
        let home = crate::testenv::TempHome::new("managed-root");
        assert_eq!(super::root().unwrap(), home.dir.join(".wbox"));
    }

    #[test]
    fn managed_root_does_not_fall_back_to_another_os_environment_variable() {
        let mut home = crate::testenv::TempHome::new("managed-root-no-fallback");
        let fallback = home.dir.join("wrong-os-home");
        #[cfg(windows)]
        {
            home.env().set("USERPROFILE", "");
            home.env().set("HOME", &fallback);
        }
        #[cfg(not(windows))]
        {
            home.env().set("HOME", "");
            home.env().set("USERPROFILE", &fallback);
        }
        assert!(super::root().is_err());
    }
}
