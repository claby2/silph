use std::process::ExitCode;

use silph_collector::config::Config;

const USAGE: &str = "usage: silph-collector --config <path>";

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
    let app = silph_collector::router(&config.token, config.collect_config());

    // Current-thread runtime: the collector serves one client at a trickle.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(&config.listen).await?;
        tracing::info!(listen = %config.listen, "silph-collector listening");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        Ok(())
    })
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
                println!("silph-collector {}", env!("CARGO_PKG_VERSION"));
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
