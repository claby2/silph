use std::process::ExitCode;

use clap::Parser;
use silph_collector::config::Config;

#[derive(Parser)]
#[command(version, about = "silph metrics collector daemon")]
struct Args {
    /// Path to the TOML config file.
    #[arg(short, long)]
    config: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("silph-collector: {e}");
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
    let app = silph_collector::router(config.token.as_deref(), config.collect_config());

    // Current-thread runtime: the collector serves one client at a trickle.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(&config.listen).await?;
        tracing::info!(listen = %config.listen, "silph-collector listening");
        axum::serve(listener, app)
            .with_graceful_shutdown(silph_core::shutdown_signal())
            .await?;
        Ok(())
    })
}
