//! Docker/Podman-style aggregate disk accounting (`wbox system df`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{Result, WboxError};
use crate::runstate::Liveness;

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageRow {
    kind: &'static str,
    total: usize,
    active: usize,
    logical_bytes: u64,
    reclaimable_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SystemDf {
    rows: Vec<UsageRow>,
    backing_path: PathBuf,
    volume: agenterm_platform::storage::VolumeSpace,
}

pub fn cmd_system(args: &[String]) -> Result<u32> {
    match args.first().map(String::as_str) {
        Some("df") if args.len() == 1 => {
            print_report(&collect_report()?);
            Ok(0)
        }
        Some("df") => Err(WboxError::args("system df 当前不接受额外参数")),
        Some(other) => Err(WboxError::args(format!(
            "未知 system 子命令 '{}'（支持 df）",
            other
        ))),
        None => Err(WboxError::args("system 缺少子命令（支持 df）")),
    }
}

fn collect_report() -> Result<SystemDf> {
    let containers = crate::runstate::list()?;

    let referenced_images: HashSet<PathBuf> = containers
        .iter()
        .filter_map(|(entry, _)| crate::oci::ImageRef::parse(&entry.target, None).ok())
        .filter_map(|image| crate::oci::image_dir(&image).ok())
        .collect();
    let images = crate::oci::list_refs()?;
    let image_paths: Vec<_> = images.iter().map(|image| image.directory.clone()).collect();
    let reclaimable_images: Vec<_> = images
        .iter()
        .filter(|image| !referenced_images.contains(&image.directory))
        .map(|image| image.directory.clone())
        .collect();

    let container_paths: Vec<_> = containers
        .iter()
        .map(|(entry, _)| crate::runstate::dir_for(&entry.name))
        .collect::<Result<_>>()?;
    let reclaimable_containers: Vec<_> = containers
        .iter()
        .filter(|(_, liveness)| *liveness == Liveness::Exited)
        .map(|(entry, _)| crate::runstate::dir_for(&entry.name))
        .collect::<Result<_>>()?;

    let volume_names = crate::volume::list()?;
    let active_volumes: HashSet<_> = volume_names
        .iter()
        .filter(|name| !crate::volume::users(name).is_empty())
        .cloned()
        .collect();
    let volume_paths: Vec<_> = volume_names
        .iter()
        .map(|name| crate::volume::dir_for(name))
        .collect::<Result<_>>()?;
    let reclaimable_volumes: Vec<_> = volume_names
        .iter()
        .filter(|name| !active_volumes.contains(*name))
        .map(|name| crate::volume::dir_for(name))
        .collect::<Result<_>>()?;

    let build_root = crate::build::cache_root()?;
    let build_entries = child_directories(&build_root)?;

    let managed_root = crate::oci::cache_root()?
        .parent()
        .ok_or_else(|| WboxError::args("镜像缓存目录缺少 .wbox 父目录"))?
        .to_path_buf();
    let backing_path = closest_existing_directory(&managed_root)?;
    let volume = agenterm_platform::storage::volume_space(&backing_path)
        .map_err(|error| WboxError::args(format!("查询存储卷失败：{error}")))?;

    Ok(SystemDf {
        rows: vec![
            row(
                "Images",
                images.len(),
                images.len().saturating_sub(reclaimable_images.len()),
                &image_paths,
                &reclaimable_images,
            )?,
            row(
                "Containers",
                containers.len(),
                containers
                    .iter()
                    .filter(|(_, liveness)| *liveness == Liveness::Running)
                    .count(),
                &container_paths,
                &reclaimable_containers,
            )?,
            row(
                "Local Volumes",
                volume_names.len(),
                active_volumes.len(),
                &volume_paths,
                &reclaimable_volumes,
            )?,
            row(
                "Build Cache",
                build_entries.len(),
                0,
                &build_entries,
                &build_entries,
            )?,
        ],
        backing_path,
        volume,
    })
}

fn row(
    kind: &'static str,
    total: usize,
    active: usize,
    paths: &[PathBuf],
    reclaimable: &[PathBuf],
) -> Result<UsageRow> {
    Ok(UsageRow {
        kind,
        total,
        active,
        logical_bytes: logical_size_many(paths)?,
        reclaimable_bytes: logical_size_many(reclaimable)?,
    })
}

fn logical_size_many(paths: &[PathBuf]) -> Result<u64> {
    paths.iter().try_fold(0_u64, |total, path| {
        let bytes = crate::disk_usage::logical_size(path).map_err(|error| {
            WboxError::args(format!("统计 '{}' 的磁盘占用失败：{error}", path.display()))
        })?;
        total
            .checked_add(bytes)
            .ok_or_else(|| WboxError::args("逻辑磁盘占用总量超过 u64"))
    })
}

fn child_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(WboxError::args(format!(
                "读取构建缓存 '{}' 失败：{error}",
                root.display()
            )))
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            WboxError::args(format!("读取构建缓存 '{}' 失败：{error}", root.display()))
        })?;
        if entry
            .file_type()
            .map_err(|error| WboxError::args(format!("读取缓存类型失败：{error}")))?
            .is_dir()
        {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

fn closest_existing_directory(path: &Path) -> Result<PathBuf> {
    path.ancestors()
        .find(|candidate| candidate.is_dir())
        .map(Path::to_path_buf)
        .ok_or_else(|| WboxError::args(format!("'{}' 没有可查询的现存父目录", path.display())))
}

fn print_report(report: &SystemDf) {
    println!(
        "{:<16} {:>8} {:>8} {:>12} {:>20}",
        "TYPE", "TOTAL", "ACTIVE", "SIZE", "RECLAIMABLE"
    );
    for row in &report.rows {
        let percent = if row.logical_bytes == 0 {
            0.0
        } else {
            row.reclaimable_bytes as f64 / row.logical_bytes as f64 * 100.0
        };
        println!(
            "{:<16} {:>8} {:>8} {:>12} {:>12} ({:>3.0}%)",
            row.kind,
            row.total,
            row.active,
            super::stats::human_bytes(row.logical_bytes),
            super::stats::human_bytes(row.reclaimable_bytes),
            percent
        );
    }
    println!();
    println!("Backing volume : {}", report.backing_path.display());
    println!(
        "Capacity       : {} total, {} available, {} allocation unit",
        super::stats::human_bytes(report.volume.total_bytes.get()),
        super::stats::human_bytes(report.volume.available_bytes),
        super::stats::human_bytes(report.volume.allocation_unit.get())
    );
    println!("Note           : sizes are logical bytes; hard links may be counted more than once.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runstate::ExecContext;
    use crate::testenv::TempHome;

    #[test]
    fn rejects_missing_unknown_and_extra_arguments() {
        assert!(cmd_system(&[]).is_err());
        assert!(cmd_system(&["bogus".into()]).is_err());
        assert!(cmd_system(&["df".into(), "extra".into()]).is_err());
    }

    #[test]
    fn reports_owned_trees_and_active_references() {
        let home = TempHome::new("system-df");
        let image = home.plant_fake_image("registry-1.docker.io", "library_fake", "latest");
        let unused_image =
            home.plant_fake_image("registry-1.docker.io", "library_unused", "latest");
        let (volume_dir, _) = crate::volume::create("data").unwrap();
        std::fs::write(volume_dir.join("payload"), b"volume").unwrap();
        let (idle_volume, _) = crate::volume::create("idle").unwrap();
        std::fs::write(idle_volume.join("payload"), b"idle").unwrap();
        let build = crate::build::cache_root().unwrap().join("step-key/rootfs");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("artifact"), b"build").unwrap();

        let registration = crate::runstate::register_with_context(
            "c1",
            &["/bin/true".into()],
            "fake:latest",
            false,
            Some(ExecContext {
                volumes: vec![format!("{}:/data", volume_dir.display())],
                ..ExecContext::default()
            }),
            None,
        )
        .unwrap();
        std::fs::create_dir_all(registration.dir().join("layer/upper")).unwrap();
        std::fs::write(registration.dir().join("layer/upper/change"), b"container").unwrap();

        crate::runstate::create_pending(
            "created",
            &["run".into()],
            &["/bin/true".into()],
            "fake:latest",
            None,
        )
        .unwrap();
        let mut reservation = crate::runstate::reserve_detached("exited").unwrap();
        let token = reservation.token().to_string();
        let exited = crate::runstate::register_with_context(
            "exited",
            &["/bin/true".into()],
            "fake:latest",
            true,
            None,
            Some(&token),
        )
        .unwrap();
        reservation.disarm();
        std::fs::write(exited.dir().join("stdout.log"), b"old log").unwrap();
        let exited_dir = exited.dir().to_path_buf();
        drop(exited);

        let expected_image_reclaim = crate::disk_usage::logical_size(&unused_image).unwrap();
        let expected_container_reclaim = crate::disk_usage::logical_size(&exited_dir).unwrap();
        let expected_volume_reclaim = crate::disk_usage::logical_size(&idle_volume).unwrap();

        let report = collect_report().unwrap();
        assert_eq!(report.rows[0].total, 2);
        assert_eq!(report.rows[0].active, 1);
        assert_eq!(report.rows[0].reclaimable_bytes, expected_image_reclaim);
        assert_eq!(report.rows[1].total, 3);
        assert_eq!(report.rows[1].active, 1);
        assert_eq!(report.rows[1].reclaimable_bytes, expected_container_reclaim);
        assert_eq!(report.rows[2].total, 2);
        assert_eq!(report.rows[2].active, 1);
        assert_eq!(report.rows[2].reclaimable_bytes, expected_volume_reclaim);
        assert_eq!(report.rows[3].total, 1);
        assert_eq!(report.rows[3].active, 0);
        assert_eq!(report.rows[3].logical_bytes, 5);
        assert_eq!(report.rows[3].reclaimable_bytes, 5);
        assert!(report.rows[0].logical_bytes >= crate::disk_usage::logical_size(&image).unwrap());
        assert!(report.volume.total_bytes.get() > 0);
    }

    #[test]
    fn fresh_home_uses_the_closest_existing_parent_volume() {
        let home = TempHome::new("system-df-fresh");
        let report = collect_report().unwrap();
        assert_eq!(report.backing_path, home.dir);
        assert!(report.rows.iter().all(|row| row.total == 0));
    }
}
