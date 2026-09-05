//! Disk usage per mount point, from `/proc/mounts` + `statvfs(3)`.
//!
//! Instanced metric: wire keys look like `disk_total:/home`. Mounts are either
//! listed explicitly in the collector config or auto-detected by filesystem
//! type, so dead network mounts never get a blocking statvfs call by default.
//!
//! `/proc/mounts` is parsed here rather than through `procfs::mounts()`, which
//! allocates a `HashMap<String, Option<String>>` of mount options plus a
//! `String` per field for every entry on every scrape, all so three fields can
//! be read.

use std::ffi::CString;
use std::fs;
use std::io;

use crate::key::MetricKey;
use crate::metric::{CollectConfig, Metric, OutputSpec, Point, RawSnapshot, Unit};

const PROC_MOUNTS: &str = "/proc/mounts";

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
        // Borrow the configured list rather than cloning it every scrape.
        let detected;
        let mounts: &[String] = match &cfg.disk_mounts {
            Some(mounts) => mounts,
            None => {
                detected = select_mounts(&fs::read_to_string(PROC_MOUNTS)?);
                &detected
            }
        };
        let mut out = Vec::with_capacity(mounts.len() * 2);
        for mount in mounts {
            match statvfs(mount) {
                Ok((total, free)) => {
                    out.push((MetricKey::with_instance("disk_total", mount), total));
                    out.push((MetricKey::with_instance("disk_free", mount), free));
                }
                // A single unreadable mount shouldn't fail the whole scrape.
                Err(_) => continue,
            }
        }
        Ok(out)
    }

    fn process(&self, _prev: Option<&RawSnapshot>, curr: &RawSnapshot) -> Vec<Point> {
        let mut points = Vec::new();
        // One reused buffer for the paired `disk_free:<instance>` lookups;
        // building a fresh key per mount allocated twice per mount per scrape.
        let mut free_key = String::new();
        for (instance, total) in curr.for_field("disk_total") {
            free_key.clear();
            free_key.push_str("disk_free:");
            free_key.push_str(instance);
            let Some(free) = curr.get(&free_key) else {
                continue;
            };
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

/// Pick mount points from `/proc/mounts` content: allowlisted filesystem
/// types, first mount per device (bind mounts and btrfs subvolumes repeat the
/// device).
fn select_mounts(mounts: &str) -> Vec<String> {
    let mut seen_devices: Vec<&str> = Vec::new();
    let mut selected = Vec::new();
    for line in mounts.lines() {
        let mut fields = line.split_ascii_whitespace();
        let (Some(spec), Some(file), Some(vfstype)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !FSTYPE_ALLOWLIST.contains(&vfstype) || seen_devices.contains(&spec) {
            continue;
        }
        // Devices are only ever compared against each other, so they stay in
        // their mangled form; the mount point is used as a real path and as
        // the series instance, so it gets unescaped.
        seen_devices.push(spec);
        selected.push(unmangle_octal(file));
    }
    selected
}

/// Undoes the `\nnn` octal escapes the kernel's `mangle_path` writes for
/// space, tab, newline and backslash. Only ASCII escapes are decoded, which is
/// all the kernel emits; anything else is left alone rather than guessing at
/// an encoding.
fn unmangle_octal(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut rest = field;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        match rest
            .get(at + 1..at + 4)
            .and_then(|digits| u8::from_str_radix(digits, 8).ok())
            .filter(|byte| byte.is_ascii())
        {
            Some(byte) => {
                out.push(byte as char);
                rest = &rest[at + 4..];
            }
            None => {
                out.push('\\');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out
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

    #[test]
    fn selects_allowlisted_mounts_deduped_by_device() {
        // Real /proc/mounts shape. An empty leading line is ignored, and
        // `\040` is how the kernel escapes a space in a mount point.
        let mounts = "
proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
/dev/nvme0n1p2 /home ext4 rw,relatime 0 0
/dev/sda1 /mnt/backup\\040drive xfs rw,relatime 0 0
tmpfs /tmp tmpfs rw,nosuid,nodev 0 0
10.0.0.5:/export /mnt/nfs nfs4 rw,relatime 0 0
";
        // /home shares its device with / (first wins); the rest are not
        // allowlisted filesystem types.
        assert_eq!(select_mounts(mounts), vec!["/", "/mnt/backup drive"]);
    }

    #[test]
    fn unmangles_only_complete_ascii_escapes() {
        assert_eq!(unmangle_octal("/mnt/plain"), "/mnt/plain");
        assert_eq!(unmangle_octal(r"a\040b\011c\012d\134e"), "a b\tc\nd\\e");
        // Not a complete octal escape: left exactly as written.
        assert_eq!(unmangle_octal(r"/mnt/back\slash"), r"/mnt/back\slash");
        assert_eq!(unmangle_octal(r"/mnt/trailing\04"), r"/mnt/trailing\04");
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
