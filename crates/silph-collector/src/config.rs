use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;
use silph_core::{CollectConfig, METRICS};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: String,
    /// Bearer token the server must present when scraping. Omit to serve
    /// `/metrics` unauthenticated.
    pub token: Option<String>,
    /// Which metrics to collect. Every metric is opt-in: a category is
    /// enabled by the presence of its table (`[metrics.cpu]`), even one with
    /// no options of its own.
    pub metrics: MetricsConfig,
}

/// One optional table per metric category. Field names must match
/// [`silph_core::Metric::category`]; `all_categories_are_configurable` checks
/// that they cover the registry.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    pub cpu: Option<CpuConfig>,
    pub memory: Option<MemoryConfig>,
    pub disk: Option<DiskConfig>,
    pub temperature: Option<TemperatureConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuConfig {}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskConfig {
    /// Explicit mount points to report. Omit to auto-detect local filesystems.
    pub mounts: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemperatureConfig {
    /// Explicit sensors to report, by instance name (`<chip>/<label>`, e.g.
    /// `k10temp/Tctl`). Omit to report every readable hwmon sensor.
    pub sensors: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum ConfigError {
    Parse(toml::de::Error),
    NoMetrics,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Parse(e) => e.fmt(f),
            ConfigError::NoMetrics => {
                let names: Vec<&str> = METRICS.iter().map(|m| m.category()).collect();
                write!(
                    f,
                    "no metrics enabled: add a [metrics.<name>] table for each metric to \
                     collect (available: {})",
                    names.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl MetricsConfig {
    /// Names of the enabled categories.
    pub fn enabled(&self) -> BTreeSet<String> {
        [
            ("cpu", self.cpu.is_some()),
            ("memory", self.memory.is_some()),
            ("disk", self.disk.is_some()),
            ("temperature", self.temperature.is_some()),
        ]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| name.to_string())
        .collect()
    }
}

impl Config {
    /// Parses and validates a TOML config. Beyond the shape checks serde
    /// does, at least one metric must be enabled.
    pub fn from_toml(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text).map_err(ConfigError::Parse)?;
        if config.metrics.enabled().is_empty() {
            return Err(ConfigError::NoMetrics);
        }
        Ok(config)
    }

    pub fn collect_config(&self) -> CollectConfig {
        CollectConfig {
            enabled: self.metrics.enabled(),
            disk_mounts: self.metrics.disk.as_ref().and_then(|d| d.mounts.clone()),
            temperature_sensors: self
                .metrics
                .temperature
                .as_ref()
                .and_then(|t| t.sensors.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_listen_is_a_clear_error() {
        let err = Config::from_toml("token = \"t\"\n[metrics.cpu]\n").unwrap_err();
        assert!(
            err.to_string().contains("listen"),
            "error should name the missing field: {err}"
        );
    }

    #[test]
    fn missing_metrics_table_is_a_clear_error() {
        let err = Config::from_toml(r#"listen = "127.0.0.1:9100""#).unwrap_err();
        assert!(
            err.to_string().contains("metrics"),
            "error should name the missing field: {err}"
        );
    }

    #[test]
    fn empty_metrics_table_is_rejected() {
        let err = Config::from_toml("listen = \"127.0.0.1:9100\"\n[metrics]\n").unwrap_err();
        assert!(matches!(err, ConfigError::NoMetrics));
        assert!(err.to_string().contains("temperature"), "{err}");
    }

    #[test]
    fn unknown_metric_is_rejected() {
        let err = Config::from_toml("listen = \"127.0.0.1:9100\"\n[metrics.gpu]\n").unwrap_err();
        assert!(err.to_string().contains("gpu"), "{err}");
    }

    #[test]
    fn only_listed_metrics_are_enabled() {
        let config = Config::from_toml(
            r#"
listen = "127.0.0.1:9100"
[metrics.memory]
[metrics.disk]
mounts = ["/"]
[metrics.temperature]
sensors = ["k10temp/Tctl"]
"#,
        )
        .unwrap();
        let collect = config.collect_config();
        assert_eq!(
            collect.enabled,
            BTreeSet::from([
                "memory".to_string(),
                "disk".to_string(),
                "temperature".to_string()
            ])
        );
        assert!(!collect.is_enabled(&silph_core::metrics::cpu::Cpu));
        assert!(collect.is_enabled(&silph_core::metrics::disk::Disk));
        assert_eq!(collect.disk_mounts.as_deref(), Some(&["/".to_string()][..]));
        assert_eq!(
            collect.temperature_sensors.as_deref(),
            Some(&["k10temp/Tctl".to_string()][..])
        );
    }

    /// Adding a metric to `silph_core::METRICS` must come with a field here,
    /// or it could never be enabled.
    #[test]
    fn all_categories_are_configurable() {
        let mut text = String::from("listen = \"127.0.0.1:9100\"\n");
        for metric in METRICS {
            text.push_str(&format!("[metrics.{}]\n", metric.category()));
        }
        let config = Config::from_toml(&text).unwrap();
        let expected: BTreeSet<String> = METRICS.iter().map(|m| m.category().to_string()).collect();
        assert_eq!(config.metrics.enabled(), expected);
    }

    #[test]
    fn example_config_parses_with_every_metric_enabled() {
        let config = Config::from_toml(include_str!("../../../examples/collector.toml")).unwrap();
        assert!(config.listen.parse::<std::net::SocketAddr>().is_ok());
        assert_eq!(config.metrics.enabled().len(), METRICS.len());
    }
}
