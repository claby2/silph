//! All tsink contact lives here, so pre-1.0 API changes stay localized.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use silph_core::Point;
use tsink::{
    Aggregation, DataPoint, Label, QueryOptions, Row, Storage, StorageBuilder, TimestampPrecision,
    Value,
};

#[derive(Clone)]
pub struct Store {
    inner: Arc<dyn Storage>,
}

impl Store {
    pub fn open(data_dir: &Path, retention: Duration) -> tsink::Result<Store> {
        let inner = StorageBuilder::new()
            .with_data_path(data_dir)
            .with_retention(retention)
            .with_timestamp_precision(TimestampPrecision::Milliseconds)
            .build()?;
        Ok(Store { inner })
    }

    /// Insert one host's processed points at the given scrape timestamp.
    /// Series are labeled `host`, plus `instance` for per-resource metrics.
    pub async fn insert(&self, host: &str, ts_ms: i64, points: Vec<Point>) -> tsink::Result<()> {
        let inner = self.inner.clone();
        let host = host.to_string();
        tokio::task::spawn_blocking(move || {
            let rows: Vec<Row> = points
                .into_iter()
                .map(|p| {
                    let mut labels = vec![Label::new("host", &host)];
                    if let Some(instance) = &p.instance {
                        labels.push(Label::new("instance", instance));
                    }
                    Row::with_labels(p.name, labels, DataPoint::new(ts_ms, p.value))
                })
                .collect();
            inner.insert_rows(&rows)
        })
        .await
        .expect("storage insert task panicked")
    }

    /// Range query for one series, downsampled into `step_ms` buckets
    /// (average). Returns (bucket timestamp ms, value) pairs.
    pub async fn query(
        &self,
        metric: &str,
        host: &str,
        instance: Option<&str>,
        start_ms: i64,
        end_ms: i64,
        step_ms: i64,
    ) -> tsink::Result<Vec<(i64, f64)>> {
        let inner = self.inner.clone();
        let metric = metric.to_string();
        let mut labels = vec![Label::new("host", host)];
        if let Some(instance) = instance {
            labels.push(Label::new("instance", instance));
        }
        tokio::task::spawn_blocking(move || {
            let opts = QueryOptions::new(start_ms, end_ms)
                .with_labels(labels)
                .with_downsample(step_ms, Aggregation::Avg);
            let points = inner.select_with_options(&metric, opts)?;
            Ok(points
                .iter()
                .filter_map(|p| Some((p.timestamp, value_to_f64(&p.value)?)))
                .collect())
        })
        .await
        .expect("storage query task panicked")
    }

    /// Flush and close the engine; call once on shutdown.
    pub fn close(&self) -> tsink::Result<()> {
        self.inner.close()
    }
}

fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::F64(v) => Some(*v),
        Value::I64(v) => Some(*v as f64),
        Value::U64(v) => Some(*v as f64),
        _ => None,
    }
}
