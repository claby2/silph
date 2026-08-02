//! Memory usage, from `/proc/meminfo` via procfs. All values are gauges in
//! bytes.

use std::io;

use procfs::prelude::*;

use crate::key::MetricKey;
use crate::metric::{CollectConfig, Metric, OutputSpec, Point, RawSnapshot, Unit};

pub struct Memory;

impl Metric for Memory {
    fn category(&self) -> &'static str {
        "memory"
    }

    fn outputs(&self) -> &'static [OutputSpec] {
        &[
            OutputSpec {
                name: "memory_total",
                unit: Unit::Bytes,
                instanced: false,
            },
            OutputSpec {
                name: "memory_used",
                unit: Unit::Bytes,
                instanced: false,
            },
            OutputSpec {
                name: "memory_used_percent",
                unit: Unit::Percent,
                instanced: false,
            },
            OutputSpec {
                name: "memory_swap_total",
                unit: Unit::Bytes,
                instanced: false,
            },
            OutputSpec {
                name: "memory_swap_used",
                unit: Unit::Bytes,
                instanced: false,
            },
        ]
    }

    fn collect(&self, _cfg: &CollectConfig) -> io::Result<Vec<(MetricKey, f64)>> {
        let info = procfs::Meminfo::current().map_err(io::Error::other)?;
        let mut out = vec![
            (MetricKey::new("memory_total"), info.mem_total as f64),
            (MetricKey::new("memory_free"), info.mem_free as f64),
            (MetricKey::new("memory_buffers"), info.buffers as f64),
            (MetricKey::new("memory_cached"), info.cached as f64),
            (MetricKey::new("memory_swap_total"), info.swap_total as f64),
            (MetricKey::new("memory_swap_free"), info.swap_free as f64),
        ];
        // Absent on pre-3.14 kernels; process() falls back to MemFree.
        if let Some(available) = info.mem_available {
            out.push((MetricKey::new("memory_available"), available as f64));
        }
        Ok(out)
    }

    fn process(&self, _prev: Option<&RawSnapshot>, curr: &RawSnapshot) -> Vec<Point> {
        let mut points = Vec::new();
        let Some(total) = curr.get("memory_total") else {
            return points;
        };
        points.push(Point::new("memory_total", total));
        // "Used" follows the `free(1)` convention: total minus available, which
        // unlike total-minus-free excludes reclaimable cache. Fall back to
        // MemFree on pre-3.14 kernels without MemAvailable.
        let available = curr
            .get("memory_available")
            .or_else(|| curr.get("memory_free"));
        if let Some(available) = available {
            let used = (total - available).max(0.0);
            points.push(Point::new("memory_used", used));
            if total > 0.0 {
                points.push(Point::new("memory_used_percent", used / total * 100.0));
            }
        }
        if let (Some(swap_total), Some(swap_free)) =
            (curr.get("memory_swap_total"), curr.get("memory_swap_free"))
        {
            points.push(Point::new("memory_swap_total", swap_total));
            points.push(Point::new(
                "memory_swap_used",
                (swap_total - swap_free).max(0.0),
            ));
        }
        points
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn snapshot(pairs: &[(&str, f64)]) -> RawSnapshot {
        RawSnapshot {
            ts_ms: 0,
            values: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn collect_reports_meminfo_fields_in_bytes() {
        let values = Memory.collect(&CollectConfig::default()).unwrap();
        let get = |name: &str| values.iter().find(|(k, _)| k.name == name).map(|(_, v)| *v);
        let total = get("memory_total").unwrap();
        // Plausibility: more than 1 MiB of RAM, and free never exceeds total.
        assert!(total > 1024.0 * 1024.0);
        assert!(get("memory_free").unwrap() <= total);
    }

    #[test]
    fn process_computes_used_from_available() {
        let curr = snapshot(&[
            ("memory_total", 16307904.0 * 1024.0),
            ("memory_free", 8144296.0 * 1024.0),
            ("memory_available", 12002888.0 * 1024.0),
            ("memory_swap_total", 8388604.0 * 1024.0),
            ("memory_swap_free", 8388604.0 * 1024.0),
        ]);
        let points = Memory.process(None, &curr);
        let get = |name: &str| points.iter().find(|p| p.name == name).unwrap().value;
        let expected_used = (16307904.0 - 12002888.0) * 1024.0;
        assert_eq!(get("memory_used"), expected_used);
        let percent = get("memory_used_percent");
        assert!((percent - 26.4).abs() < 0.1, "got {percent}");
        assert_eq!(get("memory_swap_used"), 0.0);
    }

    #[test]
    fn falls_back_to_memfree_without_memavailable() {
        let curr = snapshot(&[
            ("memory_total", 1000.0 * 1024.0),
            ("memory_free", 400.0 * 1024.0),
        ]);
        let points = Memory.process(None, &curr);
        let used = points.iter().find(|p| p.name == "memory_used").unwrap();
        assert_eq!(used.value, 600.0 * 1024.0);
    }
}
