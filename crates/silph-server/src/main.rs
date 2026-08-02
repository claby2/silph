use std::process::ExitCode;
use std::sync::Arc;

use silph_server::api::AppState;
use silph_server::config::Config;
use silph_server::scrape;
use silph_server::storage::Store;

const USAGE: &str = "usage: silph-server --config <path>";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("silph-server: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = match parse_args()? {
        Some(path) => path,
        None => return Ok(()), // --help / --version
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config: Config = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    if config.targets.is_empty() {
        tracing::warn!("no [[targets]] configured; nothing will be scraped");
    }

    let store = Store::open(&config.data_dir, config.retention)?;
    let hosts: scrape::Hosts = Arc::new(std::sync::RwLock::new(Default::default()));

    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(serve(&config, store.clone(), hosts));
    // Flush the WAL and stop background work before exiting.
    store.close()?;
    result
}

async fn serve(
    config: &Config,
    store: Store,
    hosts: scrape::Hosts,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .timeout(config.scrape_timeout)
        .build()?;
    for target in config.targets.clone() {
        tokio::spawn(scrape::scrape_loop(
            target,
            hosts.clone(),
            store.clone(),
            client.clone(),
            config.scrape_interval,
        ));
    }

    let app = silph_server::router(AppState {
        hosts,
        store,
        scrape_interval: config.scrape_interval,
    });
    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    tracing::info!(listen = %config.listen, "silph-server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}

fn parse_args() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut config_path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                config_path = Some(args.next().ok_or("--config requires a path")?);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--version" | "-V" => {
                println!("silph-server {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            other => return Err(format!("unknown argument: {other}\n{USAGE}").into()),
        }
    }
    match config_path {
        Some(path) => Ok(Some(path)),
        None => Err(USAGE.into()),
    }
}
