use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: String,
    /// Directory for the time-series database.
    pub data_dir: PathBuf,
    #[serde(with = "humantime_serde", default = "default_scrape_interval")]
    pub scrape_interval: Duration,
    #[serde(with = "humantime_serde", default = "default_scrape_timeout")]
    pub scrape_timeout: Duration,
    #[serde(with = "humantime_serde", default = "default_retention")]
    pub retention: Duration,
    #[serde(default)]
    pub targets: Vec<Target>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Display name; becomes the `host` label on stored series.
    pub name: String,
    /// Collector base URL, e.g. `http://10.0.0.2:9100`.
    pub url: String,
    pub token: Option<String>,
}

fn default_scrape_interval() -> Duration {
    Duration::from_secs(15)
}

fn default_scrape_timeout() -> Duration {
    Duration::from_secs(5)
}

fn default_retention() -> Duration {
    Duration::from_hours(30 * 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let config: Config = toml::from_str(
            r#"
                listen = "0.0.0.0:8080"
                data_dir = "/var/lib/silph"
                scrape_interval = "30s"
                retention = "7d"

                [[targets]]
                name = "web-1"
                url = "http://10.0.0.2:9100"
                token = "secret"
            "#,
        )
        .unwrap();
        assert_eq!(config.scrape_interval, Duration::from_secs(30));
        assert_eq!(config.scrape_timeout, Duration::from_secs(5)); // default
        assert_eq!(config.retention, Duration::from_secs(7 * 24 * 3600));
        assert_eq!(config.targets.len(), 1);
        assert_eq!(config.targets[0].name, "web-1");
    }

    #[test]
    fn missing_listen_is_a_clear_error() {
        let err = toml::from_str::<Config>(r#"data_dir = "/var/lib/silph""#).unwrap_err();
        assert!(
            err.to_string().contains("listen"),
            "error should name the missing field: {err}"
        );
    }

    #[test]
    fn example_config_parses_and_binds_loopback() {
        let config: Config = toml::from_str(include_str!("../../../examples/server.toml")).unwrap();
        // The query API and dashboard have no authentication, so the
        // example everyone copies must not expose them to the network.
        let addr: std::net::SocketAddr = config.listen.parse().unwrap();
        assert!(
            addr.ip().is_loopback(),
            "examples/server.toml must bind loopback, got {addr}"
        );
    }
}
