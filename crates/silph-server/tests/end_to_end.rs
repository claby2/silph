//! Scrapes a real in-process collector into a real on-disk store, twice, and
//! checks the query API returns processed CPU data.

use std::sync::Arc;
use std::time::Duration;

use silph_core::{CollectConfig, METRICS};
use silph_server::api::AppState;
use silph_server::config::Target;
use silph_server::scrape::{self, Hosts};
use silph_server::storage::Store;

const TOKEN: &str = "test-token";

async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test(flavor = "multi_thread")]
async fn scrape_store_query() {
    let all_metrics = CollectConfig {
        enabled: METRICS.iter().map(|m| m.category().to_string()).collect(),
        ..Default::default()
    };
    let collector_url = serve(silph_collector::router(Some(TOKEN), all_metrics)).await;

    let data_dir = tempfile::tempdir().unwrap();
    let store = Store::open(data_dir.path(), Duration::from_secs(3600)).unwrap();
    let hosts: Hosts = Arc::new(std::sync::RwLock::new(Default::default()));
    let client = reqwest::Client::new();
    let target = Target {
        name: "local".to_string(),
        url: collector_url.clone(),
        token: Some(TOKEN.to_string()),
    };

    // Unauthenticated and wrong-token scrapes are rejected.
    async fn metrics_status(
        client: &reqwest::Client,
        base: &str,
        token: Option<&str>,
    ) -> reqwest::StatusCode {
        let mut req = client.get(format!("{base}/metrics"));
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        req.send().await.unwrap().status()
    }
    assert_eq!(metrics_status(&client, &collector_url, None).await, 401);
    assert_eq!(
        metrics_status(&client, &collector_url, Some("wrong")).await,
        401
    );
    assert_eq!(
        metrics_status(&client, &collector_url, Some(TOKEN)).await,
        200
    );

    // Metrics are opt-in: a collector with only memory enabled reports only
    // memory keys.
    let memory_only = CollectConfig {
        enabled: ["memory".to_string()].into(),
        ..Default::default()
    };
    let memory_only_url = serve(silph_collector::router(None, memory_only)).await;
    let body: serde_json::Value = client
        .get(format!("{memory_only_url}/metrics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert!(!keys.is_empty());
    assert!(
        keys.iter().all(|k| k.starts_with("memory_")),
        "unexpected keys: {keys:?}"
    );

    // First scrape stores gauges only; the second adds CPU (needs a delta).
    let first = scrape::scrape_once(&target, &hosts, &store, &client)
        .await
        .unwrap();
    assert!(first > 0, "expected gauge points from first scrape");
    tokio::time::sleep(Duration::from_millis(150)).await;
    let second = scrape::scrape_once(&target, &hosts, &store, &client)
        .await
        .unwrap();
    assert!(second > first, "second scrape should add cpu_usage_percent");

    let api_url = serve(silph_server::router(AppState {
        hosts: hosts.clone(),
        store: store.clone(),
        scrape_interval: Duration::from_secs(15),
    }))
    .await;

    let config: serde_json::Value = client
        .get(format!("{api_url}/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(config["scrape_interval_ms"], 15_000);

    let hosts_json: serde_json::Value = client
        .get(format!("{api_url}/api/hosts"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hosts_json[0]["name"], "local");
    assert_eq!(hosts_json[0]["up"], true);

    let now = scrape::now_ms();
    let query: serde_json::Value = client
        .get(format!(
            "{api_url}/api/query?host=local&metric=cpu_usage_percent&start={}&end={}&step=1000",
            now - 60_000,
            now + 1_000,
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let series = query["series"].as_array().unwrap();
    assert_eq!(series.len(), 1);
    let cpu_values: Vec<f64> = series[0]["values"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_f64())
        .collect();
    assert!(!cpu_values.is_empty(), "expected a stored cpu sample");
    assert!(cpu_values.iter().all(|v| (0.0..=100.0).contains(v)));

    // Instanced metric: disk series carry mount-point instances.
    let disk: serde_json::Value = client
        .get(format!(
            "{api_url}/api/query?host=local&metric=disk_used_percent&start={}&end={}&step=1000",
            now - 60_000,
            now + 1_000,
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let disk_series = disk["series"].as_array().unwrap();
    assert!(!disk_series.is_empty(), "expected at least one mount");
    assert!(disk_series[0]["instance"].is_string());

    // One request covers several hosts and several metrics at once: that is
    // how the dashboard draws a whole panel for a multi-host selection.
    let second_target = Target {
        name: "local-2".to_string(),
        ..target.clone()
    };
    scrape::scrape_once(&second_target, &hosts, &store, &client)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    scrape::scrape_once(&second_target, &hosts, &store, &client)
        .await
        .unwrap();

    let combined: serde_json::Value = client
        .get(format!(
            "{api_url}/api/query?host=local,local-2&metric=cpu_usage_percent,memory_used\
             &start={}&end={}&step=1000",
            now - 60_000,
            now + 60_000,
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let combined_series = combined["series"].as_array().unwrap();
    let labels: Vec<(&str, &str)> = combined_series
        .iter()
        .map(|s| (s["host"].as_str().unwrap(), s["metric"].as_str().unwrap()))
        .collect();
    for host in ["local", "local-2"] {
        for metric in ["cpu_usage_percent", "memory_used"] {
            assert!(
                labels.contains(&(host, metric)),
                "missing {host}/{metric} in {labels:?}"
            );
        }
    }

    // A window with no samples in it yields no series at all, rather than a
    // column of nulls per host.
    let absent: serde_json::Value = client
        .get(format!(
            "{api_url}/api/query?host=local,local-2&metric=cpu_usage_percent\
             &start=0&end=60000&step=1000"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(absent["series"].as_array().unwrap().is_empty());

    // Unknown metric and host are 404s.
    for bad in [
        format!("{api_url}/api/query?host=local&metric=nope&start=0&end=1&step=1"),
        format!("{api_url}/api/query?host=nope&metric=cpu_usage_percent&start=0&end=1&step=1"),
    ] {
        assert_eq!(client.get(bad).send().await.unwrap().status(), 404);
    }

    store.close().unwrap();
}
