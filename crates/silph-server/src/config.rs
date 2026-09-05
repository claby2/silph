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

impl Config {
    /// Checks what the type system can't: target names must be unique (two
    /// targets sharing a name would interleave into one series) and free of
    /// the comma the query API uses to separate host names.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for target in &self.targets {
            let name = target.name.as_str();
            if name.trim().is_empty() {
                return Err("target name must not be empty".to_string());
            }
            if name.contains(',') {
                return Err(format!("target name must not contain a comma: {name:?}"));
            }
            if seen.contains(&name) {
                return Err(format!("duplicate target name: {name:?}"));
            }
            seen.push(name);
        }
        Ok(())
    }
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

    fn config_with_target_names(names: &[&str]) -> Config {
        let targets: String = names
            .iter()
            .map(|name| format!("[[targets]]\nname = {name:?}\nurl = \"http://x\"\n"))
            .collect();
        toml::from_str(&format!(
            "listen = \"127.0.0.1:8080\"\ndata_dir = \"/tmp/silph\"\n{targets}"
        ))
        .unwrap()
    }

    #[test]
    fn validate_rejects_unusable_target_names() {
        config_with_target_names(&["web-1", "web-2"])
            .validate()
            .unwrap();
        // The query API splits its `host` parameter on commas.
        let err = config_with_target_names(&["web,1"]).validate().unwrap_err();
        assert!(err.contains("comma"), "{err}");
        let err = config_with_target_names(&["web", "web"])
            .validate()
            .unwrap_err();
        assert!(err.contains("duplicate"), "{err}");
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
