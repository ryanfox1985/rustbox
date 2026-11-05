// State shared between the HTTP API and the Matter stack, which runs on its
// own thread. The HTTP side publishes every pin it saves or deletes; the
// Matter side reads that state and sends commands back, which go through the
// same code path as `PATCH /pins/{id}`.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use axum::http::StatusCode;
use tokio::sync::{Notify, mpsc, oneshot};

use crate::{
    controllers::pins::{self, PinParams},
    models::Pin,
    routes::AppState,
};

#[derive(Clone, PartialEq)]
pub struct BridgedPin {
    pub name: Option<String>,
    pub on: bool,
}

pub enum Change {
    On(bool),
    Name(String),
}

struct Command {
    id: i64,
    change: Change,
    reply: oneshot::Sender<Result<(), StatusCode>>,
}

pub struct Bridge {
    pins: Mutex<BTreeMap<i64, BridgedPin>>,
    changed: Notify,
    commands: mpsc::UnboundedSender<Command>,
}

pub struct Commands(mpsc::UnboundedReceiver<Command>);

impl Bridge {
    pub fn new() -> (Arc<Self>, Commands) {
        let (commands, rx) = mpsc::unbounded_channel();
        let bridge = Self {
            pins: Mutex::default(),
            changed: Notify::new(),
            commands,
        };
        (Arc::new(bridge), Commands(rx))
    }

    pub fn publish(&self, pin: &Pin) {
        let bridged = BridgedPin {
            name: pin.name.clone(),
            on: pin.value.unwrap_or(false),
        };
        self.pins.lock().unwrap().insert(pin.id, bridged);
        self.changed.notify_one();
    }

    pub fn remove(&self, id: i64) {
        self.pins.lock().unwrap().remove(&id);
        self.changed.notify_one();
    }

    pub fn get(&self, id: i64) -> Option<BridgedPin> {
        self.pins.lock().unwrap().get(&id).cloned()
    }

    pub fn snapshot(&self) -> BTreeMap<i64, BridgedPin> {
        self.pins.lock().unwrap().clone()
    }

    // Resolves after the next `publish` or `remove`, or immediately if one
    // happened since the last call.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    // Applies a change from Matter as if it came from the HTTP API.
    pub async fn send(&self, id: i64, change: Change) -> Result<(), StatusCode> {
        let (reply, rx) = oneshot::channel();
        let command = Command { id, change, reply };
        self.commands
            .send(command)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        rx.await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    }
}

impl Commands {
    pub async fn serve(mut self, state: AppState) {
        while let Some(Command { id, change, reply }) = self.0.recv().await {
            let params = match change {
                Change::On(on) => PinParams::value(on),
                Change::Name(name) => PinParams::name(name),
            };
            let result = pins::save(&state.db, &state.gpio, &state.bridge, id, params).await;
            let _ = reply.send(result.map(|_| ()));
        }
    }
}
