mod bridge;
mod controllers;
mod gpio;
mod matter;
mod models;
mod routes;

use std::{net::SocketAddr, sync::Arc};

use axum::Router;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use tokio::{net::TcpListener, signal, sync::watch};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let api_addr = SocketAddr::from(([0, 0, 0, 0], env_port("PORT", 3000)));
    let ui_addr = SocketAddr::from(([0, 0, 0, 0], env_port("UI_PORT", 8080)));

    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:rustbox.db".into());
    let options = database_url
        .parse::<SqliteConnectOptions>()
        .expect("invalid DATABASE_URL")
        .create_if_missing(true);
    let db = SqlitePool::connect_with(options)
        .await
        .expect("failed to connect to database");
    sqlx::migrate!().run(&db).await.expect("failed to run migrations");

    let gpio = Arc::new(gpio::Gpio::new());
    let (bridge, commands) = bridge::Bridge::new();
    controllers::pins::restore(&db, &gpio, &bridge)
        .await
        .expect("failed to restore pin states");

    let state = routes::AppState { db, gpio, bridge };
    tokio::spawn(commands.serve(state.clone()));
    matter::spawn(state.bridge.clone());
    let api = routes::api_router(state.clone()).layer(TraceLayer::new_for_http());
    let ui = routes::ui_router(state).layer(TraceLayer::new_for_http());

    let api_listener = TcpListener::bind(api_addr)
        .await
        .expect("failed to bind API address");
    let ui_listener = TcpListener::bind(ui_addr)
        .await
        .expect("failed to bind UI address");
    tracing::info!("API listening on {api_addr}, UI on {ui_addr}");

    // Both servers stop together: dropping `stop` wakes every receiver.
    let (stop, stopped) = watch::channel(());
    tokio::spawn(async move {
        shutdown_signal().await;
        drop(stop);
    });

    tokio::try_join!(
        serve(api_listener, api, stopped.clone()),
        serve(ui_listener, ui, stopped),
    )
    .expect("server error");
}

fn env_port(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(default)
}

async fn serve(
    listener: TcpListener,
    app: Router,
    mut stopped: watch::Receiver<()>,
) -> std::io::Result<()> {
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Nothing is ever sent, so this returns once the sender is dropped.
            let _ = stopped.changed().await;
        })
        .await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutting down");
}
