//! hadal-brokerd — the HadalOS capability broker.
//!
//! Runs privileged, contains no model code, and is the only path between a
//! generation and anything that changes this system. See ARCHITECTURE.md §2.

mod action;
mod broker;
mod capability;
mod executor;
mod model;
mod policy;
mod session;
mod token;

use std::sync::Arc;

use zbus::connection;

use crate::broker::Broker;
use crate::executor::Executor;
use crate::model::ModelClient;
use crate::policy::Policy;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("HADAL_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        // systemd captures stderr into the journal, and the unit sets
        // LogLevelMax. No syslog plumbing needed.
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let conn = connection::Builder::system()?.build().await?;

    let policy = Policy::new(&conn).await?;

    // Fail closed. A broker running without its policy file would deny every
    // capability at the polkit layer anyway; refusing to start turns a
    // baffling silent failure into one legible line at the point of install.
    if let Err(e) = policy.verify_actions_installed().await {
        tracing::error!("{e}");
        return Err(e.into());
    }
    tracing::info!("polkit policy verified: {} capabilities", capability::Capability::ALL.len());

    let endpoint = std::env::var("HADAL_ENDPOINT").ok();
    let model = Arc::new(ModelClient::new(endpoint));
    if model.ready().await {
        tracing::info!("hadald is reachable");
    } else {
        // Not fatal: the socket-activated model host may simply not have been
        // hit yet, and sessions are useful for capability discovery regardless.
        tracing::warn!("hadald is not reachable yet; sessions will fail until it is");
    }

    let executor = Arc::new(Executor::new(conn.clone()));
    let broker = Broker::new(model, Arc::new(policy), executor);

    let _conn = connection::Builder::system()?
        .name(broker::NAME)?
        .serve_at(broker::PATH, broker)?
        .build()
        .await?;

    tracing::info!("{} listening at {}", broker::NAME, broker::PATH);

    // systemd sends SIGTERM on stop; without handling it the unit is killed
    // mid-execution rather than declining new work.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("SIGTERM received, shutting down"),
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupted, shutting down"),
    }

    Ok(())
}
