//! EmuBackend：Linux 用户态模拟后端。
//!
//! wbox-linux.exe 是 `crates/wbox-linux` 里的 x86-64 Linux 用户态模拟器
//! （纯 Rust；取代了原先 vendored 的 C 实现 blink）。本后端职责：
//! 1. 定位 wbox-linux.exe：`WBOX_LINUX` 环境变量优先，其次 wbox.exe 同目录；
//! 2. 构造执行计划：rootfs 目录作为工作目录与 VFS 根前缀，
//!    guest 命令（已按 docker 规则合并 Entrypoint/Cmd）作为 wbox-linux 的参数；
//! 3. spawn 委托 NativeBackend —— wbox-linux.exe 本身是 Windows 程序，
//!    跑在 AppContainer + Job 内形成双层隔离。
//!
//! 找不到 wbox-linux.exe 时 prepare 返回明确错误（而不是含糊的"文件不存在"），
//! 也不静默降级去直接执行 guest 程序。

use super::{Backend, Prepared, RunSpec};
use crate::error::{Result, WboxError};
use std::path::{Path, PathBuf};

/// Linux 模拟器二进制文件名。
pub const LINUX_EXE_NAME: &str = "wbox-linux.exe";
/// 显式指定 wbox-linux.exe 路径的环境变量。
pub const LINUX_EXE_ENV: &str = "WBOX_LINUX";
/// VFS 根前缀（guest `/` 映射到的宿主目录），必须设置，
/// 否则动态链接的 guest 程序会命中宿主系统库。
///
/// 键名保留 `BLINK_PREFIX` 是为了兼容：模拟器同时认 `WBOX_PREFIX` 和这个
/// 旧名（见 `crates/wbox-linux/src/syscall/fs.rs`），改名要两边一起动，
/// 单独改这里会让 rootfs 隔离静默失效。
pub const BLINK_PREFIX_ENV: &str = "BLINK_PREFIX";

/// Linux 镜像后端（无状态）。
pub struct EmuBackend;

/// 定位 wbox-linux.exe：`WBOX_LINUX` > wbox 自身 exe 同目录。
/// 返回 `(路径, 来源描述)`。仅做存在性检查，不校验可执行性。
fn locate_linux_exe() -> Result<(PathBuf, &'static str)> {
    if let Some(p) = std::env::var_os(LINUX_EXE_ENV) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok((p, "WBOX_LINUX 环境变量"));
        }
        return Err(not_ready_error(format!(
            "{} 指向的 '{}' 不存在",
            LINUX_EXE_ENV,
            p.display()
        )));
    }
    // wbox.exe 同目录（portable 分发形态：两个 exe 放在一起）
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(LINUX_EXE_NAME);
            if p.is_file() {
                return Ok((p, "wbox 同目录"));
            }
        }
    }
    Err(not_ready_error(format!(
        "未找到 {}（请将 {} 与 wbox.exe 放在同一目录，或设置 {} 环境变量指向它）",
        LINUX_EXE_NAME, LINUX_EXE_NAME, LINUX_EXE_ENV
    )))
}

/// 统一的"后端未就绪"错误（退出码 4 = 进程创建类，语义最接近）。
fn not_ready_error(detail: String) -> WboxError {
    WboxError::spawn(format!(
            "Linux 后端不可用：{}。请把 wbox-linux.exe 与 wbox.exe 放在一起\
             （portable 分发形态就是这两个文件），或用 WBOX_LINUX 指向它",
            detail
        ))
}

/// 公共 DNS（阿里公共 DNS）。rootfs 内缺失 /etc/resolv.conf 时注入，
/// 保证 guest 程序能解析域名（模拟器的 socket 直通宿主网络栈）。
pub const DEFAULT_DNS: &str = "223.5.5.5";

/// 模拟器的 VFS 初始化会挂载内建的 `/dev` 与 `/proc`。AppContainer 只有
/// rootfs 的读执行权限，不能在降权后创建缺失目录，因此必须由 wbox 预建。
fn ensure_vfs_mountpoints(rootfs: &Path) -> Result<()> {
    for name in ["dev", "proc"] {
        let path = rootfs.join(name);
        std::fs::create_dir_all(&path).map_err(|e| {
            WboxError::registry(format!(
                "创建模拟器 VFS 挂载点 '{}' 失败：{}",
                path.display(),
                e
            ))
        })?;
    }
    Ok(())
}

/// 确保 rootfs 内存在 `/etc/resolv.conf`；缺失（或为空文件）时写入公共 DNS。
/// 已存在且有内容的 resolv.conf 不覆盖（用户/镜像自定义优先）。
/// 返回是否发生了注入。纯文件操作，Windows/Linux 宿主通用。
pub(super) fn ensure_resolv_conf(rootfs: &Path) -> Result<bool> {
    let etc = rootfs.join("etc");
    let resolv = etc.join("resolv.conf");
    let need_inject = match std::fs::metadata(&resolv) {
        Ok(m) => m.is_file() && m.len() == 0,
        Err(_) => true,
    };
    if !need_inject {
        return Ok(false);
    }
    std::fs::create_dir_all(&etc).map_err(|e| {
        WboxError::registry(format!(
            "创建 rootfs etc 目录 '{}' 失败：{}",
            etc.display(),
            e
        ))
    })?;
    std::fs::write(&resolv, format!("nameserver {}\n", DEFAULT_DNS)).map_err(|e| {
        WboxError::registry(format!(
            "写入 '{}' 失败：{}",
            resolv.display(),
            e
        ))
    })?;
    Ok(true)
}

/// 为 Windows OCI 运行创建完整的私有 rootfs 副本。
///
/// 首版优先保证语义正确：guest 的 create/write/rename/delete 都只发生在副本；
/// 共享镜像缓存绝不作为工作根。后续可把复制实现替换为具备 copy-up/whiteout
/// 的稀疏层，而不改变调用方生命周期。
#[cfg(windows)]
pub(crate) fn create_private_rootfs(
    source: &Path,
    destination: &Path,
    profile_name: &str,
) -> Result<()> {
    super::require_rootfs_dir(source)?;
    if destination.exists() {
        return Err(WboxError::spawn(format!(
            "私有 rootfs '{}' 已存在，拒绝与旧运行实例合并",
            destination.display()
        )));
    }
    copy_rootfs_tree(source, destination)?;
    crate::acl::grant_modify_recursive_for_profile(destination, profile_name)?;
    Ok(())
}

#[cfg(any(windows, test))]
pub(crate) fn copy_rootfs_tree(source: &Path, destination: &Path) -> Result<()> {
    copy_rootfs_tree_inner(source, destination, source, destination)
}

#[cfg(any(windows, test))]
fn copy_rootfs_tree_inner(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
) -> Result<()> {
    std::fs::create_dir(destination).map_err(|e| {
        WboxError::spawn(format!(
            "创建私有 rootfs 目录 '{}' 失败：{}",
            destination.display(),
            e
        ))
    })?;
    let entries = std::fs::read_dir(source).map_err(|e| {
        WboxError::spawn(format!("读取 rootfs '{}' 失败：{}", source.display(), e))
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| WboxError::spawn(format!("读取 rootfs 目录项失败：{}", e)))?;
        let src = entry.path();
        let dst = destination.join(entry.file_name());
        let ty = entry.file_type().map_err(|e| {
            WboxError::spawn(format!("读取 rootfs 项 '{}' 类型失败：{}", src.display(), e))
        })?;
        if ty.is_dir() {
            copy_rootfs_tree_inner(&src, &dst, source_root, destination_root)?;
        } else if ty.is_file() {
            copy_rootfs_file(&src, &dst)?;
        } else if ty.is_symlink() {
            copy_rootfs_symlink(&src, &dst, source_root, destination_root)?;
        } else {
            return Err(WboxError::spawn(format!(
                "rootfs 项 '{}' 类型不受 Windows 私有层支持",
                src.display()
            )));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn copy_rootfs_file(source: &Path, destination: &Path) -> Result<()> {
    // CopyFile 会连安全描述符一起复制，构建 staging 上临时授予的 AppContainer
    // 修改 ACE 就可能泄漏进最终镜像缓存。显式新建目标并只复制内容，让目标继承
    // destination 父目录的干净 DACL。
    let mut input = std::fs::File::open(source).map_err(|e| {
        WboxError::spawn(format!("打开 rootfs 文件 '{}' 失败：{}", source.display(), e))
    })?;
    let mut output = std::fs::File::create(destination).map_err(|e| {
        WboxError::spawn(format!(
            "创建 rootfs 文件 '{}' 失败：{}",
            destination.display(),
            e
        ))
    })?;
    std::io::copy(&mut input, &mut output).map_err(|e| {
        WboxError::spawn(format!(
            "复制 rootfs 文件 '{}' 到 '{}' 失败：{}",
            source.display(),
            destination.display(),
            e
        ))
    })?;
    Ok(())
}

#[cfg(all(unix, test))]
fn copy_rootfs_file(source: &Path, destination: &Path) -> Result<()> {
    std::fs::copy(source, destination).map_err(|e| {
        WboxError::spawn(format!(
            "复制 rootfs 文件 '{}' 到 '{}' 失败：{}",
            source.display(),
            destination.display(),
            e
        ))
    })?;
    Ok(())
}

#[cfg(windows)]
fn copy_rootfs_symlink(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
) -> Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    let target = std::fs::read_link(source).map_err(|e| {
        WboxError::spawn(format!("读取 rootfs symlink '{}' 失败：{}", source.display(), e))
    })?;
    let has_windows_prefix = target
        .components()
        .any(|component| matches!(component, std::path::Component::Prefix(_)));
    let source_target = if has_windows_prefix {
        // 私有 rootfs 再复制时，上一轮已经把 Linux 绝对 symlink 重写成了
        // staging 内的 Windows 绝对路径。直接校验它仍在 source_root 内，
        // 不能再按容器根路径拼一次。
        target.clone()
    } else if target.has_root() {
        let relative: std::path::PathBuf = target
            .components()
            .filter(|component| {
                !matches!(
                    component,
                    std::path::Component::Prefix(_) | std::path::Component::RootDir
                )
            })
            .collect();
        source_root.join(relative)
    } else {
        source
            .parent()
            .ok_or_else(|| WboxError::spawn("rootfs symlink 缺少父目录"))?
            .join(&target)
    };
    let source_root = std::fs::canonicalize(source_root).map_err(|e| {
        WboxError::spawn(format!("规范化共享 rootfs '{}' 失败：{}", source_root.display(), e))
    })?;
    let source_target = std::fs::canonicalize(&source_target).map_err(|e| {
        WboxError::spawn(format!(
            "rootfs symlink '{}' 的目标不存在或不可解析：{}",
            source.display(),
            e
        ))
    })?;
    let relative_target = source_target.strip_prefix(&source_root).map_err(|_| {
        WboxError::spawn(format!("rootfs symlink '{}' 的目标越出共享 rootfs", source.display()))
    })?;
    let private_target = destination_root.join(relative_target);
    let target_is_dir = source_target.is_dir();
    let result = if target_is_dir {
        symlink_dir(&private_target, destination)
    } else {
        symlink_file(&private_target, destination)
    };
    result.map_err(|e| {
        WboxError::spawn(format!(
            "复制 rootfs symlink '{}' 到 '{}' 失败：{}",
            source.display(),
            destination.display(),
            e
        ))
    })
}

#[cfg(all(unix, test))]
fn copy_rootfs_symlink(
    source: &Path,
    destination: &Path,
    _source_root: &Path,
    _destination_root: &Path,
) -> Result<()> {
    let target = std::fs::read_link(source).map_err(|e| {
        WboxError::spawn(format!("读取 rootfs symlink '{}' 失败：{}", source.display(), e))
    })?;
    std::os::unix::fs::symlink(&target, destination).map_err(|e| {
        WboxError::spawn(format!(
            "复制 rootfs symlink '{}' 到 '{}' 失败：{}",
            source.display(),
            destination.display(),
            e
        ))
    })
}

/// 构造 wbox-linux 的命令行：`wbox-linux.exe <guest cmd...>`。
/// （rootfs 不经命令行传递，而是工作目录 + BLINK_PREFIX 环境变量。）
fn build_emu_command(exe: &Path, guest_cmd: &[String]) -> Vec<String> {
    let mut cmd = vec![exe.to_string_lossy().into_owned()];
    cmd.extend(guest_cmd.iter().cloned());
    cmd
}

impl Backend for EmuBackend {
    fn prepare(&self, spec: &RunSpec) -> Result<Prepared> {
        super::reject_volumes_if_unsupported(&spec.volumes)?;
        super::require_cmd(&spec.cmd)?;
        let (exe, src) = locate_linux_exe()?;
        let rootfs = &spec.workdir; // 镜像模式下 workdir = rootfs 目录
        super::require_rootfs_dir(rootfs)?;
        if spec.verbose {
            super::verbose_kv("Linux 后端模拟器", format!("{}（{}）", exe.display(), src));
            super::verbose_kv("rootfs", rootfs.display());
        }
        ensure_vfs_mountpoints(rootfs)?;
        // rootfs 网络可用性：缺失/空的 /etc/resolv.conf 注入公共 DNS，
        // 使 guest 程序（apt/wget 等）开箱即可解析域名。
        if ensure_resolv_conf(rootfs)? && spec.verbose {
            super::verbose_kv(
                "resolv.conf",
                format!("rootfs 缺失/为空，已注入公共 DNS {}", DEFAULT_DNS),
            );
        }
        // H2 修复：镜像 config.Env 中的隔离/凭证保留键（BLINK_*/WBOX_*）一律
        // 丢弃；BLINK_PREFIX 由 wbox 强制为 rootfs（镜像/宿主均不可覆盖），
        // 子进程环境为显式白名单（不直通宿主环境，H6）。策略实现统一在
        // backend::build_sanitized_env（与 NativeBackend 单一出口）。
        let forced = [(
            BLINK_PREFIX_ENV.to_string(),
            rootfs.to_string_lossy().into_owned(),
        )];
        let env = super::build_sanitized_env(&spec.env, &forced, spec.env_pass_all, spec.verbose, super::env::GuestFlavor::Windows);
        let cmd = build_emu_command(&exe, &spec.cmd);
        if spec.verbose {
            super::verbose_kv("guest 命令行（Entrypoint/Cmd 合并后）", format!("{:?}", spec.cmd));
            super::verbose_kv("最终命令行", format!("{:?}", cmd));
            for (k, v) in &env {
                if k == BLINK_PREFIX_ENV {
                    super::verbose_kv(k, v);
                }
            }
        }
        Ok(Prepared {
            cmd,
            workdir: rootfs.clone(),
            env,
        })
    }

    #[cfg(windows)]
    fn spawn(&self, spec: &RunSpec, prepared: &Prepared) -> Result<u32> {
        // 双层隔离：wbox-linux.exe 经 NativeBackend 在 AppContainer + Job 内启动。
        // run_image 已把共享 cache 复制到状态目录，并仅向本 profile SID 授予
        // 修改权；prepared.workdir 绝不能重新指回共享镜像缓存。
        super::native::spawn_native(
            spec,
            prepared,
            &format!("wbox-linux（blink 模拟器，BLINK_PREFIX={}）", prepared.workdir.display()),
        )
    }

    #[cfg(not(windows))]
    fn spawn(&self, _spec: &RunSpec, _prepared: &Prepared) -> Result<u32> {
        Err(WboxError::spawn("Linux 后端仅在 Windows 上可用（外层隔离为 AppContainer/Job Object）"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(cmd: &[&str]) -> RunSpec {
        RunSpec {
            name: "t".to_string(),
            workdir: std::env::temp_dir(), // 已存在的目录充当 rootfs
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            env: vec![("PATH".to_string(), "/usr/bin".to_string())],
            ..RunSpec::default()
        }
    }

    #[test]
    fn prepare_errors_when_exe_missing() {
        // WBOX_LINUX 指向不存在的路径：必须明确报错，且**点名是哪个路径没找到**
        // ——含糊的"文件不存在"会让用户不知道该去修哪一处配置。
        let mut g = crate::testenv::EnvGuard::new();
        g.set(LINUX_EXE_ENV, "/nonexistent/wbox-linux.exe");
        let err = EmuBackend.prepare(&spec(&["bash"])).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("Linux 后端不可用"), "{}", msg);
        assert!(msg.contains("/nonexistent/wbox-linux.exe"), "{}", msg);
        assert!(msg.contains(LINUX_EXE_ENV), "要提示怎么修：{}", msg);
    }

    #[test]
    fn prepare_builds_command_and_blink_prefix() {
        // WBOX_LINUX 指向已存在文件（用 /bin/true 或当前 exe 代替 exe）
        let fake = std::env::current_exe().unwrap();
        let mut g = crate::testenv::EnvGuard::new();
        g.set(LINUX_EXE_ENV, &fake);
        let p = EmuBackend.prepare(&spec(&["bash", "-l"])).unwrap();
        assert_eq!(p.cmd[0], fake.to_string_lossy());
        assert_eq!(&p.cmd[1..], &["bash", "-l"]);
        // BLINK_PREFIX 注入且指向 rootfs
        assert!(p
            .env
            .iter()
            .any(|(k, v)| k == BLINK_PREFIX_ENV && v == &std::env::temp_dir().to_string_lossy()));
        // 镜像 Env 保留
        assert!(p.env.iter().any(|(k, _)| k == "PATH"));
    }

    #[test]
    fn image_blink_prefix_forced_to_rootfs() {
        // H2：镜像 Env 里的 BLINK_PREFIX（如恶意值 "/"）不得生效，
        // wbox 强制覆盖为 rootfs。
        let fake = std::env::current_exe().unwrap();
        let mut g = crate::testenv::EnvGuard::new();
        g.set(LINUX_EXE_ENV, &fake);
        let mut s = spec(&["bash"]);
        s.env.push((BLINK_PREFIX_ENV.to_string(), "/".to_string()));
        let p = EmuBackend.prepare(&s).unwrap();
        let vals: Vec<&str> = p
            .env
            .iter()
            .filter(|(k, _)| k == BLINK_PREFIX_ENV)
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            vals,
            vec![std::env::temp_dir().to_string_lossy().as_ref()],
            "BLINK_PREFIX 必须唯一且强制为 rootfs"
        );
    }

    #[test]
    fn image_env_reserved_keys_dropped_and_host_not_inherited() {
        // H2/H6：镜像 Env 的 WBOX_* 隔离旋钮被丢弃；宿主环境（含机密）
        // 默认不透传给子进程。
        let fake = std::env::current_exe().unwrap();
        let mut g = crate::testenv::EnvGuard::new();
        g.set(LINUX_EXE_ENV, &fake);
        g.set("WBOX_REGISTRY_PASS", "hunter2");
        let mut s = spec(&["bash"]);
        s.env.push(("WBOX_ROOT".to_string(), r"C:\".to_string()));
        s.env.push(("WBOX_VA_BITS".to_string(), "43".to_string()));
        s.env.push(("LANG".to_string(), "C".to_string()));
        let p = EmuBackend.prepare(&s).unwrap();
        assert!(!p.env.iter().any(|(k, _)| k == "WBOX_ROOT"));
        assert!(!p.env.iter().any(|(k, _)| k == "WBOX_VA_BITS"));
        assert!(!p.env.iter().any(|(k, _)| k == "WBOX_REGISTRY_PASS"));
        assert!(p.env.iter().any(|(k, v)| k == "LANG" && v == "C"));
    }

    #[test]
    fn prepare_requires_command() {
        let err = EmuBackend.prepare(&spec(&[])).unwrap_err();
        assert!(format!("{}", err).contains("Entrypoint/Cmd"));
    }

    // ---- resolv.conf 注入 ----

    /// 建一个临时 rootfs（含唯一目录名，避免并发冲突）。
    fn temp_rootfs(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wbox-test-rootfs-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn injects_resolv_conf_when_missing() {
        let rootfs = temp_rootfs("missing");
        assert!(ensure_resolv_conf(&rootfs).unwrap());
        let content = std::fs::read_to_string(rootfs.join("etc/resolv.conf")).unwrap();
        assert!(content.contains(DEFAULT_DNS), "{}", content);
        // 幂等：已存在且有内容则不再注入
        assert!(!ensure_resolv_conf(&rootfs).unwrap());
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    #[test]
    fn creates_vfs_mountpoints_for_minimal_rootfs() {
        let rootfs = temp_rootfs("mountpoints");
        ensure_vfs_mountpoints(&rootfs).unwrap();
        assert!(rootfs.join("dev").is_dir());
        assert!(rootfs.join("proc").is_dir());
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    #[test]
    fn private_rootfs_copy_is_independent_from_image_cache() {
        let source = temp_rootfs("private-source");
        let destination = source.with_file_name(format!(
            "{}-copy",
            source.file_name().unwrap().to_string_lossy()
        ));
        std::fs::create_dir_all(source.join("etc")).unwrap();
        std::fs::write(source.join("etc/value"), b"cached").unwrap();

        copy_rootfs_tree(&source, &destination).unwrap();
        std::fs::write(destination.join("etc/value"), b"private").unwrap();
        std::fs::write(destination.join("created"), b"guest").unwrap();
        std::fs::remove_file(destination.join("etc/value")).unwrap();

        assert_eq!(std::fs::read(source.join("etc/value")).unwrap(), b"cached");
        assert!(!source.join("created").exists());
        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&destination);
    }

    #[test]
    fn rewrites_empty_resolv_conf_but_keeps_custom() {
        let rootfs = temp_rootfs("empty");
        let etc = rootfs.join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        // 空文件 → 注入
        std::fs::write(etc.join("resolv.conf"), "").unwrap();
        assert!(ensure_resolv_conf(&rootfs).unwrap());
        assert!(std::fs::read_to_string(etc.join("resolv.conf"))
            .unwrap()
            .contains(DEFAULT_DNS));
        // 自定义内容 → 保留
        std::fs::write(etc.join("resolv.conf"), "nameserver 8.8.8.8\n").unwrap();
        assert!(!ensure_resolv_conf(&rootfs).unwrap());
        assert_eq!(
            std::fs::read_to_string(etc.join("resolv.conf")).unwrap(),
            "nameserver 8.8.8.8\n"
        );
        let _ = std::fs::remove_dir_all(&rootfs);
    }
}
