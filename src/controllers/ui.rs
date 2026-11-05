// Server-rendered HTML pages for managing pins in a browser, served on their
// own port (`UI_PORT`). HTML forms can only GET and POST, so updates and
// deletes are POSTs to their own paths.

use std::sync::Arc;

use askama::Template;
use axum::{
    Form, Router,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use sqlx::SqlitePool;

use super::pins::{self, PinParams, drive, find, internal};
use crate::{bridge::Bridge, gpio::Gpio, models::Pin, routes::AppState};

const INDEX: &str = "/pins";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(|| async { Redirect::to(INDEX) }))
        .route("/pins", get(index).post(create))
        .route("/pins/new", get(new))
        .route("/pins/{id}", get(edit).post(update))
        .route("/pins/{id}/toggle", post(toggle))
        .route("/pins/{id}/delete", post(destroy))
        .route("/style.css", get(stylesheet))
        .route_layer(middleware::from_fn(same_origin))
}

#[derive(Template)]
#[template(path = "pins/index.html")]
struct IndexPage {
    pins: Vec<Pin>,
}

#[derive(Template)]
#[template(path = "pins/form.html")]
struct FormPage {
    title: &'static str,
    action: String,
    name: String,
    gpio_options: Vec<GpioOption>,
    value: bool,
    error: Option<String>,
}

struct GpioOption {
    number: String,
    selected: bool,
}

impl FormPage {
    // `id` is `None` for a new pin. `pin_pi` is the selected GPIO, or empty.
    fn new(gpio: &Gpio, id: Option<i64>, name: &str, pin_pi: &str, value: bool) -> Self {
        let mut numbers: Vec<String> = gpio.header().iter().map(u8::to_string).collect();
        // Keep a saved GPIO that isn't on this board selectable, so the form
        // doesn't silently change it.
        if !pin_pi.is_empty() && !numbers.iter().any(|n| n == pin_pi) {
            numbers.push(pin_pi.to_owned());
        }
        Self {
            title: if id.is_some() { "Edit pin" } else { "New pin" },
            action: id.map_or_else(|| INDEX.to_owned(), |id| format!("{INDEX}/{id}")),
            name: name.to_owned(),
            gpio_options: numbers
                .into_iter()
                .map(|number| GpioOption {
                    selected: number == pin_pi,
                    number,
                })
                .collect(),
            value,
            error: None,
        }
    }

    fn from_pin(gpio: &Gpio, pin: &Pin) -> Self {
        let pin_pi = pin.pin_pi.map(|n| n.to_string()).unwrap_or_default();
        let name = pin.name.as_deref().unwrap_or_default();
        Self::new(gpio, Some(pin.id), name, &pin_pi, pin.is_on())
    }

    // Shows the submitted values again with an error message.
    fn invalid(gpio: &Gpio, id: Option<i64>, form: &PinForm, error: String) -> Self {
        let value = form.value.is_some();
        Self {
            error: Some(error),
            ..Self::new(gpio, id, &form.name, form.pin_pi.trim(), value)
        }
    }
}

#[derive(Deserialize)]
struct PinForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    pin_pi: String,
    // A checkbox, only sent when ticked.
    value: Option<String>,
}

struct Fields {
    name: Option<String>,
    pin_pi: Option<i64>,
    value: Option<bool>,
}

impl PinForm {
    // Returns a message for the form when the GPIO isn't on the header.
    fn parse(&self, gpio: &Gpio) -> Result<Fields, String> {
        let pin_pi = match self.pin_pi.trim() {
            "" => None,
            n => Some(
                n.parse()
                    .ok()
                    .filter(|&n| gpio.header_pin(n).is_some())
                    .ok_or_else(|| format!("GPIO {n} isn't on this board's header."))?,
            ),
        };
        let name = Some(self.name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        Ok(Fields {
            name,
            pin_pi,
            value: Some(self.value.is_some()),
        })
    }
}

async fn index(State(db): State<SqlitePool>) -> Result<Response, StatusCode> {
    let pins = Pin::all(&db).await.map_err(internal)?;
    render(IndexPage { pins }, StatusCode::OK)
}

async fn new(State(gpio): State<Arc<Gpio>>) -> Result<Response, StatusCode> {
    render(FormPage::new(&gpio, None, "", "", false), StatusCode::OK)
}

async fn create(
    State(db): State<SqlitePool>,
    State(gpio): State<Arc<Gpio>>,
    State(bridge): State<Arc<Bridge>>,
    Form(form): Form<PinForm>,
) -> Result<Response, StatusCode> {
    let fields = match form.parse(&gpio) {
        Ok(fields) => fields,
        Err(error) => {
            let page = FormPage::invalid(&gpio, None, &form, error);
            return render(page, StatusCode::UNPROCESSABLE_ENTITY);
        }
    };
    drive(&gpio, fields.pin_pi, fields.value)?;
    let pin = Pin::create(&db, fields.name, fields.pin_pi, fields.value)
        .await
        .map_err(internal)?;
    bridge.publish(&pin);
    Ok(Redirect::to(INDEX).into_response())
}

async fn edit(
    State(db): State<SqlitePool>,
    State(gpio): State<Arc<Gpio>>,
    Path(id): Path<i64>,
) -> Result<Response, StatusCode> {
    let pin = find(&db, id).await?;
    render(FormPage::from_pin(&gpio, &pin), StatusCode::OK)
}

async fn update(
    State(db): State<SqlitePool>,
    State(gpio): State<Arc<Gpio>>,
    State(bridge): State<Arc<Bridge>>,
    Path(id): Path<i64>,
    Form(form): Form<PinForm>,
) -> Result<Response, StatusCode> {
    find(&db, id).await?;
    let fields = match form.parse(&gpio) {
        Ok(fields) => fields,
        Err(error) => {
            let page = FormPage::invalid(&gpio, Some(id), &form, error);
            return render(page, StatusCode::UNPROCESSABLE_ENTITY);
        }
    };
    drive(&gpio, fields.pin_pi, fields.value)?;
    let pin = Pin::update(&db, id, fields.name, fields.pin_pi, fields.value)
        .await
        .map_err(internal)?
        .ok_or(StatusCode::NOT_FOUND)?;
    bridge.publish(&pin);
    Ok(Redirect::to(INDEX).into_response())
}

async fn toggle(
    State(db): State<SqlitePool>,
    State(gpio): State<Arc<Gpio>>,
    State(bridge): State<Arc<Bridge>>,
    Path(id): Path<i64>,
) -> Result<Redirect, StatusCode> {
    let pin = find(&db, id).await?;
    let params = PinParams::value(!pin.is_on());
    pins::save(&db, &gpio, &bridge, id, params).await?;
    Ok(Redirect::to(INDEX))
}

async fn destroy(
    State(db): State<SqlitePool>,
    State(bridge): State<Arc<Bridge>>,
    Path(id): Path<i64>,
) -> Result<Redirect, StatusCode> {
    if Pin::delete(&db, id).await.map_err(internal)? {
        bridge.remove(id);
        Ok(Redirect::to(INDEX))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../../assets/style.css"),
    )
}

fn render(page: impl Template, status: StatusCode) -> Result<Response, StatusCode> {
    let html = page.render().map_err(|err| {
        tracing::error!("template error: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok((status, Html(html)).into_response())
}

// Blocks cross-site form posts (CSRF): otherwise any web page open in a
// browser on the network could switch pins. Requests carrying neither header,
// like curl, aren't from a browser and pass.
async fn same_origin(request: Request, next: Next) -> Response {
    if request.method().is_safe() || is_same_origin(request.headers()) {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

fn is_same_origin(headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site") {
        return site == "same-origin" || site == "none";
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let origin = origin
        .to_str()
        .ok()
        .and_then(|origin| origin.split_once("://"))
        .map(|(_, host)| host);
    let host = headers.get(header::HOST).and_then(|host| host.to_str().ok());
    origin.is_some() && origin == host
}
