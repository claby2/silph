use std::collections::BTreeMap;
use std::io;

use crate::key::MetricKey;

/// One scrape's worth of raw values from a collector, keyed by wire key.
#[derive(Debug, Clone)]
pub struct RawSnapshot {
    /// Collection timestamp, milliseconds since the Unix epoch.
    pub ts_ms: i64,
    pub values: BTreeMap<String, f64>,
}

impl RawSnapshot {
    pub fn get(&self, key: &str) -> Option<f64> {
        self.values.get(key).copied()
    }

    /// Iterate the values of one instanced field, e.g. `for_field("disk_total")`
    /// yields `("/", v)` and `("/home", v)` from `disk_total:/` etc.
    pub fn for_field<'a>(&'a self, name: &'a str) -> impl Iterator<Item = (&'a str, f64)> {
        self.values.iter().filter_map(move |(k, v)| {
            let (n, instance) = k.split_once(':')?;
            (n == name).then_some((instance, *v))
        })
    }
}

/// A processed value the server stores in the time-series database.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub name: &'static str,
    /// Set for per-resource metrics (e.g. the mount point for disk metrics);
    /// becomes a label on the stored series.
    pub instance: Option<String>,
    pub value: f64,
}

impl Point {
    pub fn new(name: &'static str, value: f64) -> Self {
        Point {
            name,
            instance: None,
            value,
        }
    }

    pub fn with_instance(name: &'static str, instance: impl Into<String>, value: f64) -> Self {
        Point {
            name,
            instance: Some(instance.into()),
            value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Percent,
    Bytes,
    Count,
}

impl Unit {
    pub fn as_str(&self) -> &'static str {
        match self {
            Unit::Percent => "percent",
            Unit::Bytes => "bytes",
            Unit::Count => "count",
        }
    }
}

/// Describes one processed series a metric emits; drives the server's
/// `/api/metrics` endpoint and therefore dashboard chart discovery.
#[derive(Debug, Clone, Copy)]
pub struct OutputSpec {
    pub name: &'static str,
    pub unit: Unit,
    /// True if the series exists per instance (e.g. per mount point).
    pub instanced: bool,
}

/// Collector-side configuration knobs passed to `collect`.
#[derive(Debug, Clone, Default)]
pub struct CollectConfig {
    /// Explicit list of mount points to report disk usage for. When unset,
    /// mounts are auto-detected by filesystem type.
    pub disk_mounts: Option<Vec<String>>,
}

/// A metric category, defined once and shared by collector and server.
///
/// `collect` runs on the monitored host and returns raw values (counters are
/// fine); `process` runs on the server and turns raw snapshots into the values
/// actually stored, using the previous snapshot for counter deltas.
pub trait Metric: Sync {
    /// Wire-key prefix, e.g. `"cpu"` for `cpu_user`, `cpu_idle`, ...
    fn category(&self) -> &'static str;

    /// The processed series this metric emits from `process`.
    fn outputs(&self) -> &'static [OutputSpec];

    fn collect(&self, cfg: &CollectConfig) -> io::Result<Vec<(MetricKey, f64)>>;

    fn process(&self, prev: Option<&RawSnapshot>, curr: &RawSnapshot) -> Vec<Point>;
}
