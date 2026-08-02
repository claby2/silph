use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The collector's scrape response: a flat JSON object of raw metric values,
/// e.g. `{"cpu_user": 12345.0, "memory_total": 8.2e9, "disk_free:/": 1.1e10}`.
///
/// Keys parse as [`crate::MetricKey`]. Unknown keys are preserved by
/// construction (they simply land in the map), so mixed collector/server
/// versions tolerate each other.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricsResponse {
    #[serde(flatten)]
    pub values: BTreeMap<String, f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_json_round_trip() {
        let mut response = MetricsResponse::default();
        response.values.insert("cpu_user".into(), 123.0);
        response.values.insert("disk_free:/".into(), 4.5e9);
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(json, r#"{"cpu_user":123.0,"disk_free:/":4500000000.0}"#);
        assert_eq!(
            serde_json::from_str::<MetricsResponse>(&json).unwrap(),
            response
        );
    }
}
