//! CPU usage, from the aggregate `cpu` line of `/proc/stat`.
//!
//! The collector reports raw jiffy counters; the server computes a busy
//! percentage from the delta between consecutive scrapes, so no wall-clock or
//! core count is needed.
//!
//! The line is parsed directly rather than through procfs, whose `KernelStats`
//! allocates a `CpuTime` per core (plus `ctxt`, `btime`, ...) on every scrape
//! when only the aggregate is ever used.

use std::fs::File;
use std::io::{self, BufRead, BufReader};

use crate::key::MetricKey;
use crate::metric::{CollectConfig, Metric, OutputSpec, Point, RawSnapshot, Unit};

const PROC_STAT: &str = "/proc/stat";

/// All reported wire fields, in the order `/proc/stat` lists them. Fields
/// absent on older kernels (e.g. `steal` before 2.6.11) are simply not
/// reported. `guest`/`guest_nice` follow `steal` on the line and are excluded
/// by stopping here: the kernel already accounts guest time inside
/// `user`/`nice`.
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

/// The fields that must be present for the line to be usable at all; `process`
/// needs idle and a total to divide by.
const REQUIRED_FIELDS: usize = 4;

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
        // The aggregate is the first line, so one buffered read reaches it
        // without paying to parse the per-core lines behind it.
        let mut line = String::new();
        BufReader::new(File::open(PROC_STAT)?).read_line(&mut line)?;
        parse_total_line(&line)
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

/// Parses the aggregate `cpu` line into wire fields in [`FIELDS`] order.
/// Trailing counters the kernel doesn't emit are left out, matching the
/// tolerance the flat wire format already has for missing keys.
fn parse_total_line(line: &str) -> io::Result<Vec<(MetricKey, f64)>> {
    let invalid = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
    let mut tokens = line.split_ascii_whitespace();
    if tokens.next() != Some("cpu") {
        return Err(invalid(format!(
            "{PROC_STAT}: expected the aggregate `cpu` line first"
        )));
    }
    let mut out = Vec::with_capacity(FIELDS.len());
    for (name, token) in FIELDS.iter().zip(tokens) {
        let value: u64 = token
            .parse()
            .map_err(|_| invalid(format!("{PROC_STAT}: {name} is not a counter: {token:?}")))?;
        out.push((MetricKey::new(*name), value as f64));
    }
    if out.len() < REQUIRED_FIELDS {
        return Err(invalid(format!(
            "{PROC_STAT}: aggregate cpu line has {} fields, need {REQUIRED_FIELDS}",
            out.len()
        )));
    }
    Ok(out)
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

    #[test]
    fn parses_full_line_and_stops_before_guest() {
        // A modern kernel emits 10 counters; the last two are guest/guest_nice.
        let line = "cpu  100 20 30 900 5 1 2 3 40 4\n";
        let values = parse_total_line(line).unwrap();
        assert_eq!(
            values,
            FIELDS
                .iter()
                .zip([100.0, 20.0, 30.0, 900.0, 5.0, 1.0, 2.0, 3.0])
                .map(|(name, v)| (MetricKey::new(*name), v))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn omits_trailing_fields_an_old_kernel_does_not_emit() {
        let values = parse_total_line("cpu 100 20 30 900").unwrap();
        assert_eq!(values.len(), 4);
        assert_eq!(values[3], (MetricKey::new("cpu_idle"), 900.0));
    }

    #[test]
    fn rejects_lines_that_are_not_a_usable_aggregate() {
        // Per-core line, too few counters, and a non-numeric counter.
        assert!(parse_total_line("cpu0 100 20 30 900").is_err());
        assert!(parse_total_line("cpu 100 20 30").is_err());
        assert!(parse_total_line("cpu 100 20 30 nine").is_err());
    }
}
