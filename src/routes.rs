use std::sync::Arc;

use axum::{Json, Router, extract::FromRef, routing::get};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{
    bridge::Bridge,
    controllers::{pins, ui},
    gpio::Gpio,
};

#[derive(Clone, FromRef)]
pub struct AppState {
    pub db: SqlitePool,
    pub gpio: Arc<Gpio>,
    pub bridge: Arc<Bridge>,
}

// JSON API, served on `PORT`.
pub fn api_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(pins::router())
        .with_state(state)
}

// HTML pages, served on `UI_PORT`.
pub fn ui_router(state: AppState) -> Router {
    ui::router().with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}
