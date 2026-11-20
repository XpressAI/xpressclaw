//! Lightweight, demand-driven monitoring of the machine running XpressClaw.
//! No process scans, subprocesses, sleeps, or agent/container locks are needed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Serialize;
use sysinfo::{DiskRefreshKind, Disks, System};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Capacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub used_percent: f64,
}

impl Capacity {
    fn new(total: u64, available: u64) -> Option<Self> {
        if total == 0 || available > total {
            return None;
        }
        let used = total - available;
        Some(Self {
            total_bytes: total,
            available_bytes: available,
            used_bytes: used,
            used_percent: used as f64 / total as f64 * 100.0,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskUsage {
    pub locations: Vec<String>,
    pub mount_point: Option<String>,
    pub capacity: Option<Capacity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceSnapshot {
    pub sampled_at: String,
    /// None until two reasonably spaced CPU samples exist.
    pub cpu_percent: Option<f32>,
    pub cpu_count: usize,
    pub memory: Option<Capacity>,
    pub disks: Vec<DiskUsage>,
}

pub struct ResourceMonitor {
    system: System,
    last_sample: Option<Instant>,
    cached: Option<ResourceSnapshot>,
    paths: Option<(PathBuf, PathBuf)>,
}

impl Default for ResourceMonitor {
    fn default() -> Self {
        Self {
            system: System::new(),
            last_sample: None,
            cached: None,
            paths: None,
        }
    }
}

impl ResourceMonitor {
    /// Called on a blocking thread, behind one shared instance-local mutex.
    /// A cached sample bounds OS work when multiple dashboards are open.
    pub fn sample(&mut self, data_dir: &Path, workspace_dir: &Path) -> ResourceSnapshot {
        let paths = (data_dir.to_path_buf(), workspace_dir.to_path_buf());
        let elapsed = self.last_sample.map(|last| last.elapsed());
        if elapsed.is_some_and(|age| age < SAMPLE_INTERVAL) && self.paths.as_ref() == Some(&paths) {
            if let Some(cached) = &self.cached {
                return cached.clone();
            }
        }
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        let cpu = self.system.global_cpu_usage();
        // After a long-hidden tab the next delta would describe the whole
        // absence, not current load. Prime a new interval instead.
        let cpu_percent = elapsed
            .filter(|age| {
                *age >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL && *age <= SAMPLE_INTERVAL * 3
            })
            .filter(|_| !self.system.cpus().is_empty() && cpu.is_finite())
            .map(|_| cpu.clamp(0.0, 100.0));
        let disks =
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());
        let mounts: Vec<_> = disks
            .iter()
            .map(|disk| {
                (
                    canonical_mount(disk.mount_point()),
                    Capacity::new(disk.total_space(), disk.available_space()),
                )
            })
            .collect();
        let snapshot = ResourceSnapshot {
            sampled_at: Utc::now().to_rfc3339(),
            cpu_percent,
            cpu_count: self.system.cpus().len(),
            // Available memory includes reclaimable caches; free memory alone
            // would incorrectly present healthy Unix systems as almost full.
            memory: Capacity::new(self.system.total_memory(), self.system.available_memory()),
            disks: disk_usage(&mounts, data_dir, workspace_dir),
        };
        self.last_sample = Some(Instant::now());
        self.paths = Some(paths);
        self.cached = Some(snapshot.clone());
        snapshot
    }
}

fn canonical_mount(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    for ancestor in absolute.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(path) => return Some(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            // Permission/I/O failures must not fall back to an unrelated disk.
            Err(_) => return None,
        }
    }
    None
}

fn disk_usage(
    mounts: &[(PathBuf, Option<Capacity>)],
    data: &Path,
    workspace: &Path,
) -> Vec<DiskUsage> {
    let mut results: Vec<DiskUsage> = Vec::new();
    for (label, path) in [("Data", data), ("Workspaces", workspace)] {
        let existing = existing_ancestor(path);
        let matched = existing.as_ref().and_then(|path| {
            mounts
                .iter()
                .filter(|(mount, _)| path.starts_with(mount))
                .max_by_key(|(mount, _)| mount.components().count())
        });
        let mount_point = matched.map(|(path, _)| path.to_string_lossy().into_owned());
        if let Some(previous) = results
            .iter_mut()
            .find(|disk| mount_point.is_some() && disk.mount_point == mount_point)
        {
            previous.locations.push(label.into());
        } else {
            results.push(DiskUsage {
                locations: vec![label.into()],
                mount_point,
                capacity: matched.and_then(|(_, capacity)| capacity.clone()),
            });
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_capacity_is_not_zero_usage() {
        assert!(Capacity::new(0, 0).is_none());
        assert!(Capacity::new(10, 11).is_none());
        let capacity = Capacity::new(100, 15).unwrap();
        assert_eq!(capacity.used_bytes, 85);
        assert_eq!(capacity.used_percent, 85.0);
        assert_eq!(Capacity::new(100, 0).unwrap().used_percent, 100.0);
    }

    #[test]
    fn chooses_containing_volume_and_combines_shared_locations() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let mounts = vec![
            (root.clone(), Capacity::new(100, 50)),
            (nested.clone(), Capacity::new(200, 20)),
        ];
        let disks = disk_usage(
            &mounts,
            &root.join("new-data"),
            &nested.join("new-workspaces"),
        );
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[1].capacity.as_ref().unwrap().total_bytes, 200);
        let shared = disk_usage(&mounts, &root, &root.join("new-workspaces"));
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].locations, ["Data", "Workspaces"]);
        assert!(disk_usage(&[], &root, &root)[0].capacity.is_none());
    }

    #[test]
    fn first_cpu_sample_is_unknown_and_quick_reads_share_the_sample() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = ResourceMonitor::default();
        let first = monitor.sample(dir.path(), dir.path());
        assert!(first.cpu_percent.is_none());
        let cached = monitor.sample(dir.path(), dir.path());
        assert_eq!(first.sampled_at, cached.sampled_at);
        assert_eq!(first.memory, cached.memory);
    }
}
