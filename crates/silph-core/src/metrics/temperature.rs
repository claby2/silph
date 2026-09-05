//! Hardware temperatures, from the kernel's hwmon sysfs interface:
//! `/sys/class/hwmon/hwmon*/temp*_input`, in millidegrees Celsius.
//!
//! Instanced metric: one series per sensor, named `<chip>/<label>` after the
//! chip's `name` attribute and the sensor's `temp*_label`, e.g. `k10temp/Tctl`
//! or `nvme/Composite`. A sensor without a label falls back to its attribute
//! name (`iwlwifi_1/temp1`). Chips sharing a name (two NVMe drives, say) are
//! qualified by their hwmon directory, `nvme[hwmon0]/Composite`; hwmon
//! numbering is assigned at boot, so those instances may swap between reboots.
//!
//! Sensors can be allowlisted in the collector config; by default every
//! readable sensor is reported.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::key::MetricKey;
use crate::metric::{CollectConfig, Metric, OutputSpec, Point, RawSnapshot, Unit};

const HWMON_ROOT: &str = "/sys/class/hwmon";

pub struct Temperature;

impl Metric for Temperature {
    fn category(&self) -> &'static str {
        "temperature"
    }

    fn outputs(&self) -> &'static [OutputSpec] {
        &[OutputSpec {
            name: "temperature_celsius",
            unit: Unit::Celsius,
            instanced: true,
        }]
    }

    fn collect(&self, cfg: &CollectConfig) -> io::Result<Vec<(MetricKey, f64)>> {
        collect_from(Path::new(HWMON_ROOT), cfg)
    }

    fn process(&self, _prev: Option<&RawSnapshot>, curr: &RawSnapshot) -> Vec<Point> {
        curr.for_field("temperature_celsius")
            .map(|(instance, value)| Point::with_instance("temperature_celsius", instance, value))
            .collect()
    }
}

fn collect_from(root: &Path, cfg: &CollectConfig) -> io::Result<Vec<(MetricKey, f64)>> {
    Ok(read_sensors(root)?
        .into_iter()
        .filter(|(instance, _)| {
            cfg.temperature_sensors
                .as_ref()
                .is_none_or(|allow| allow.contains(instance))
        })
        .map(|(instance, celsius)| {
            (
                MetricKey::with_instance("temperature_celsius", instance),
                celsius,
            )
        })
        .collect())
}

/// Every readable temperature sensor under `root` as `(instance, celsius)`,
/// ordered by hwmon index then sensor index. A missing `root` is an error (the
/// metric was enabled on a host without hwmon); an empty one is simply no
/// sensors.
fn read_sensors(root: &Path) -> io::Result<Vec<(String, f64)>> {
    let mut chips: Vec<(u32, PathBuf)> = fs::read_dir(root)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let index = indexed_name(&entry.file_name().to_string_lossy(), "hwmon", "")?;
            Some((index, entry.path()))
        })
        .collect();
    chips.sort();

    // Chips whose `name` is shared get qualified by their hwmon directory so
    // their sensors don't collide.
    let names: Vec<String> = chips.iter().map(|(_, dir)| chip_name(dir)).collect();
    let mut sensors = Vec::new();
    for ((_, dir), name) in chips.iter().zip(&names) {
        let chip = if names.iter().filter(|n| *n == name).count() > 1 {
            format!(
                "{name}[{}]",
                dir.file_name().unwrap_or_default().to_string_lossy()
            )
        } else {
            name.clone()
        };
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut inputs: Vec<(u32, String)> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let attr = entry.file_name().to_string_lossy().into_owned();
                let index = indexed_name(&attr, "temp", "_input")?;
                Some((index, attr.trim_end_matches("_input").to_string()))
            })
            .collect();
        inputs.sort();
        for (_, stem) in inputs {
            // A sensor that fails to read (device asleep, driver error)
            // shouldn't take the rest of the chip with it.
            let Some(celsius) = read_trimmed(&dir.join(format!("{stem}_input")))
                .and_then(|raw| raw.parse::<f64>().ok())
                .map(|millidegrees| millidegrees / 1000.0)
            else {
                continue;
            };
            let label = read_trimmed(&dir.join(format!("{stem}_label")))
                .filter(|label| !label.is_empty())
                .unwrap_or(stem);
            sensors.push((format!("{chip}/{label}"), celsius));
        }
    }
    Ok(sensors)
}

/// Parses `<prefix><n><suffix>` into `n`.
fn indexed_name(name: &str, prefix: &str, suffix: &str) -> Option<u32> {
    name.strip_prefix(prefix)?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

/// The chip's `name` attribute, falling back to the directory name.
fn chip_name(dir: &Path) -> String {
    read_trimmed(&dir.join("name"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            dir.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// `(attribute stem, raw input, label)`, e.g. `("temp1", "36375", Some("Tctl"))`.
    type Sensor<'a> = (&'a str, &'a str, Option<&'a str>);
    /// `(directory, chip name, sensors)`, e.g. `("hwmon0", Some("k10temp"), ..)`.
    type Chip<'a> = (&'a str, Option<&'a str>, &'a [Sensor<'a>]);

    /// Builds a fake hwmon tree under a temporary directory.
    fn fixture(chips: &[Chip]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (dir, name, sensors) in chips {
            let dir = root.path().join(dir);
            fs::create_dir(&dir).unwrap();
            if let Some(name) = name {
                fs::write(dir.join("name"), format!("{name}\n")).unwrap();
            }
            for (stem, input, label) in *sensors {
                fs::write(dir.join(format!("{stem}_input")), format!("{input}\n")).unwrap();
                if let Some(label) = label {
                    fs::write(dir.join(format!("{stem}_label")), format!("{label}\n")).unwrap();
                }
            }
        }
        root
    }

    #[test]
    fn reads_sensors_in_hwmon_order_with_label_fallback() {
        let root = fixture(&[
            // Numeric order: hwmon2 sorts before hwmon10.
            (
                "hwmon10",
                Some("iwlwifi_1"),
                &[("temp1", "47000", Some(""))],
            ),
            (
                "hwmon2",
                Some("k10temp"),
                &[
                    ("temp3", "35000", Some("Tccd1")),
                    ("temp1", "36375", Some("Tctl")),
                ],
            ),
            ("hwmon3", None, &[("temp1", "20500", None)]),
            ("not-a-chip", Some("bogus"), &[("temp1", "1000", None)]),
        ]);
        assert_eq!(
            read_sensors(root.path()).unwrap(),
            vec![
                ("k10temp/Tctl".to_string(), 36.375),
                ("k10temp/Tccd1".to_string(), 35.0),
                ("hwmon3/temp1".to_string(), 20.5),
                ("iwlwifi_1/temp1".to_string(), 47.0),
            ]
        );
    }

    #[test]
    fn duplicate_chip_names_are_qualified_by_directory() {
        let root = fixture(&[
            (
                "hwmon0",
                Some("nvme"),
                &[("temp1", "36850", Some("Composite"))],
            ),
            (
                "hwmon1",
                Some("nvme"),
                &[("temp1", "41000", Some("Composite"))],
            ),
            (
                "hwmon2",
                Some("k10temp"),
                &[("temp1", "36375", Some("Tctl"))],
            ),
        ]);
        assert_eq!(
            read_sensors(root.path()).unwrap(),
            vec![
                ("nvme[hwmon0]/Composite".to_string(), 36.85),
                ("nvme[hwmon1]/Composite".to_string(), 41.0),
                ("k10temp/Tctl".to_string(), 36.375),
            ]
        );
    }

    #[test]
    fn unreadable_sensor_is_skipped() {
        let root = fixture(&[(
            "hwmon0",
            Some("acpitz"),
            &[("temp1", "not a number", None), ("temp2", "27800", None)],
        )]);
        assert_eq!(
            read_sensors(root.path()).unwrap(),
            vec![("acpitz/temp2".to_string(), 27.8)]
        );
    }

    #[test]
    fn missing_root_is_an_error_but_empty_root_is_not() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_sensors(root.path()).unwrap().is_empty());
        assert!(read_sensors(&root.path().join("nope")).is_err());
    }

    #[test]
    fn allowlist_filters_by_instance() {
        let root = fixture(&[
            (
                "hwmon0",
                Some("k10temp"),
                &[("temp1", "36375", Some("Tctl"))],
            ),
            (
                "hwmon1",
                Some("nvme"),
                &[("temp1", "36850", Some("Composite"))],
            ),
        ]);
        let cfg = CollectConfig {
            temperature_sensors: Some(vec!["nvme/Composite".to_string()]),
            ..Default::default()
        };
        assert_eq!(
            collect_from(root.path(), &cfg).unwrap(),
            vec![(
                MetricKey::with_instance("temperature_celsius", "nvme/Composite"),
                36.85
            )]
        );
    }

    #[test]
    fn process_passes_gauges_through_per_instance() {
        let curr = RawSnapshot {
            ts_ms: 0,
            values: BTreeMap::from([
                ("temperature_celsius:k10temp/Tctl".to_string(), 36.375),
                ("temperature_celsius:nvme/Composite".to_string(), 36.85),
                ("cpu_user".to_string(), 1.0),
            ]),
        };
        assert_eq!(
            Temperature.process(None, &curr),
            vec![
                Point::with_instance("temperature_celsius", "k10temp/Tctl", 36.375),
                Point::with_instance("temperature_celsius", "nvme/Composite", 36.85),
            ]
        );
    }
}
