//! CPU usage, from the aggregate line of `/proc/stat` via procfs.
//!
//! The collector reports raw jiffy counters; the server computes a busy
//! percentage from the delta between consecutive scrapes, so no wall-clock or
//! core count is needed.

use std::io;

use procfs::prelude::*;

use crate::key::MetricKey;
use crate::metric::{CollectConfig, Metric, OutputSpec, Point, RawSnapshot, Unit};

/// All reported wire fields. Fields absent on older kernels (e.g. `steal`
/// before 2.6.11) are simply not reported. `guest`/`guest_nice` are excluded:
/// the kernel already accounts guest time inside `user`/`nice`.
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
        let cpu = procfs::KernelStats::current()
            .map_err(io::Error::other)?
            .total;
        let mut out = vec![
            (MetricKey::new("cpu_user"), cpu.user as f64),
            (MetricKey::new("cpu_nice"), cpu.nice as f64),
            (MetricKey::new("cpu_system"), cpu.system as f64),
            (MetricKey::new("cpu_idle"), cpu.idle as f64),
        ];
        for (name, value) in [
            ("cpu_iowait", cpu.iowait),
            ("cpu_irq", cpu.irq),
            ("cpu_softirq", cpu.softirq),
            ("cpu_steal", cpu.steal),
        ] {
            if let Some(value) = value {
                out.push((MetricKey::new(name), value as f64));
            }
        }
        Ok(out)
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
    fn collect_reports_monotonic_jiffies() {
        let values = Cpu.collect(&CollectConfig::default()).unwrap();
        // The always-present fields come first; every value is a counter >= 0.
        assert!(values.len() >= 4);
        assert_eq!(values[0].0, MetricKey::new("cpu_user"));
        assert!(values.iter().all(|(_, v)| *v >= 0.0));
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
