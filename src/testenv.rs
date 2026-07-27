//! 测试专用脚手架：进程环境变量的互斥访问 + 自动还原。
//!
//! 背景：`std::env::set_var/remove_var` 改的是**进程级**全局状态，而 `cargo test`
//! 默认在同一进程内多线程并行跑用例。任何两个改环境的用例都会互踩——典型故障是
//! A 用例把 `HOME` 指向自己的临时目录，B 用例随后按 `HOME` 定位缓存却读到 A 的
//! 目录（甚至读到一半被 A 的清理删掉）。
//!
//! 因此本模块提供唯一的环境改动入口 [`EnvGuard`]：
//! - **互斥**：构造时取全局锁，同一时刻只有一个用例能改环境；
//! - **还原**：记录每个键的原值（含"原本不存在"），Drop 时逐一回滚，杜绝泄漏。
//!
//! 约定：测试中**不要**再直接调用 `std::env::set_var/remove_var`；需要临时环境
//! 变量一律经 `EnvGuard`。需要临时 HOME 的用例用本模块的 [`TempHome`]（内部持有同一
//! 把守卫）；若还要额外变量，经 `TempHome::env()` 借出**同一个**守卫——重复
//! 构造 `EnvGuard` 会自死锁。

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

/// 进程环境的全局互斥。粒度=整个环境（而非单键），因为读取侧
/// （`build_child_env` 的宿主环境全量快照）本就依赖整体一致性。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 环境变量的独占访问守卫：持有期间独占进程环境，Drop 时还原所有改动。
pub(crate) struct EnvGuard {
    /// `None` = 嵌套守卫（锁由外层持有，见 [`EnvGuard::nested`]）。
    _lock: Option<MutexGuard<'static, ()>>,
    /// 键 → 改动前的原值（`None` = 原本不存在，还原时删除）。
    saved: BTreeMap<String, Option<OsString>>,
}

impl EnvGuard {
    pub fn new() -> Self {
        // 忽略中毒：被保护的是"进程环境"这一外部状态，不会因上一个用例 panic
        // 而处于逻辑损坏状态（且那个用例的 EnvGuard 已在展开中完成还原）。
        // 若在此 unwrap，一个用例失败会连坐后续所有改环境的用例。
        let lock = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        Self { _lock: Some(lock), saved: BTreeMap::new() }
    }

    /// 嵌套守卫：**调用方所在线程已持有**外层守卫时使用（不再加锁，否则自死锁）。
    /// 用于在外层作用域内再开一段"改动后即刻还原"的小作用域。
    #[allow(dead_code)] // 目前仅本模块自测使用；保留为通用原语
    pub fn nested() -> Self {
        Self { _lock: None, saved: BTreeMap::new() }
    }

    /// 设置环境变量（首次触碰该键时记录原值）。
    pub fn set(&mut self, key: &str, val: impl AsRef<OsStr>) {
        self.remember(key);
        std::env::set_var(key, val);
    }

    /// 删除环境变量（首次触碰该键时记录原值）。
    pub fn remove(&mut self, key: &str) {
        self.remember(key);
        std::env::remove_var(key);
    }

    /// 只记录**第一次**触碰前的值：同一键多次改动后仍还原到用例开始前的状态。
    fn remember(&mut self, key: &str) {
        self.saved
            .entry(key.to_string())
            .or_insert_with(|| std::env::var_os(key));
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, original) in &self.saved {
            match original {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// 测试共享脚手架：临时 HOME（构造期间把 HOME 指向独立临时目录，
/// Drop 时恢复并清理；cache_root 优先 USERPROFILE，故同时摘掉它）。
///
/// HOME 是**进程级**状态：并行用例各自指向自己的临时目录必然互踩（且一方的
/// 清理会删掉另一方正在读的文件）。因此内部持有 [`crate::testenv::EnvGuard`]，
/// 存活期间独占进程环境——用例之间由此天然串行。需要额外临时环境变量时，
/// 经 [`TempHome::env`] 借出**同一把**守卫，切勿另起 `EnvGuard`（会自死锁）。
#[cfg(test)]
pub(crate) struct TempHome {
    pub dir: std::path::PathBuf,
    env: crate::testenv::EnvGuard,
}

#[cfg(test)]
impl TempHome {
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "wbox-test-home-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut env = crate::testenv::EnvGuard::new();
        env.set("HOME", &dir);
        env.remove("USERPROFILE");
        Self { dir, env }
    }

    /// 借出内部环境守卫：用例需要额外临时环境变量时经它设置（Drop 时一并还原）。
    pub fn env(&mut self) -> &mut crate::testenv::EnvGuard {
        &mut self.env
    }

    /// 在缓存布局中安放一个假镜像（rootfs + 元数据），返回缓存目录。
    pub fn plant_fake_image(&self, registry: &str, name_flat: &str, tag: &str) -> std::path::PathBuf {
        let dir = self
            .dir
            .join(".wbox/images")
            .join(registry)
            .join(name_flat)
            .join(tag);
        std::fs::create_dir_all(dir.join("rootfs/bin")).unwrap();
        std::fs::write(dir.join("rootfs/bin/sh"), b"fake").unwrap();
        std::fs::write(dir.join("manifest.json"), b"{}").unwrap();
        std::fs::write(dir.join("layers.json"), r#"["sha256:l1","sha256:l2"]"#).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"config":{"Env":["PATH=/usr/bin","APP_TOKEN=hunter2"],"Cmd":["-l"],"Entrypoint":["/bin/sh"],"WorkingDir":"/root"}}"#,
        )
        .unwrap();
        dir
    }
}

#[cfg(test)]
impl Drop for TempHome {
    fn drop(&mut self) {
        // 只清目录；环境变量由字段 `env` 的 Drop 还原（本 Drop 先于字段执行，
        // 故清理期间 HOME 仍指向本目录，不会误删他人）。
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 原本不存在的键：改动后 Drop 必须删除（不留泄漏）。
    #[test]
    fn absent_key_is_removed_on_drop() {
        assert!(std::env::var_os("WBOX_TESTENV_ABSENT").is_none());
        {
            let mut g = EnvGuard::new();
            g.set("WBOX_TESTENV_ABSENT", "v");
            assert_eq!(std::env::var("WBOX_TESTENV_ABSENT").unwrap(), "v");
        }
        assert!(std::env::var_os("WBOX_TESTENV_ABSENT").is_none());
    }

    /// 原本存在的键：多次覆盖后 Drop 必须还原到**最初**值，不是上一次值。
    #[test]
    fn preexisting_key_restored_to_original_value() {
        let mut outer = EnvGuard::new();
        outer.set("WBOX_TESTENV_PRE", "original");
        {
            let mut g = EnvGuard::nested(); // 外层已持锁，嵌套改动
            g.set("WBOX_TESTENV_PRE", "first");
            g.set("WBOX_TESTENV_PRE", "second");
            assert_eq!(std::env::var("WBOX_TESTENV_PRE").unwrap(), "second");
        }
        assert_eq!(std::env::var("WBOX_TESTENV_PRE").unwrap(), "original");
    }

    /// remove 分支同样被记录与还原。
    #[test]
    fn removed_key_restored_on_drop() {
        let mut outer = EnvGuard::new();
        outer.set("WBOX_TESTENV_RM", "keepme");
        {
            let mut g = EnvGuard::nested();
            g.remove("WBOX_TESTENV_RM");
            assert!(std::env::var_os("WBOX_TESTENV_RM").is_none());
        }
        assert_eq!(std::env::var("WBOX_TESTENV_RM").unwrap(), "keepme");
    }
}
