use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use silph_server::api::AppState;
use silph_server::config::Config;
use silph_server::scrape;
use silph_server::storage::Store;

#[derive(Parser)]
#[command(version, about = "silph monitoring server")]
struct Args {
    /// Path to the TOML config file.
    #[arg(short, long)]
    config: String,
    /// Validate the config file and exit without starting the daemon.
    #[arg(long)]
    check_config: bool,
}

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
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config: Config = toml::from_str(&std::fs::read_to_string(&args.config)?)?;
    if args.check_config {
        return Ok(());
    }
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
        .with_graceful_shutdown(silph_core::shutdown_signal())
        .await?;
    Ok(())
}
