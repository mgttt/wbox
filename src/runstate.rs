//! 运行中容器的状态目录与发现（`PRD.md` F8.a）。
//!
//! 目录布局：`~/.wbox/run/<name>/`
//! - `meta.json`：容器规格摘要（名字、pid、命令行、目标、创建时间）；
//! - `lock`：**由运行中的 wbox 独占持有**的空文件，存活判定全靠它。
//!
//! # 为什么用锁文件而不是 pid
//!
//! pid 会被复用。拿"`/proc/<pid>` 还在"当存活判据，在一台繁忙机器上迟早会把
//! 别人的进程认成自己的容器——而 `stop` 一旦据此发信号，就是杀错进程。
//!
//! 起初打算用 pid + `/proc/<pid>/stat` 的 starttime 做二元组来识破复用
//! （这是常见做法），但它有两个缺点：一是 Linux 专有，Windows 要另写一套
//! `GetProcessTimes`；二是那套 Windows 代码我在本机无从验证。
//!
//! 锁文件同时解决这两点：**两个平台的语义都由操作系统保证**——
//! 进程无论怎么死（正常退出、崩溃、被 SIGKILL），内核都会关掉它的 fd/句柄，
//! 锁随之释放。于是"能拿到锁"精确等价于"没有活着的 owner"，不存在 pid 复用
//! 这类误判，也不需要清理钩子跑到才算数。而且同一套单测能同时覆盖两侧实现
//! （见本文件末尾的 `lock_reflects_owner_liveness`），Windows 那半由 CI 的
//! windows runner 真实执行，不是我拍脑袋写完就算。

use crate::error::{Result, WboxError};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const OPERATION_LOCK: &str = ".operations.lock";
const DETACHED_RESERVATION: &str = ".detached-reservation";
const CREATED_MARKER: &str = ".created";
const CREATE_CONFIG: &str = "create.json";
const EXIT_CODE: &str = "exit-code";
const OPERATION_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// 状态根目录：`~/.wbox/run`（与镜像缓存同在 `~/.wbox` 下，便于统一清理）。
pub fn run_root() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| WboxError::args("无法确定用户主目录（USERPROFILE/HOME 均未设置）"))?;
    Ok(PathBuf::from(home).join(".wbox").join("run"))
}

/// 容器名 → 状态目录。名字已由 `validate_container_name` 校验过长度，
/// 这里再挡一次路径分隔符：`--name ../x` 不能把状态目录写到别处去。
pub fn dir_for(name: &str) -> Result<PathBuf> {
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(WboxError::args(format!(
            "容器名 '{}' 含路径分隔符，不能用作状态目录名",
            name
        )));
    }
    Ok(run_root()?.join(name))
}

/// 状态目录里记录的容器摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// 记录用途：展示与排查。**不用于存活判定**（见模块文档）。
    pub pid: u32,
    /// Unix 秒。不引 chrono——只为了显示"跑了多久"，精度到秒足够。
    pub created_unix: u64,
    /// guest 命令行（已合并 Entrypoint/Cmd）
    pub cmd: Vec<String>,
    /// 运行目标：镜像引用，或宿主程序模式下的 `(native)`
    pub target: String,
    /// 运行实例是否已进入不可再附着的停止阶段。旧版记录缺失此字段时按
    /// `running` 读取；最终 `exited` 仍由 owner 锁是否释放判定。
    pub stopping: bool,
    /// `exec` 重建隔离边界所需的最小上下文。刻意不存环境变量，避免把凭证
    /// 或调用方秘密落到用户目录下的状态文件中。
    pub exec_context: Option<ExecContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecContext {
    pub allow_network: bool,
    pub workdir: String,
}

impl Entry {
    fn to_json(&self) -> String {
        let exec_context = self.exec_context.as_ref().map(|context| {
            serde_json::json!({
                "allow_network": context.allow_network,
                "workdir": context.workdir,
            })
        });
        let v = serde_json::json!({
            "name": self.name,
            "pid": self.pid,
            "created_unix": self.created_unix,
            "cmd": self.cmd,
            "target": self.target,
            "lifecycle": if self.stopping { "stopping" } else { "running" },
            "exec_context": exec_context,
        });
        v.to_string()
    }

    /// 解析 `meta.json`。字段缺失/类型不对一律返回 `None`——状态目录可能被
    /// 用户手改或被上一版 wbox 写过，读不懂就当这条不存在，不要让 `ps` 崩掉。
    fn from_json(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let exec_context = v.get("exec_context").and_then(|context| {
            if context.is_null() {
                return None;
            }
            Some(ExecContext {
                allow_network: context.get("allow_network")?.as_bool()?,
                workdir: context.get("workdir")?.as_str()?.to_string(),
            })
        });
        Some(Entry {
            name: v.get("name")?.as_str()?.to_string(),
            pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            created_unix: v.get("created_unix")?.as_u64()?,
            cmd: v
                .get("cmd")?
                .as_array()?
                .iter()
                .map(|x| x.as_str().unwrap_or_default().to_string())
                .collect(),
            target: v.get("target")?.as_str()?.to_string(),
            stopping: v
                .get("lifecycle")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == "stopping"),
            exec_context,
        })
    }
}

/// 容器存活状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// 已保存配置但尚无运行中的 owner
    Created,
    /// 锁被占住 → 有活着的 owner
    Running,
    /// 锁能拿到 → owner 已消失（正常退出没来得及清理，或崩溃）
    Exited,
}

/// 独占打开一个文件。成功即表示**此刻没有别人持有它**。
///
/// Linux 用 `flock(LOCK_EX|LOCK_NB)`：锁绑定在 *open file description* 上，
/// 因此同一进程再打开一次也会冲突——这正是我们要的（单测就在同进程内验证）。
/// 换成 `fcntl` 记录锁则不行：那是按进程算的，同进程重复加锁会直接成功。
#[cfg(unix)]
fn try_lock_exclusive(path: &Path) -> Option<File> {
    use std::os::unix::io::AsRawFd;
    let f = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .ok()?;
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Some(f)
    } else {
        None
    }
}

/// 串行化状态树的复合变更。单个容器的 `lock` 证明 owner 是否存活，但不能保护
/// “检查存活状态 → 删除旧目录/创建新目录”这一整段操作；没有根锁时，`rm`
/// 可以在 `register` 刚取得容器锁后删掉它的目录。
fn lock_operations(root: &Path) -> Result<File> {
    std::fs::create_dir_all(root)
        .map_err(|e| WboxError::args(format!("创建状态根目录 '{}' 失败：{}", root.display(), e)))?;
    let path = root.join(OPERATION_LOCK);
    let started = Instant::now();
    loop {
        if let Some(lock) = try_lock_exclusive(&path) {
            return Ok(lock);
        }
        if started.elapsed() >= OPERATION_LOCK_TIMEOUT {
            return Err(WboxError::args(format!(
                "等待状态操作锁 '{}' 超时（是否有卡住的 wbox？）",
                path.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Windows：`share_mode(0)` 表示打开期间不允许他人共享访问，
/// 别的进程再开就会拿到共享冲突——与 flock 等效。
#[cfg(windows)]
fn try_lock_exclusive(path: &Path) -> Option<File> {
    use std::os::windows::fs::OpenOptionsExt;
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
        .ok()
}

/// 判定一个状态目录的存活状态。
pub fn liveness(dir: &Path) -> Liveness {
    // detached 父进程先在操作锁内预留名字，supervisor 随后凭令牌接管。
    // 预留期间必须按 Running 处理，否则并发 run/rm 会把它正在准备的目录删掉。
    if dir.join(DETACHED_RESERVATION).is_file() {
        return Liveness::Running;
    }
    if dir.join(CREATED_MARKER).is_file() {
        return Liveness::Created;
    }
    let lock = dir.join("lock");
    if !lock.exists() {
        // 没有锁文件：要么根本没登记完，要么被清理过——都按已亡处理
        return Liveness::Exited;
    }
    match try_lock_exclusive(&lock) {
        // 能独占 → 没有 owner。拿到后立刻释放（drop File）
        Some(_) => Liveness::Exited,
        None => Liveness::Running,
    }
}

/// 一次运行的登记。**RAII**：drop 时释放锁，并按 `persist` 决定是否清目录。
///
/// 崩溃时 drop 不会执行，状态目录会残留——这是刻意接受的：锁已随进程消失，
/// [`liveness`] 会把它判为 `Exited`，`ps` 如实显示，`rm` 负责清理。
/// 与其为"保证清理"引入守护进程，不如让残留可见且可解释。
#[derive(Debug)]
pub struct Registration {
    dir: PathBuf,
    /// 持有到 drop 为止；这个句柄就是"我还活着"的证据
    lock: Option<File>,
    /// 退出后**保留**记录与日志（`--detach` 用）。
    ///
    /// 前台容器退出即清理：输出已经打在用户终端上了，留个空目录只是垃圾。
    /// 后台容器相反——用户回头查 `wbox logs` 正是为了看它到底输出了什么，
    /// 一退出就把日志删掉等于把这个命令的主要用途废掉。因此后台记录保留为
    /// `exited`，由 `wbox rm` 显式清理（与 docker 的 ps -a / rm 一致）。
    persist: bool,
}

impl Registration {
    /// 状态目录路径。Windows OCI 的私有可写 rootfs 也放在这里，使前台退出、
    /// `--rm` 和显式 `wbox rm` 都复用同一生命周期清理路径。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `--rm` supervisor 从登记成功起就在任意错误出口清理状态树。
    pub fn remove_on_drop(&mut self) {
        self.persist = false;
    }
}

/// detached 父进程对容器名的原子预留。只有拿到同一令牌的 supervisor 能接管。
pub struct DetachedReservation {
    dir: PathBuf,
    token: String,
    armed: bool,
}

impl DetachedReservation {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DetachedReservation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(root) = self.dir.parent() else {
            return;
        };
        if let Ok(_operation_lock) = lock_operations(root) {
            let reservation = self.dir.join(DETACHED_RESERVATION);
            if std::fs::read_to_string(&reservation).ok().as_deref() == Some(&self.token) {
                if self.dir.join(CREATE_CONFIG).is_file() {
                    restore_created_dir(&self.dir);
                } else {
                    purge_dir(&self.dir);
                }
            }
        }
    }
}

/// 在根操作锁内检查旧状态、清理已退出残留并预留 detached 名称。
pub fn reserve_detached(name: &str) -> Result<DetachedReservation> {
    let dir = dir_for(name)?;
    let root = dir
        .parent()
        .ok_or_else(|| WboxError::args("状态目录缺少根目录"))?;
    let _operation_lock = lock_operations(root)?;
    if dir.exists() {
        if dir.join(CREATE_CONFIG).is_file() {
            return Err(WboxError::args(format!(
                "容器名 '{}' 已有可 start 的保存配置；请执行 `wbox start {}` 或先 rm",
                name, name
            )));
        }
        match liveness(&dir) {
            Liveness::Created => {
                return Err(WboxError::args(format!(
                    "容器名 '{}' 已由 create 使用；请执行 `wbox start {}` 或先 rm",
                    name, name
                )));
            }
            Liveness::Running => {
                return Err(WboxError::args(format!(
                    "容器名 '{}' 正在使用中。换个 --name，或先停掉正在运行的那个",
                    name
                )));
            }
            Liveness::Exited => purge_dir(&dir),
        }
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| WboxError::args(format!("创建状态目录 '{}' 失败：{}", dir.display(), e)))?;
    let token = next_reservation_token();
    std::fs::write(dir.join(DETACHED_RESERVATION), &token)
        .map_err(|e| WboxError::args(format!("写入 detached 预留令牌失败：{}", e)))?;
    Ok(DetachedReservation {
        dir,
        token,
        armed: true,
    })
}

fn next_reservation_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_RESERVATION: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}-{}",
        std::process::id(),
        now_unix(),
        NEXT_RESERVATION.fetch_add(1, Ordering::Relaxed)
    )
}

/// 保存一个尚未运行的容器配置。配置文件只对当前用户可见；显式 `-e` 值会像
/// Docker/Podman 的容器配置一样持久化，因此调用方不应把长期凭证放进参数。
pub fn create_pending(
    name: &str,
    args: &[String],
    cmd: &[String],
    target: &str,
    exec_context: Option<ExecContext>,
) -> Result<()> {
    let dir = dir_for(name)?;
    let root = dir
        .parent()
        .ok_or_else(|| WboxError::args("状态目录缺少根目录"))?;
    let _operation_lock = lock_operations(root)?;
    if dir.exists() {
        return Err(WboxError::args(format!(
            "容器名 '{}' 已存在；请换名或先执行 `wbox rm {}`",
            name, name
        )));
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| WboxError::args(format!("创建状态目录 '{}' 失败：{}", dir.display(), e)))?;
    let result = (|| {
        let config = serde_json::json!({
            "schema": 1,
            "run_args": args,
        });
        let config_path = dir.join(CREATE_CONFIG);
        std::fs::write(&config_path, config.to_string())
            .map_err(|e| WboxError::args(format!("写 create.json 失败：{}", e)))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| WboxError::args(format!("限制 create.json 权限失败：{}", e)))?;
        }
        let entry = Entry {
            name: name.to_string(),
            pid: 0,
            created_unix: now_unix(),
            cmd: cmd.to_vec(),
            target: target.to_string(),
            stopping: false,
            exec_context,
        };
        let mut meta = serde_json::from_str::<serde_json::Value>(&entry.to_json())
            .map_err(|e| WboxError::args(format!("生成 created 元数据失败：{}", e)))?;
        meta["lifecycle"] = serde_json::Value::String("created".to_string());
        std::fs::write(dir.join("meta.json"), meta.to_string())
            .map_err(|e| WboxError::args(format!("写 meta.json 失败：{}", e)))?;
        std::fs::write(dir.join(CREATED_MARKER), b"")
            .map_err(|e| WboxError::args(format!("写 created 状态标记失败：{}", e)))
    })();
    if result.is_err() {
        purge_dir(&dir);
    }
    result
}

fn read_create_args(dir: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(dir.join(CREATE_CONFIG))
        .map_err(|_| WboxError::args("容器没有可重用的 create 配置"))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| WboxError::args(format!("create.json 不可读：{}", e)))?;
    value
        .get("run_args")
        .and_then(|args| args.as_array())
        .ok_or_else(|| WboxError::args("create.json 缺少 run_args"))?
        .iter()
        .map(|arg| {
            arg.as_str()
                .map(str::to_string)
                .ok_or_else(|| WboxError::args("create.json 的 run_args 含非字符串值"))
        })
        .collect()
}

/// 删除一次运行产生的临时状态，但保留 create 配置与展示元数据。
fn restore_created_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == CREATE_CONFIG || name == "meta.json" || name == CREATED_MARKER {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(path);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
    let _ = std::fs::write(dir.join(CREATED_MARKER), b"");
}

/// 原子领取 created/exited 容器的保存配置，转换为 detached 启动预留。
pub fn activate_created(name: &str) -> Result<(Vec<String>, DetachedReservation)> {
    let dir = dir_for(name)?;
    let root = dir
        .parent()
        .ok_or_else(|| WboxError::args("状态目录缺少根目录"))?;
    let _operation_lock = lock_operations(root)?;
    if !dir.exists() {
        return Err(missing_record(name));
    }
    if liveness(&dir) == Liveness::Running {
        return Err(WboxError::args(format!("容器 '{}' 已在运行", name)));
    }
    let args = read_create_args(&dir)?;
    restore_created_dir(&dir);
    let token = next_reservation_token();
    std::fs::write(dir.join(DETACHED_RESERVATION), &token)
        .map_err(|e| WboxError::args(format!("写 start 预留令牌失败：{}", e)))?;
    Ok((
        args,
        DetachedReservation {
            dir,
            token,
            armed: true,
        },
    ))
}

/// supervisor 在开始 pull/prepare 前接过清理责任；若尚未完成登记就报错退出，
/// guard 会删除预留及日志，避免名称永久卡在“运行中”。
pub fn adopt_detached(name: &str, token: &str) -> Result<DetachedReservation> {
    let dir = dir_for(name)?;
    let root = dir
        .parent()
        .ok_or_else(|| WboxError::args("状态目录缺少根目录"))?;
    let _operation_lock = lock_operations(root)?;
    let actual = std::fs::read_to_string(dir.join(DETACHED_RESERVATION))
        .map_err(|_| WboxError::args(format!("容器 '{}' 的 detached 预留不存在", name)))?;
    if actual != token {
        return Err(WboxError::args(format!(
            "容器 '{}' 的 detached 预留令牌不匹配",
            name
        )));
    }
    Ok(DetachedReservation {
        dir,
        token: token.to_string(),
        armed: true,
    })
}

/// 删掉一个状态目录的全部内容。**调用前必须确认没有 owner 持锁**
/// （Drop 里是自己刚放的手，其余路径靠 [`liveness`] 判定）。
///
/// 尽力而为、不报错：清理失败顶多留下一条 `Exited` 记录，比 panic 或让
/// 调用方多一条错误分支都好。
fn purge_dir(dir: &Path) {
    // **整棵删掉，不逐个列举已知产物。**
    //
    // 这里原先是"删 meta.json、删 lock、删两个日志、删 container.pid、删
    // exit code、删预留文件、递归删 rootfs，最后 remove_dir"——每新增一种
    // 状态文件就得记得回来补一行，而漏掉的后果很隐蔽：`remove_dir` 因非空
    // 静默失败，状态目录永远留着，`ps -a` 里挂一条清不掉的记录。本轮给
    // wineprefix 找落脚处时正好又要加第九个，与其继续列举不如一次删干净。
    //
    // 安全性由调用方保证：`dir` 只能来自 [`dir_for`]（已挡掉路径分隔符与
    // `..`），因此必定落在状态根目录之下；guest 无法把它指到别处。
    // Windows 上还要求先关掉 owner 锁句柄，否则文件仍被占用——`Drop` 已经
    // 先 `drop(lock)` 再调本函数。
    let _ = std::fs::remove_dir_all(dir);
}

impl Drop for Registration {
    fn drop(&mut self) {
        let root = self.dir.parent().map(Path::to_path_buf);
        // 先持有根操作锁、再释放 owner 锁：exec/stop/rm 若先取得根锁会完成
        // 本轮核对后退出；本路径若先取得根锁，则同名 register 无法插入
        // “释放 owner 锁 → 旧 Job/状态尚未收尾”的窗口。
        if let Some(root) = root {
            if let Ok(_operation_lock) = lock_operations(&root) {
                // Windows 不允许删除仍被打开的锁文件，必须先显式关闭句柄。
                drop(self.lock.take());
                if !self.persist {
                    purge_dir(&self.dir);
                }
                return;
            }
        }
        // 取根锁失败时宁可留下可识别的 Exited 记录，也不能无锁删除并发登记。
        drop(self.lock.take());
    }
}

/// 日志文件名（`--detach` 时 guest 的 stdout/stderr 落盘处）。
pub const LOG_STDOUT: &str = "stdout.log";
pub const LOG_STDERR: &str = "stderr.log";

/// 日志体积上限。超过后**丢旧留新**并在截断处留标记——见 [`enforce_log_cap`]。
/// 默认 8 MiB；`WBOX_LOG_MAX_BYTES` 可覆盖（测试用小值，不必写满 8MB）。
pub fn log_cap_bytes() -> u64 {
    std::env::var("WBOX_LOG_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(8 * 1024 * 1024)
}

/// 以**追加**方式打开日志文件。追加是关键：[`enforce_log_cap`] 靠截断控制
/// 体积，而只有 append 模式的写入才会在截断后从新的文件末尾（0）继续；
/// 普通写模式下 writer 自带偏移，截断后文件立刻变回原大小的稀疏文件，
/// 上限就形同虚设。
pub fn open_log_append(dir: &Path, file: &str) -> Result<File> {
    File::options()
        .create(true)
        .append(true)
        .open(dir.join(file))
        .map_err(|e| WboxError::args(format!("打开日志 '{}' 失败：{}", file, e)))
}

/// 超上限则清空并写入一行截断说明。
///
/// 这是**丢旧留新**的粗粒度轮转，不是精细的按行轮转：容器日志的价值绝大多数
/// 在最近的输出，而实现精确轮转要在 wbox 与 guest 之间插一层转写，代价远大于
/// 收益。关键是两条：体积有界，且**截断这件事对用户可见**——无声丢日志比
/// 日志不全更糟。
pub fn enforce_log_cap(dir: &Path, file: &str) {
    let path = dir.join(file);
    let Ok(md) = std::fs::metadata(&path) else {
        return;
    };
    let cap = log_cap_bytes();
    if md.len() <= cap {
        return;
    }
    // 截断到 0；append 写入方随后从头继续（见 open_log_append 的说明）
    if let Ok(f) = File::options().write(true).open(&path) {
        let _ = f.set_len(0);
        use std::io::Write;
        let mut f = f;
        let _ = writeln!(
            f,
            "[wbox] 日志超过 {} 字节上限，已丢弃此前内容（保留最新输出）",
            cap
        );
    }
}

/// 在 owner 锁释放前持久化 guest 退出码，供 `wait` / `inspect` 读取。
pub fn record_exit_code(dir: &Path, code: u32) -> Result<()> {
    std::fs::write(dir.join(EXIT_CODE), code.to_string())
        .map_err(|e| WboxError::args(format!("写容器退出码失败：{}", e)))
}

/// 尚未退出或旧版记录没有退出码时返回 `None`；畸形文件也按未知处理。
pub fn read_exit_code(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join(EXIT_CODE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// [`register_with`] 的简写（不保留日志）。生产路径一律走 `register_with`
/// ——它要按是否 supervised 决定日志去留——故此处只剩测试在用。
#[cfg(test)]
pub fn register(name: &str, cmd: &[String], target: &str) -> Result<Registration> {
    register_with(name, cmd, target, false)
}

/// [`register`] 的完整形式。`keep_logs=true` 时不清除既有日志——
/// `--detach` 流程里日志文件是**父进程**先建好、再作为子进程 stdio 传下去的，
/// 子进程随后才登记；此时若按"复用残留"的常规逻辑清目录，就会把父进程刚建好
/// 的日志一起删掉，容器还没开口就先失声。
#[cfg(test)]
pub fn register_with(
    name: &str,
    cmd: &[String],
    target: &str,
    keep_logs: bool,
) -> Result<Registration> {
    register_with_context(name, cmd, target, keep_logs, None, None)
}

pub fn register_with_context(
    name: &str,
    cmd: &[String],
    target: &str,
    keep_logs: bool,
    exec_context: Option<ExecContext>,
    reservation_token: Option<&str>,
) -> Result<Registration> {
    let dir = dir_for(name)?;
    let root = dir
        .parent()
        .ok_or_else(|| WboxError::args("状态目录缺少根目录"))?;
    let _operation_lock = lock_operations(root)?;
    if dir.exists() && keep_logs {
        // detach 的子进程只允许凭父进程写下的同一令牌接管。
        let expected = reservation_token.ok_or_else(|| {
            WboxError::args(format!("容器 '{}' 缺少 detached 预留令牌", name))
        })?;
        let reservation = dir.join(DETACHED_RESERVATION);
        let actual = std::fs::read_to_string(&reservation).map_err(|_| {
            WboxError::args(format!("容器 '{}' 的 detached 预留不存在或不可读", name))
        })?;
        if actual != expected {
            return Err(WboxError::args(format!(
                "容器 '{}' 的 detached 预留令牌不匹配（并发的 wbox？）",
                name
            )));
        }
        let lock = try_lock_exclusive(&dir.join("lock")).ok_or_else(|| {
            WboxError::args(format!("无法独占容器 '{}' 的锁文件（并发的 wbox？）", name))
        })?;
        write_meta(&dir, name, cmd, target, exec_context)?;
        if dir.join(CREATED_MARKER).is_file() {
            std::fs::remove_file(dir.join(CREATED_MARKER))
                .map_err(|e| WboxError::args(format!("切换 created 状态失败：{}", e)))?;
        }
        std::fs::remove_file(&reservation)
            .map_err(|e| WboxError::args(format!("接管 detached 预留失败：{}", e)))?;
        return Ok(Registration {
            dir,
            lock: Some(lock),
            // detach：退出后保留，供 `wbox logs` 事后查看
            persist: true,
        });
    }
    if dir.exists() {
        if dir.join(CREATE_CONFIG).is_file() {
            return Err(WboxError::args(format!(
                "容器名 '{}' 已有可 start 的保存配置；请执行 `wbox start {}` 或先 rm",
                name, name
            )));
        }
        match liveness(&dir) {
            Liveness::Created => {
                return Err(WboxError::args(format!(
                    "容器名 '{}' 已由 create 使用；请执行 `wbox start {}` 或先 rm",
                    name, name
                )));
            }
            Liveness::Running => {
                return Err(WboxError::args(format!(
                    // 别在这里建议 `wbox rm`：已退出的残留在上面就被自动复用了，
                    // 所以能走到这条分支的**一定是运行中的**，而 rm 明确拒绝
                    // 运行中的容器——那样等于指使用户去撞一堵墙。
                    "容器名 '{}' 正在使用中。换个 --name，或先停掉正在运行的那个",
                    name
                )));
            }
            // 已亡的残留：可以安全复用，直接清掉重来
            Liveness::Exited => purge_dir(&dir),
        }
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| WboxError::args(format!("创建状态目录 '{}' 失败：{}", dir.display(), e)))?;
    let lock = try_lock_exclusive(&dir.join("lock")).ok_or_else(|| {
        WboxError::args(format!(
            "无法独占容器 '{}' 的锁文件（是否有并发的 wbox？）",
            name
        ))
    })?;
    let entry = Entry {
        name: name.to_string(),
        pid: std::process::id(),
        created_unix: now_unix(),
        cmd: cmd.to_vec(),
        target: target.to_string(),
        stopping: false,
        exec_context,
    };
    write_meta_entry(&dir, entry)?;
    Ok(Registration {
        dir,
        lock: Some(lock),
        persist: false,
    })
}

fn write_meta(
    dir: &Path,
    name: &str,
    cmd: &[String],
    target: &str,
    exec_context: Option<ExecContext>,
) -> Result<()> {
    write_meta_entry(
        dir,
        Entry {
            name: name.to_string(),
            pid: std::process::id(),
            created_unix: now_unix(),
            cmd: cmd.to_vec(),
            target: target.to_string(),
            stopping: false,
            exec_context,
        },
    )
}

fn write_meta_entry(dir: &Path, entry: Entry) -> Result<()> {
    std::fs::write(dir.join("meta.json"), entry.to_json())
        .map_err(|e| WboxError::args(format!("写 meta.json 失败：{}", e)))
}

/// 读取状态目录里的 `meta.json`。读不懂返回 `None`——状态目录是用户可见的
/// 普通目录，手改、残留、跨版本都可能出现，调用方按"这条不存在"处理即可。
///
/// **`meta.json` 只能由这一处解析**：`stop` 曾自己手写一份取 pid 的 JSON 解析，
/// 于是同一个文件有了两个解析器；字段一改就得记得两边都改，而漏掉的那边只会
/// 在运行时安静地取到 `None`。
pub fn read_meta(dir: &Path) -> Option<Entry> {
    Entry::from_json(&std::fs::read_to_string(dir.join("meta.json")).ok()?)
}

/// 解析容器名并要求该记录**存在**，返回状态目录。
///
/// `logs` / `stop` / `exec` / `rm` 开头都要做同一件事：名字合法性（挡路径分隔符）
/// 加"有没有这条记录"，且报错文案必须一致——四份各写一遍时，任何一处措辞改动
/// 都会让用户看到自相矛盾的提示。存活与否**不在这里判**：三个命令对"已退出"
/// 的态度根本不同（`stop` 视作成功、`exec` 必须拒绝、`rm` 恰恰只接受它），
/// 硬凑成一个函数只会逼调用方传布尔开关。
pub fn resolve_existing(name: &str) -> Result<PathBuf> {
    let dir = dir_for(name)?;
    if !dir.exists() {
        return Err(missing_record(name));
    }
    Ok(dir)
}

/// "查无此记录"的统一措辞。
///
/// 单独抽出来是因为它有**两个**产生点：[`resolve_existing`] 走无锁快路径，
/// 而 [`lock_existing`] 必须**先拿操作锁再判存在**（否则与并发 `stop` 有竞态），
/// 没法直接复用前者。两处各写一份文案，任何一处改动都会让用户在不同子命令下
/// 看到不同说法——所以复用的是措辞，不是控制流。
pub(crate) fn missing_record(name: &str) -> WboxError {
    WboxError::args(format!("没有名为 '{}' 的容器记录", name))
}

/// "容器已退出"的统一措辞（`exec` 两侧共用）。
///
/// Linux 与 Windows 的 `exec` 走完全不同的实现，但用户看到的这句话必须一致：
/// 它描述的是**容器状态**，与实现无关。此前两侧各写一份，已经分叉成
/// "已退出，无法 exec（namespace 已随之消失）"和"已退出，无法 exec"。
pub(crate) fn already_exited(name: &str) -> WboxError {
    WboxError::args(format!("容器 '{}' 已退出，无法 exec", name))
}

/// 持有状态根的操作锁，并返回同一代运行记录。调用方应只在核对实例并取得
/// 平台对象句柄（或递送终止）期间持有；绝不能覆盖整个 `exec` 命令生命周期，
/// 否则并发 `stop` 会被阻塞。
pub struct LockedEntry {
    pub dir: PathBuf,
    pub entry: Entry,
    _operation_lock: File,
}

impl LockedEntry {
    /// 在状态操作锁保护下发布停止意图。发布后新的 `exec` 必须拒绝附着；
    /// 调用方应在释放本锁前终止平台生命周期对象。
    #[cfg(windows)]
    pub fn mark_stopping(&mut self) -> Result<()> {
        if !self.entry.stopping {
            self.entry.stopping = true;
            write_meta_entry(&self.dir, self.entry.clone())?;
        }
        Ok(())
    }
}

pub fn lock_existing(name: &str) -> Result<LockedEntry> {
    let dir = dir_for(name)?;
    let root = dir
        .parent()
        .ok_or_else(|| WboxError::args("状态目录缺少根目录"))?;
    let operation_lock = lock_operations(root)?;
    if !dir.exists() {
        return Err(missing_record(name));
    }
    let entry = read_meta(&dir).ok_or_else(|| {
        WboxError::args(format!("容器 '{}' 的 meta.json 不可读", name))
    })?;
    Ok(LockedEntry {
        dir,
        entry,
        _operation_lock: operation_lock,
    })
}

/// 删除一条已退出的容器记录（`wbox rm`）。
///
/// **运行中的一律拒绝**：`rm` 只是删记录，并不会停掉容器；真删了只会让一个
/// 还在跑的容器从 `ps` 里消失，等于把它变成没人管得到的孤儿。要停容器是
/// F8.3 `stop` 的事，两件事不能混。
#[cfg(test)]
fn pause_remove_after_liveness_for_test() {
    let Some(sync) = std::env::var_os("WBOX_RUNSTATE_PAUSE_REMOVE").map(PathBuf::from) else {
        return;
    };
    std::fs::write(sync.join("remove-checked"), b"").unwrap();
    let release = sync.join("release-remove");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !release.exists() {
        assert!(
            Instant::now() < deadline,
            "等待 remove 测试同步信号超时：{}",
            release.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn remove(name: &str) -> Result<()> {
    let dir = dir_for(name)?;
    let root = dir
        .parent()
        .ok_or_else(|| WboxError::args("状态目录缺少根目录"))?;
    let _operation_lock = lock_operations(root)?;
    if !dir.exists() {
        return Err(missing_record(name));
    }
    match liveness(&dir) {
        Liveness::Running => Err(WboxError::args(format!(
            "容器 '{}' 仍在运行，拒绝删除记录（rm 不会停掉它）",
            name
        ))),
        Liveness::Created | Liveness::Exited => {
            #[cfg(test)]
            pause_remove_after_liveness_for_test();
            purge_dir(&dir);
            if dir.exists() {
                return Err(WboxError::args(format!(
                    "删除状态目录 '{}' 失败（检查权限或是否有进程占用）",
                    dir.display()
                )));
            }
            Ok(())
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 列出所有状态目录。目录不存在时返回空表（还没跑过任何容器）。
///
/// 读不懂的条目直接跳过而不是报错：状态目录是用户可见的普通目录，
/// 手改、残留、跨版本都可能出现，`ps` 不该因为其中一条坏掉就整个失败。
pub fn list() -> Result<Vec<(Entry, Liveness)>> {
    let root = run_root()?;
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let rd = std::fs::read_dir(&root)
        .map_err(|e| WboxError::args(format!("读取 '{}' 失败：{}", root.display(), e)))?;
    for ent in rd.flatten() {
        let dir = ent.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(entry) = read_meta(&dir) else {
            continue;
        };
        out.push((entry, liveness(&dir)));
    }
    // 按创建时间排序，最新的在后——与 `docker ps` 相反，但连续观察时更顺眼
    out.sort_by_key(|(e, _)| e.created_unix);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;
    use std::process::{Child, Command};

    /// 锁的语义就是存活判定的全部依据，两个平台都必须成立：
    /// 持有期间判 Running，释放后判 Exited。
    ///
    /// 这条测试**在 Windows CI 上同样执行**（test-windows job），所以
    /// `share_mode(0)` 那半实现不是"写完就算"，而是真被跑过。
    #[test]
    fn lock_reflects_owner_liveness() {
        let _home = TempHome::new("lock");
        let reg = register("c1", &["/bin/true".into()], "(native)").unwrap();
        let dir = reg.dir().to_path_buf();
        assert_eq!(liveness(&dir), Liveness::Running, "持锁期间应判为运行中");
        drop(reg);
        // drop 会删掉整个目录，故这里手工造一个"崩溃残留"：有锁文件但无人持有
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        assert_eq!(liveness(&dir), Liveness::Exited, "无人持锁时应判为已退出");
    }

    /// 正常退出后状态目录应当消失，不给 `ps` 留垃圾。
    #[test]
    fn registration_cleans_up_on_drop() {
        let _home = TempHome::new("cleanup");
        let dir = {
            let reg = register("c2", &["/bin/true".into()], "(native)").unwrap();
            let private_file = reg.dir().join("rootfs/var/lib/app/state");
            std::fs::create_dir_all(private_file.parent().unwrap()).unwrap();
            std::fs::write(&private_file, b"private").unwrap();
            reg.dir().to_path_buf()
        };
        assert!(!dir.exists(), "drop 后状态目录应已删除：{}", dir.display());
        assert!(list().unwrap().is_empty());
    }

    /// 同名且存活 → 必须报错，不能覆盖（PRD F8.c）。
    #[test]
    fn duplicate_running_name_is_rejected() {
        let _home = TempHome::new("dup");
        let _held = register("dup", &["/bin/true".into()], "(native)").unwrap();
        let err = register("dup", &["/bin/true".into()], "(native)").unwrap_err();
        let m = format!("{}", err);
        assert!(m.contains("正在使用中"), "{}", m);
        // 解法必须是**真能用的**那条：此时 `wbox rm` 会拒绝（容器还在跑），
        // 所以文案不该提它。
        assert!(m.contains("换个 --name"), "报错要给出可行解法：{}", m);
        assert!(!m.contains("wbox rm"), "不该建议一条必然被拒的命令：{}", m);
    }

    #[test]
    fn detached_reservation_blocks_competing_operations_and_requires_token() {
        let _home = TempHome::new("detached-reservation");
        let mut reservation = reserve_detached("reserved").unwrap();
        let dir = reservation.dir().to_path_buf();
        let token = reservation.token().to_string();
        assert_eq!(liveness(&dir), Liveness::Running);
        assert!(register("reserved", &["x".into()], "(native)").is_err());
        assert!(remove("reserved").is_err());
        assert!(
            register_with_context(
                "reserved",
                &["x".into()],
                "(native)",
                true,
                None,
                Some("wrong-token"),
            )
            .is_err()
        );

        let reg = register_with_context(
            "reserved",
            &["x".into()],
            "(native)",
            true,
            None,
            Some(&token),
        )
        .unwrap();
        reservation.disarm();
        assert!(!dir.join(DETACHED_RESERVATION).exists());
        assert_eq!(liveness(&dir), Liveness::Running);
        drop(reg);
        assert_eq!(liveness(&dir), Liveness::Exited);
        remove("reserved").unwrap();
    }

    #[test]
    fn adopted_reservation_cleans_up_before_registration_on_error() {
        let _home = TempHome::new("detached-adopt");
        let mut parent = reserve_detached("adopted").unwrap();
        let dir = parent.dir().to_path_buf();
        let token = parent.token().to_string();
        parent.disarm();
        drop(parent);

        {
            let _supervisor_guard = adopt_detached("adopted", &token).unwrap();
            assert!(dir.exists());
        }
        assert!(!dir.exists(), "未登记的 supervisor 退出后必须清理预留");
    }

    #[test]
    fn created_config_transitions_atomically_and_can_start_again() {
        let _home = TempHome::new("created-lifecycle");
        let args = vec![
            "--name".to_string(),
            "saved".to_string(),
            "--".to_string(),
            "echo".to_string(),
        ];
        create_pending(
            "saved",
            &args,
            &["echo".to_string()],
            "(native)",
            None,
        )
        .unwrap();
        let dir = dir_for("saved").unwrap();
        assert_eq!(liveness(&dir), Liveness::Created);
        assert_eq!(read_create_args(&dir).unwrap(), args);

        let (loaded, reservation) = activate_created("saved").unwrap();
        assert_eq!(loaded, args);
        assert_eq!(liveness(&dir), Liveness::Running);
        assert!(activate_created("saved").is_err(), "并发 start 必须被挡住");
        drop(reservation);
        assert_eq!(
            liveness(&dir),
            Liveness::Created,
            "启动前失败应恢复 created"
        );

        let (_, mut reservation) = activate_created("saved").unwrap();
        let token = reservation.token().to_string();
        let reg = register_with_context(
            "saved",
            &["echo".to_string()],
            "(native)",
            true,
            None,
            Some(&token),
        )
        .unwrap();
        reservation.disarm();
        assert_eq!(liveness(&dir), Liveness::Running);
        assert!(!dir.join(CREATED_MARKER).exists());
        drop(reg);
        assert_eq!(liveness(&dir), Liveness::Exited);

        let (_, reservation) = activate_created("saved").unwrap();
        assert_eq!(liveness(&dir), Liveness::Running);
        drop(reservation);
        assert_eq!(liveness(&dir), Liveness::Created);
        remove("saved").unwrap();
    }

    /// 已亡的残留可以被同名容器复用——否则崩过一次就再也用不了这个名字。
    #[test]
    fn stale_entry_is_reused() {
        let _home = TempHome::new("stale");
        let dir = dir_for("s1").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"s1","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}"#,
        )
        .unwrap();
        assert_eq!(liveness(&dir), Liveness::Exited);
        let reg = register("s1", &["/bin/true".into()], "(native)").unwrap();
        assert_eq!(liveness(reg.dir()), Liveness::Running);
    }

    /// `list` 要能跳过坏条目而不是整个失败——状态目录是用户可见的普通目录。
    #[test]
    fn list_skips_unreadable_entries() {
        let _home = TempHome::new("bad");
        let root = run_root().unwrap();
        std::fs::create_dir_all(root.join("broken")).unwrap();
        std::fs::write(root.join("broken/meta.json"), "{ not json").unwrap();
        std::fs::create_dir_all(root.join("nometa")).unwrap();
        let _ok = register("good", &["/bin/true".into()], "(native)").unwrap();
        let got = list().unwrap();
        assert_eq!(got.len(), 1, "只应列出可读的那条：{:?}", got);
        assert_eq!(got[0].0.name, "good");
    }

    /// 容器名不得逃出状态根目录。
    #[test]
    fn name_cannot_escape_run_root() {
        let _home = TempHome::new("escape");
        for bad in ["../evil", "a/b", "..", r"a\b"] {
            assert!(dir_for(bad).is_err(), "'{}' 应被拒绝", bad);
        }
        assert!(dir_for("ok-name_1").is_ok());
    }

    #[test]
    fn entry_json_roundtrip() {
        let e = Entry {
            name: "n".into(),
            pid: 42,
            created_unix: 1700000000,
            cmd: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            target: "alpine:latest".into(),
            stopping: false,
            exec_context: Some(ExecContext {
                allow_network: true,
                workdir: "/work".into(),
            }),
        };
        let encoded = e.to_json();
        assert_eq!(Entry::from_json(&encoded).unwrap(), e);
        assert!(!encoded.contains("env"), "状态不应包含环境或凭证字段");
        assert!(Entry::from_json("{}").is_none());
        assert!(Entry::from_json("nonsense").is_none());
    }

    #[test]
    fn old_meta_without_exec_context_remains_readable() {
        let old =
            r#"{"name":"old","pid":1,"created_unix":1,"cmd":["x"],"target":"(native)"}"#;
        let entry = Entry::from_json(old).unwrap();
        assert!(!entry.stopping);
        assert!(entry.exec_context.is_none());
    }

    fn wait_for_path(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "等待子进程信号超时：{}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_child(mut child: Child, label: &str) {
        let status = child.wait().unwrap();
        assert!(status.success(), "{} 子进程失败：{}", label, status);
    }

    /// 只由 `cross_process_remove_register_is_atomic` 启动。使用真实测试子进程，
    /// 避免同进程文件锁语义掩盖 Windows/Linux 的跨进程差异。
    #[test]
    fn cross_process_actor() {
        let Ok(mode) = std::env::var("WBOX_RUNSTATE_ACTOR") else {
            return;
        };
        let sync = PathBuf::from(std::env::var_os("WBOX_RUNSTATE_SYNC").unwrap());
        match mode.as_str() {
            "remove" => {
                remove("race").unwrap();
            }
            "register" => {
                let reg = register("race", &["cmd".into()], "(native)").unwrap();
                std::fs::write(sync.join("registered"), b"").unwrap();
                wait_for_path(&sync.join("release-register"));
                drop(reg);
            }
            other => panic!("未知 actor：{}", other),
        }
    }

    #[test]
    fn cross_process_remove_register_is_atomic() {
        let tmp = TempHome::new("cross-process-race");
        let home = &tmp.dir;
        let sync = home.join("sync");
        std::fs::create_dir_all(&sync).unwrap();
        let stale = dir_for("race").unwrap();
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("lock"), b"").unwrap();

        let exe = std::env::current_exe().unwrap();
        let spawn_actor = |mode: &str| {
            let mut command = Command::new(&exe);
            command
                .arg("--exact")
                .arg("runstate::tests::cross_process_actor")
                .arg("--nocapture")
                .env("HOME", home)
                .env("USERPROFILE", home)
                .env("WBOX_RUNSTATE_ACTOR", mode)
                .env("WBOX_RUNSTATE_SYNC", &sync);
            if mode == "remove" {
                command.env("WBOX_RUNSTATE_PAUSE_REMOVE", &sync);
            }
            command.spawn().unwrap()
        };

        let remover = spawn_actor("remove");
        wait_for_path(&sync.join("remove-checked"));
        let registrar = spawn_actor("register");
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !sync.join("registered").exists(),
            "register 必须等待 remove 的复合状态变更完成"
        );

        std::fs::write(sync.join("release-remove"), b"").unwrap();
        wait_child(remover, "remove");
        wait_for_path(&sync.join("registered"));
        assert_eq!(liveness(&stale), Liveness::Running);
        assert!(
            stale.join("meta.json").is_file(),
            "remove 不得删除随后完成的 register 记录"
        );

        std::fs::write(sync.join("release-register"), b"").unwrap();
        wait_child(registrar, "register");
        assert!(!stale.exists());
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;
    use crate::testenv::EnvGuard;

    /// 上限逻辑本身：超限即清空并留下可见标记。
    #[test]
    fn cap_truncates_and_marks() {
        let d = std::env::temp_dir().join(format!("wbox-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut g = EnvGuard::new();
        g.set("WBOX_LOG_MAX_BYTES", "100");
        std::fs::write(d.join(LOG_STDOUT), vec![b'x'; 5000]).unwrap();
        enforce_log_cap(&d, LOG_STDOUT);
        let after = std::fs::read_to_string(d.join(LOG_STDOUT)).unwrap();
        assert!(after.len() < 500, "应已截断，实际 {} 字节", after.len());
        assert!(after.contains("已丢弃此前内容"), "截断要可见：{}", after);
        // 未超限的不动
        std::fs::write(d.join(LOG_STDERR), b"small").unwrap();
        enforce_log_cap(&d, LOG_STDERR);
        assert_eq!(std::fs::read_to_string(d.join(LOG_STDERR)).unwrap(), "small");
        let _ = std::fs::remove_dir_all(&d);
    }
}

// ---------------------------------------------------------------------------
// 进程终止（`wbox stop`，PRD F8.3）
// ---------------------------------------------------------------------------

/// 终止方式。Linux 上先礼后兵，Windows 只有"兵"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kill {
    /// 请求对方自行退出（Linux: SIGTERM）。Windows 无等价物，见 [`terminate_pid`]。
    #[cfg(unix)]
    Graceful,
    /// 强制终止（Linux: SIGKILL；Windows: TerminateProcess）
    Forceful,
}

/// 终止指定进程。返回是否成功递送（**不代表对方已经退出**，退出要靠轮询锁判定）。
///
/// 杀的是 **supervisor（wbox 自己）**，不是 guest：容器整棵进程树的存活绑在
/// supervisor 上（Linux 的 PDEATHSIG、Windows 的 Job kill-on-close），
/// supervisor 一死内核就会收走整棵树。直接去杀 guest 反而漏掉它的子孙。
///
/// **平台差异如实记录**：Windows 没有 SIGTERM 这类"请你自己退出"的通用机制
/// （控制台事件对无控制台的后台进程不适用），因此 `Graceful` 在 Windows 上
/// 等同于 `Forceful`。这不是偷懒，是平台确实没有对齐物；`wbox stop` 的文案
/// 会说明这一点。
#[cfg(unix)]
pub fn terminate_pid(pid: u32, how: Kill) -> bool {
    let sig = match how {
        Kill::Graceful => libc::SIGTERM,
        Kill::Forceful => libc::SIGKILL,
    };
    // SAFETY: kill(2) 只按 pid 递送信号，不解引用任何指针。
    unsafe { libc::kill(pid as libc::pid_t, sig) == 0 }
}

#[cfg(windows)]
pub fn terminate_pid(pid: u32, _how: Kill) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    // SAFETY: OpenProcess 失败返回 null，此时不调用 TerminateProcess；
    // 句柄无论成败都只关一次。
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = TerminateProcess(h, 1) != 0;
        CloseHandle(h);
        ok
    }
}

#[cfg(test)]
mod kill_tests {
    use super::*;

    /// 起一个真的会赖着不走的子进程，用来验证终止确实生效。
    fn spawn_sleeper() -> std::process::Child {
        #[cfg(unix)]
        let mut c = std::process::Command::new("/bin/sleep");
        #[cfg(unix)]
        c.arg("30");
        #[cfg(windows)]
        let mut c = std::process::Command::new("cmd");
        #[cfg(windows)]
        c.args(["/c", "ping -n 30 127.0.0.1 >nul"]);
        c.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleeper")
    }

    /// 终止必须真的把进程干掉。**两个平台都跑这条**——Windows 那半
    /// （OpenProcess + TerminateProcess）我在本机无从验证，靠 CI 的 windows
    /// runner 真实执行；本轮之前正是这种"写完就算"的平台代码出过 Drop 顺序的错。
    #[test]
    fn terminate_actually_kills() {
        let mut child = spawn_sleeper();
        assert!(child.try_wait().unwrap().is_none(), "子进程本应还活着");
        assert!(terminate_pid(child.id(), Kill::Forceful), "递送终止失败");
        // 给内核一点时间收尸
        let mut gone = false;
        for _ in 0..100 {
            if child.try_wait().unwrap().is_some() {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(gone, "终止后子进程仍未退出");
    }

    /// 不存在的 pid 应当返回失败而不是 panic，也不该误伤别人。
    #[test]
    fn terminate_missing_pid_reports_failure() {
        // 极大的 pid 在两个平台都几乎不可能被占用
        assert!(!terminate_pid(4_000_000_000, Kill::Forceful));
    }
}

/// 记录**容器内**进程的 pid，供 `wbox exec` 用 `setns` 附着（PRD §4.9 L1）。
///
/// 与 [`Entry::pid`] 必须分清：那个是 **supervisor（wbox 自己）**的 pid，
/// 留在宿主 namespace，是 `stop` 要终止的对象；这里记的是**已经进了容器
/// namespace** 的那个进程，是 `exec` 要附着的对象。混用会让 `exec` 附到宿主上，
/// 隔离形同虚设。
/// 只在 Linux 编译：唯一的调用者 [`spawn_container_pid_recorder`] 是 Linux 专有，
/// 而 `exec` 本身也只在 Linux 可用（Windows 见 PRD §4.9 W2）。
#[cfg(target_os = "linux")]
pub fn record_container_pid(name: &str, pid: u32) {
    let Ok(dir) = dir_for(name) else { return };
    let _ = std::fs::write(dir.join(CONTAINER_PID), pid.to_string());
}

/// 在 restart 拉起下一代前移除旧 PID，避免短暂窗口内把新连接送进已退出甚至已
/// 被复用的宿主 PID。
#[cfg(target_os = "linux")]
pub fn clear_container_pid(name: &str) {
    let Ok(dir) = dir_for(name) else { return };
    let _ = std::fs::remove_file(dir.join(CONTAINER_PID));
}

/// 读回 [`record_container_pid`] 写下的 pid。
#[cfg(target_os = "linux")]
/// 与 [`record_container_pid`] 同为 Linux 专有：两个调用方（`exec` 的
/// setns 附着、`-p` 端口转发的连接器）都只在 Linux 编译。
#[cfg(target_os = "linux")]
pub fn container_pid(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join(CONTAINER_PID))
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(target_os = "linux")]
pub const CONTAINER_PID: &str = "container.pid";

/// 起一个线程，在容器进程出现后把它的 pid 记下来。
///
/// # 为什么要轮询，而不是直接取 `Command::spawn()` 的返回值
///
/// **`cmd.spawn()` 在容器退出之前根本不返回**——这是实测结论，不是推测。
/// PID namespace 需要双 fork：`pre_exec` 里 `unshare(CLONE_NEWPID)` 之后再
/// fork 一次，中间进程负责转发退出码、**永不 exec**。而 Rust 的
/// `Command::spawn()` 要等那根 CLOEXEC 错误管道读到 EOF 才返回，管道写端正握
/// 在这个不 exec 的中间进程手里，于是 EOF 只在容器结束时才到来。
///
/// 这个坑很会骗人：短命容器上一切"看起来正常"，因为你总是在它结束之后才去看
/// 文件；只有用长命容器并在**运行中**检查才会暴露。
///
/// 于是改为从宿主侧观察：中间进程就是 `Command::spawn` fork 出来的**直接子
/// 进程**，读 `/proc/<self>/task/<self>/children` 即可拿到，不必等 spawn 返回。
/// supervisor 此刻没有别的子进程（看门狗是线程不是进程），所以第一个就是它。
///
/// 记的是**中间进程**而非它 fork 出的孙进程：中间进程已经在新的
/// user/mount/net namespace 里，而 PID namespace 可经
/// `/proc/<pid>/ns/pid_for_children` 取到——四个 namespace 从这一个 pid 全都
/// 够得着（`unshare(CLONE_NEWPID)` 对调用者自己不生效，故它的 `ns/pid` 仍是
/// 宿主的，必须用 `pid_for_children`）。
#[cfg(target_os = "linux")]
pub fn spawn_container_pid_recorder(name: String) {
    std::thread::spawn(move || {
        let me = std::process::id();
        let path = format!("/proc/{}/task/{}/children", me, me);
        // 上界 ~30s：容器起不来就算了，`exec` 会因缺 pid 明确报错，
        // 总好过一个永不退出的线程。
        for _ in 0..600 {
            if let Ok(s) = std::fs::read_to_string(&path) {
                if let Some(pid) = s.split_whitespace().next().and_then(|t| t.parse().ok()) {
                    record_container_pid(&name, pid);
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
}
