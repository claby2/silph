//! CPU usage, from the aggregate `cpu` line of `/proc/stat`.
//!
//! The collector reports raw jiffy counters; the server computes a busy
//! percentage from the delta between consecutive scrapes, so no wall-clock or
//! core count is needed.

use std::io;

use crate::key::MetricKey;
use crate::metric::{CollectConfig, Metric, OutputSpec, Point, RawSnapshot, Unit};

/// Aggregate-line fields in `/proc/stat` order. Older kernels emit fewer
/// trailing fields (e.g. no `steal` before 2.6.11); missing ones are simply not
/// reported. `guest`/`guest_nice` are excluded: the kernel already accounts
/// guest time inside `user`/`nice`.
const FIELDS: [&str; 8] = [
    "cpu_user",
    "cpu_nice",
    "cpu_system",
    "cpu_idle",
    "cpu_iowait",
    "cpu_irq",
    "cpu_softirq",
    "cpu_steal",
];

const IDLE_FIELDS: [&str; 2] = ["cpu_idle", "cpu_iowait"];

pub struct Cpu;

impl Metric for Cpu {
    fn category(&self) -> &'static str {
        "cpu"
    }

    fn outputs(&self) -> &'static [OutputSpec] {
        &[OutputSpec {
            name: "cpu_usage_percent",
            unit: Unit::Percent,
            instanced: false,
        }]
    }

    fn collect(&self, _cfg: &CollectConfig) -> io::Result<Vec<(MetricKey, f64)>> {
        parse_proc_stat(&std::fs::read_to_string("/proc/stat")?)
    }

    fn process(&self, prev: Option<&RawSnapshot>, curr: &RawSnapshot) -> Vec<Point> {
        let Some(prev) = prev else { return vec![] };
        let sum = |snap: &RawSnapshot, fields: &[&str]| -> f64 {
            fields.iter().filter_map(|f| snap.get(f)).sum()
        };
        let idle_delta = sum(curr, &IDLE_FIELDS) - sum(prev, &IDLE_FIELDS);
        let total_delta = sum(curr, &FIELDS) - sum(prev, &FIELDS);
        let busy_delta = total_delta - idle_delta;
        // A negative delta means the counters reset (host reboot): skip the sample.
        if total_delta <= 0.0 || busy_delta < 0.0 || idle_delta < 0.0 {
            return vec![];
        }
        vec![Point::new(
            "cpu_usage_percent",
            busy_delta / total_delta * 100.0,
        )]
    }
}

fn parse_proc_stat(contents: &str) -> io::Result<Vec<(MetricKey, f64)>> {
    let line = contents
        .lines()
        .find(|l| l.starts_with("cpu "))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no aggregate cpu line"))?;
    let values = line
        .split_ascii_whitespace()
        .skip(1)
        .map(|v| {
            v.parse::<f64>()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        })
        .collect::<io::Result<Vec<f64>>>()?;
    if values.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty cpu line"));
    }
    Ok(FIELDS
        .iter()
        .zip(values)
        .map(|(name, value)| (MetricKey::new(*name), value))
        .collect())
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
    fn parses_modern_10_field_line() {
        let fixture = "cpu  1000 20 300 4000 50 6 7 8 9 10\n\
                       cpu0 500 10 150 2000 25 3 3 4 4 5\n\
                       intr 12345\n";
        let parsed = parse_proc_stat(fixture).unwrap();
        assert_eq!(parsed.len(), 8); // guest fields dropped
        assert_eq!(parsed[0], (MetricKey::new("cpu_user"), 1000.0));
        assert_eq!(parsed[7], (MetricKey::new("cpu_steal"), 8.0));
    }

    #[test]
    fn parses_old_kernel_with_fewer_fields() {
        let parsed = parse_proc_stat("cpu  100 0 50 800\n").unwrap();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[3], (MetricKey::new("cpu_idle"), 800.0));
    }

    #[test]
    fn rejects_missing_cpu_line() {
        assert!(parse_proc_stat("intr 1 2 3\n").is_err());
    }

    #[test]
    fn computes_busy_percent_from_deltas() {
        let prev = snapshot(&[
            ("cpu_user", 100.0),
            ("cpu_system", 50.0),
            ("cpu_idle", 850.0),
        ]);
        let curr = snapshot(&[
            ("cpu_user", 130.0),
            ("cpu_system", 60.0),
            ("cpu_idle", 910.0),
        ]);
        // busy delta = 40, total delta = 100
        assert_eq!(
            Cpu.process(Some(&prev), &curr),
            vec![Point::new("cpu_usage_percent", 40.0)]
        );
    }

    #[test]
    fn first_sample_emits_nothing() {
        let curr = snapshot(&[("cpu_user", 100.0), ("cpu_idle", 900.0)]);
        assert!(Cpu.process(None, &curr).is_empty());
    }

    #[test]
    fn counter_reset_skips_sample() {
        let prev = snapshot(&[("cpu_user", 5000.0), ("cpu_idle", 5000.0)]);
        let curr = snapshot(&[("cpu_user", 10.0), ("cpu_idle", 90.0)]);
        assert!(Cpu.process(Some(&prev), &curr).is_empty());
    }
}
