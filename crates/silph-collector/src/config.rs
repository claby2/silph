use serde::Deserialize;
use silph_core::CollectConfig;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen")]
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

fn default_listen() -> String {
    "0.0.0.0:9100".to_string()
}

impl Config {
    pub fn collect_config(&self) -> CollectConfig {
        CollectConfig {
            disk_mounts: self.disk.mounts.clone(),
        }
    }
}
