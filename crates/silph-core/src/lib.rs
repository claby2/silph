//! Shared metric definitions for silph.
//!
//! Every metric is defined once, in one file under [`metrics`], as an
//! implementation of [`Metric`]: the collector calls its `collect` half and the
//! server calls its `process` half. Adding a metric means adding one file and
//! one entry to [`METRICS`].

pub mod key;
pub mod metric;
pub mod metrics;
pub mod wire;

pub use key::MetricKey;
pub use metric::{CollectConfig, Metric, OutputSpec, Point, RawSnapshot, Unit};
pub use wire::MetricsResponse;

/// The registry of all metrics known to silph, in display order.
pub static METRICS: &[&dyn Metric] = &[
    &metrics::cpu::Cpu,
    &metrics::memory::Memory,
    &metrics::disk::Disk,
];
