//! 容器状态的**唯一显示口径**（`PRD.md` F9.34）。
//!
//! # 为什么要单独一处
//!
//! `ps`、`inspect`、`compose ps` 三处都要把「这个容器现在处于什么状态」变成一个
//! 字符串给人看。此前三处各写了一份 `match liveness { … }`——于是 F9.32 给
//! `ps`/`inspect` 补上 `paused` 之后，`compose ps` 仍然把暂停的容器显示成
//! `running`。**分头维护的映射必然会漂**，而且漏掉的那一处不会有任何提示。
//!
//! 这里只做一件事：把 `Liveness` 加上「是否被暂停」这一维，折成一个标签。
//! 三处调用同一个函数，以后再加状态（比如 `restarting`）也只改这一个地方。

use crate::runstate::Liveness;
use std::path::Path;

/// 容器状态标签，与 docker 的取值对齐（`created`/`running`/`paused`/`exited`）。
///
/// `paused` 只可能出现在 `Running` 之上：暂停的是**正在运行**的容器的进程，
/// 记录本身仍然是活的（owner 锁还握着）。
pub(super) fn label(dir: &Path, live: Liveness) -> &'static str {
    match live {
        Liveness::Created => "created",
        Liveness::Exited => "exited",
        Liveness::Running => {
            if super::pause::is_paused(dir) {
                "paused"
            } else {
                "running"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    /// 非运行状态不必去看进程，直接给标签——顺带保证 `label` 对一个根本不存在
    /// 的目录也不会炸（`ps` 遍历时目录可能刚被 `rm` 掉）。
    #[test]
    fn non_running_states_map_directly() {
        let _home = TempHome::new("statuslabel");
        let missing = std::path::Path::new("/definitely/not/here");
        assert_eq!(label(missing, Liveness::Created), "created");
        assert_eq!(label(missing, Liveness::Exited), "exited");
        // 运行中但取不到容器 PID（记录不全）→ 不能凭空说 paused
        assert_eq!(label(missing, Liveness::Running), "running");
    }
}
