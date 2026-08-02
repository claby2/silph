use serde::Deserialize;
use silph_core::CollectConfig;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: String,
    /// Bearer token the server must present when scraping. Omit to serve
    /// `/metrics` unauthenticated.
    pub token: Option<String>,
    #[serde(default)]
    pub disk: DiskConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskConfig {
    /// Explicit mount points to report. Omit to auto-detect local filesystems.
    pub mounts: Option<Vec<String>>,
}

impl Config {
    pub fn collect_config(&self) -> CollectConfig {
        CollectConfig {
            disk_mounts: self.disk.mounts.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_listen_is_a_clear_error() {
        let err = toml::from_str::<Config>(r#"token = "t""#).unwrap_err();
        assert!(
            err.to_string().contains("listen"),
            "error should name the missing field: {err}"
        );
    }

    #[test]
    fn example_config_parses() {
        let config: Config =
            toml::from_str(include_str!("../../../examples/collector.toml")).unwrap();
        assert!(config.listen.parse::<std::net::SocketAddr>().is_ok());
    }
}
