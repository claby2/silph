//! Memory usage, from `/proc/meminfo`. All values are gauges in bytes.

use std::io;

use crate::key::MetricKey;
use crate::metric::{CollectConfig, Metric, OutputSpec, Point, RawSnapshot, Unit};

/// `/proc/meminfo` fields worth reporting, and their wire names.
const FIELDS: [(&str, &str); 7] = [
    ("MemTotal", "memory_total"),
    ("MemFree", "memory_free"),
    ("MemAvailable", "memory_available"),
    ("Buffers", "memory_buffers"),
    ("Cached", "memory_cached"),
    ("SwapTotal", "memory_swap_total"),
    ("SwapFree", "memory_swap_free"),
];

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
        parse_meminfo(&std::fs::read_to_string("/proc/meminfo")?)
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

fn parse_meminfo(contents: &str) -> io::Result<Vec<(MetricKey, f64)>> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let Some((label, rest)) = line.split_once(':') else {
            continue;
        };
        let Some((_, wire_name)) = FIELDS.iter().find(|(l, _)| *l == label) else {
            continue;
        };
        let kb = rest
            .split_ascii_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("bad meminfo line: {line}"),
                )
            })?;
        out.push((MetricKey::new(*wire_name), kb * 1024.0));
    }
    if out.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no meminfo fields found",
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const FIXTURE: &str = "MemTotal:       16307904 kB\n\
                           MemFree:         8144296 kB\n\
                           MemAvailable:   12002888 kB\n\
                           Buffers:          268896 kB\n\
                           Cached:          3563260 kB\n\
                           SwapCached:            0 kB\n\
                           SwapTotal:       8388604 kB\n\
                           SwapFree:        8388604 kB\n\
                           Dirty:               492 kB\n";

    #[test]
    fn parses_selected_fields_as_bytes() {
        let parsed = parse_meminfo(FIXTURE).unwrap();
        assert_eq!(parsed.len(), 7);
        assert!(parsed.contains(&(MetricKey::new("memory_total"), 16307904.0 * 1024.0)));
        assert!(parsed.contains(&(MetricKey::new("memory_swap_free"), 8388604.0 * 1024.0)));
        // Unlisted fields like Dirty are not reported.
        assert!(!parsed.iter().any(|(k, _)| k.name.contains("dirty")));
    }

    #[test]
    fn process_computes_used_from_available() {
        let curr = RawSnapshot {
            ts_ms: 0,
            values: parse_meminfo(FIXTURE)
                .unwrap()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<BTreeMap<_, _>>(),
        };
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
        let fixture = "MemTotal: 1000 kB\nMemFree: 400 kB\n";
        let curr = RawSnapshot {
            ts_ms: 0,
            values: parse_meminfo(fixture)
                .unwrap()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<BTreeMap<_, _>>(),
        };
        let points = Memory.process(None, &curr);
        let used = points.iter().find(|p| p.name == "memory_used").unwrap();
        assert_eq!(used.value, 600.0 * 1024.0);
    }
}
