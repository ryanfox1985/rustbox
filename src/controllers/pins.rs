use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use serde::{Deserialize, Deserializer};
use sqlx::SqlitePool;

use crate::{bridge::Bridge, gpio::Gpio, models::Pin, routes::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/pins", get(index).post(create))
        .route(
            "/pins/{id}",
            get(show).patch(update).put(update).delete(destroy),
        )
}

// Accepts both `{"pin": {...}}` and bare `{...}`, like Rails' wrap_parameters.
#[derive(Deserialize)]
#[serde(untagged)]
enum PinBody {
    Wrapped { pin: PinParams },
    Bare(PinParams),
}

// Outer `None` means the field was absent; `Some(None)` means it was explicitly null.
#[derive(Default, Deserialize)]
pub struct PinParams {
    #[serde(default, deserialize_with = "nullable")]
    name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pin_pi: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    value: Option<Option<bool>>,
}

impl PinParams {
    pub fn name(name: String) -> Self {
        Self {
            name: Some(Some(name)),
            ..Self::default()
        }
    }

    pub fn value(value: bool) -> Self {
        Self {
            value: Some(Some(value)),
            ..Self::default()
        }
    }
}

impl From<PinBody> for PinParams {
    fn from(body: PinBody) -> Self {
        match body {
            PinBody::Wrapped { pin } | PinBody::Bare(pin) => pin,
        }
    }
}

fn nullable<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::deserialize(de).map(Some)
}

async fn index(State(db): State<SqlitePool>) -> Result<Json<Vec<Pin>>, StatusCode> {
    Pin::all(&db).await.map(Json).map_err(internal)
}

async fn show(
    State(db): State<SqlitePool>,
    Path(id): Path<i64>,
) -> Result<Json<Pin>, StatusCode> {
    find(&db, id).await.map(Json)
}

async fn create(
    State(db): State<SqlitePool>,
    State(gpio): State<Arc<Gpio>>,
    State(bridge): State<Arc<Bridge>>,
    Json(body): Json<PinBody>,
) -> Result<impl IntoResponse, StatusCode> {
    let params = PinParams::from(body);
    let pin_pi = params.pin_pi.flatten();
    let value = params.value.unwrap_or(Some(false));
    drive(&gpio, pin_pi, value)?;

    let pin = Pin::create(&db, params.name.flatten(), pin_pi, value)
        .await
        .map_err(internal)?;
    bridge.publish(&pin);

    let location = format!("/pins/{}", pin.id);
    Ok((StatusCode::CREATED, [(header::LOCATION, location)], Json(pin)))
}

async fn update(
    State(db): State<SqlitePool>,
    State(gpio): State<Arc<Gpio>>,
    State(bridge): State<Arc<Bridge>>,
    Path(id): Path<i64>,
    Json(body): Json<PinBody>,
) -> Result<Json<Pin>, StatusCode> {
    save(&db, &gpio, &bridge, id, body.into()).await.map(Json)
}

// Applies `params` to an existing pin. Shared by the HTTP API and Matter.
pub async fn save(
    db: &SqlitePool,
    gpio: &Gpio,
    bridge: &Bridge,
    id: i64,
    params: PinParams,
) -> Result<Pin, StatusCode> {
    let pin = find(db, id).await?;
    let pin_pi = params.pin_pi.unwrap_or(pin.pin_pi);
    let value = params.value.unwrap_or(pin.value);
    drive(gpio, pin_pi, value)?;

    let pin = Pin::update(db, id, params.name.unwrap_or(pin.name), pin_pi, value)
        .await
        .map_err(internal)?
        .ok_or(StatusCode::NOT_FOUND)?;
    bridge.publish(&pin);
    Ok(pin)
}

async fn destroy(
    State(db): State<SqlitePool>,
    State(bridge): State<Arc<Bridge>>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if Pin::delete(&db, id).await.map_err(internal)? {
        bridge.remove(id);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub(super) async fn find(db: &SqlitePool, id: i64) -> Result<Pin, StatusCode> {
    Pin::find(db, id)
        .await
        .map_err(internal)?
        .ok_or(StatusCode::NOT_FOUND)
}

// Re-applies every saved pin, since GPIO levels are lost on reboot, and hands
// them to the bridge. When two pins share a `pin_pi`, the highest id wins.
pub async fn restore(db: &SqlitePool, gpio: &Gpio, bridge: &Bridge) -> Result<(), sqlx::Error> {
    for pin in Pin::all(db).await? {
        if let Err(status) = drive(gpio, pin.pin_pi, pin.value) {
            tracing::warn!("couldn't restore pin {}: {status}", pin.id);
        }
        bridge.publish(&pin);
    }
    Ok(())
}

// Drives `pin_pi` high when `value` is true, low otherwise. Pins without a
// `pin_pi` are left alone.
pub(super) fn drive(
    gpio: &Gpio,
    pin_pi: Option<i64>,
    value: Option<bool>,
) -> Result<(), StatusCode> {
    let Some(pin_pi) = pin_pi else {
        return Ok(());
    };
    let pin = gpio.header_pin(pin_pi).ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
    gpio.write(pin, value.unwrap_or(false)).map_err(|err| {
        tracing::error!("GPIO error: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

pub(super) fn internal(err: sqlx::Error) -> StatusCode {
    tracing::error!("database error: {err}");
    StatusCode::INTERNAL_SERVER_ERROR
}
