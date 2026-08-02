//! Scrapes a real in-process collector into a real tsink store, twice, and
//! checks the query API returns processed CPU data.

use std::sync::Arc;
use std::time::Duration;

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
    let collector_url = serve(silph_collector::router(TOKEN, Default::default())).await;

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
        hosts,
        store: store.clone(),
        scrape_interval: Duration::from_secs(15),
    }))
    .await;

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

    // Unknown metric and host are 404s.
    for bad in [
        format!("{api_url}/api/query?host=local&metric=nope&start=0&end=1&step=1"),
        format!("{api_url}/api/query?host=nope&metric=cpu_usage_percent&start=0&end=1&step=1"),
    ] {
        assert_eq!(client.get(bad).send().await.unwrap().status(), 404);
    }

    store.close().unwrap();
}
