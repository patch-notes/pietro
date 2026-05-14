//! Pietro entry point — CLI + server bootstrap.
//!
//! Two subcommands today (§5):
//!   * `pietro serve   --config pietro.yaml`  → run the HTTP server.
//!   * `pietro migrate --config pietro.yaml`  → run pending DB migrations (M2+).
//!
//! Per the build plan, M1 only wires `serve` through to a minimal axum app
//! exposing `/healthz`. The `migrate` subcommand exists but currently prints a
//! "not yet" notice rather than half-doing something. No half-states shipped.

#![deny(unsafe_op_in_unsafe_fn)]

mod config;
mod errors;
mod routes;
mod secret;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tokio::signal;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::routes::{AppState, build_router};

#[derive(Parser, Debug)]
#[command(name = "pietro", version, about = "Pietro — the keeper of the keys")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the HTTP server.
    Serve {
        /// Path to `pietro.yaml`.
        #[arg(short, long, default_value = "pietro.yaml")]
        config: PathBuf,
    },
    /// Apply pending database migrations and exit.
    Migrate {
        #[arg(short, long, default_value = "pietro.yaml")]
        config: PathBuf,
    },
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    rt.block_on(async {
        match cli.command {
            Cmd::Serve { config } => run_serve(&config).await,
            Cmd::Migrate { config } => run_migrate(&config).await,
        }
    })
}

/// Set up `tracing`. Format is human-readable by default; set
/// `PIETRO_LOG_FORMAT=json` for structured output (§16). `RUST_LOG` controls
/// the filter; default is `info`.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json = std::env::var("PIETRO_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if json {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

async fn run_serve(config_path: &std::path::Path) -> Result<()> {
    let cfg = Config::load(config_path)
        .with_context(|| format!("loading config: {}", config_path.display()))?;
    info!(
        listen = %cfg.listen,
        public_url = %cfg.public_url,
        services = cfg.services.len(),
        "pietro starting"
    );

    let state = AppState {
        config: Arc::new(cfg),
    };
    let listener = TcpListener::bind(&state.config.listen)
        .await
        .with_context(|| format!("binding {}", state.config.listen))?;
    let app = build_router(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum server error")?;
    info!("pietro shut down cleanly");
    Ok(())
}

async fn run_migrate(_config_path: &std::path::Path) -> Result<()> {
    // M2 will implement this. We deliberately do nothing here rather than a
    // half-working stub — "no half-states shipped" (§19).
    eprintln!("`pietro migrate` will be implemented in milestone M2.");
    std::process::exit(2);
}

/// Cooperative shutdown: SIGINT (Ctrl-C) or SIGTERM (k8s, systemd).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        let mut term = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        term.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received Ctrl-C, shutting down"),
        _ = terminate => info!("received SIGTERM, shutting down"),
    }
}
