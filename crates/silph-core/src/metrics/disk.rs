//! Disk usage per mount point, from `/proc/mounts` (via procfs) + `statvfs(3)`.
//!
//! Instanced metric: wire keys look like `disk_total:/home`. Mounts are either
//! listed explicitly in the collector config or auto-detected by filesystem
//! type, so dead network mounts never get a blocking statvfs call by default.

use std::ffi::CString;
use std::io;

use procfs::MountEntry;

use crate::key::MetricKey;
use crate::metric::{CollectConfig, Metric, OutputSpec, Point, RawSnapshot, Unit};

/// Local filesystem types worth reporting when auto-detecting.
const FSTYPE_ALLOWLIST: [&str; 7] = ["ext4", "ext3", "xfs", "btrfs", "f2fs", "vfat", "zfs"];

pub struct Disk;

impl Metric for Disk {
    fn category(&self) -> &'static str {
        "disk"
    }

    fn outputs(&self) -> &'static [OutputSpec] {
        &[
            OutputSpec {
                name: "disk_total",
                unit: Unit::Bytes,
                instanced: true,
            },
            OutputSpec {
                name: "disk_used",
                unit: Unit::Bytes,
                instanced: true,
            },
            OutputSpec {
                name: "disk_used_percent",
                unit: Unit::Percent,
                instanced: true,
            },
        ]
    }

    fn collect(&self, cfg: &CollectConfig) -> io::Result<Vec<(MetricKey, f64)>> {
        let mounts = match &cfg.disk_mounts {
            Some(mounts) => mounts.clone(),
            None => select_mounts(&procfs::mounts().map_err(io::Error::other)?),
        };
        let mut out = Vec::new();
        for mount in mounts {
            match statvfs(&mount) {
                Ok((total, free)) => {
                    out.push((MetricKey::with_instance("disk_total", &mount), total));
                    out.push((MetricKey::with_instance("disk_free", &mount), free));
                }
                // A single unreadable mount shouldn't fail the whole scrape.
                Err(_) => continue,
            }
        }
        Ok(out)
    }

    fn process(&self, _prev: Option<&RawSnapshot>, curr: &RawSnapshot) -> Vec<Point> {
        let mut points = Vec::new();
        for (instance, total) in curr.for_field("disk_total") {
            let key = MetricKey::with_instance("disk_free", instance).to_string();
            let Some(free) = curr.get(&key) else { continue };
            let used = (total - free).max(0.0);
            points.push(Point::with_instance("disk_total", instance, total));
            points.push(Point::with_instance("disk_used", instance, used));
            if total > 0.0 {
                points.push(Point::with_instance(
                    "disk_used_percent",
                    instance,
                    used / total * 100.0,
                ));
            }
        }
        points
    }
}

/// Pick mount points: allowlisted filesystem types, first mount per device
/// (bind mounts and btrfs subvolumes repeat the device). procfs has already
/// unescaped the octal sequences `/proc/mounts` uses for spaces etc.
fn select_mounts(entries: &[MountEntry]) -> Vec<String> {
    let mut seen_devices: Vec<&str> = Vec::new();
    let mut mounts = Vec::new();
    for entry in entries {
        if !FSTYPE_ALLOWLIST.contains(&entry.fs_vfstype.as_str())
            || seen_devices.contains(&entry.fs_spec.as_str())
        {
            continue;
        }
        seen_devices.push(&entry.fs_spec);
        mounts.push(entry.fs_file.clone());
    }
    mounts
}

/// Returns (total_bytes, free_bytes) for the filesystem at `path`. Free space
/// is `f_bavail` — what an unprivileged process can use, matching `df(1)`.
fn statvfs(path: &str) -> io::Result<(f64, f64)> {
    let c_path = CString::new(path).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let frsize = stat.f_frsize as f64;
    Ok((frsize * stat.f_blocks as f64, frsize * stat.f_bavail as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::collections::HashMap;

    fn entry(device: &str, mount: &str, fstype: &str) -> MountEntry {
        MountEntry {
            fs_spec: device.to_string(),
            fs_file: mount.to_string(),
            fs_vfstype: fstype.to_string(),
            fs_mntops: HashMap::new(),
            fs_freq: 0,
            fs_passno: 0,
        }
    }

    #[test]
    fn selects_allowlisted_mounts_deduped_by_device() {
        let entries = [
            entry("proc", "/proc", "proc"),
            entry("/dev/nvme0n1p2", "/", "ext4"),
            // Shares the device with / (first wins).
            entry("/dev/nvme0n1p2", "/home", "ext4"),
            entry("/dev/sda1", "/mnt/backup drive", "xfs"),
            entry("tmpfs", "/tmp", "tmpfs"),
            entry("10.0.0.5:/export", "/mnt/nfs", "nfs4"),
        ];
        assert_eq!(select_mounts(&entries), vec!["/", "/mnt/backup drive"]);
    }

    #[test]
    fn process_pairs_total_and_free_per_instance() {
        let curr = RawSnapshot {
            ts_ms: 0,
            values: BTreeMap::from([
                ("disk_total:/".to_string(), 1000.0),
                ("disk_free:/".to_string(), 250.0),
                ("disk_total:/home".to_string(), 2000.0),
                ("disk_free:/home".to_string(), 1000.0),
                // free without total: ignored
                ("disk_free:/orphan".to_string(), 5.0),
            ]),
        };
        let points = Disk.process(None, &curr);
        let get = |name: &str, instance: &str| {
            points
                .iter()
                .find(|p| p.name == name && p.instance.as_deref() == Some(instance))
                .map(|p| p.value)
        };
        assert_eq!(get("disk_used", "/"), Some(750.0));
        assert_eq!(get("disk_used_percent", "/"), Some(75.0));
        assert_eq!(get("disk_used", "/home"), Some(1000.0));
        assert!(
            !points
                .iter()
                .any(|p| p.instance.as_deref() == Some("/orphan"))
        );
    }

    #[test]
    fn statvfs_on_root_returns_sane_values() {
        let (total, free) = statvfs("/").unwrap();
        assert!(total > 0.0);
        assert!(free >= 0.0 && free <= total);
    }
}
