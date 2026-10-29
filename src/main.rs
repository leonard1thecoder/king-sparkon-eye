use std::net::SocketAddr;

use king_sparkon_eye::{api, config::Config, observability};
use tokio::signal;

#[tokio::main]
async fn main() {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(err) => {
            // Errors name the variable but never echo values.
            eprintln!("configuration error: {err}");
            std::process::exit(2);
        }
    };

    observability::init_tracing(&config.log_filter);
    let bind_addr = config.bind_addr;
    let app = api::router(config);

    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(error = %err, %bind_addr, "failed to bind listener");
            std::process::exit(1);
        }
    };
    tracing::info!(%bind_addr, "king sparkon eye listening");

    if let Err(err) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    {
        tracing::error!(error = %err, "server error");
        std::process::exit(1);
    }
    tracing::info!("shutdown complete");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sigterm) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            sigterm.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
