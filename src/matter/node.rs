// The Matter data model: the root endpoint, an aggregator on endpoint 1, and
// one bridged on/off plug-in unit per pin, added and removed as pins come and
// go. Pin ids are never reused, so pin `id` is always endpoint `id + 1`.

use std::collections::BTreeMap;

use rand::Rng;
use rs_matter::{
    clusters,
    dm::{
        AttrChangeNotifier, AttrId, Async, Cluster, ClusterId, DataModel, Dataver,
        DeviceType, DynamicNode, Endpoint, EndptId, InvokeContext, Metadata, Node, ReadContext,
        WriteContext,
        clusters::{
            decl::bridged_device_basic_information::{self as bridged, KeepActiveRequest},
            desc::{self, ClusterHandler as _},
            app::on_off,
        },
        devices::{DEV_TYPE_AGGREGATOR, DEV_TYPE_BRIDGED_NODE},
        endpoints::{self, EthSysHandlerBuilder},
        networks::SysNetifs,
    },
    devices,
    error::{Error, ErrorCode},
    root_endpoint,
    tlv::{TLVBuilderParent, Utf8StrBuilder},
    utils::{cell::RefCell, sync::blocking::Mutex},
    with,
};

use crate::bridge::{Bridge, BridgedPin, Change};

const AGGREGATOR: EndptId = 1;
const MAX_PINS: usize = 64;
const MAX_ENDPOINTS: usize = MAX_PINS + 2;

// Shows up in Google Home as an outlet.
const DEV_TYPE_ON_OFF_PLUG_IN_UNIT: DeviceType = DeviceType {
    dtype: 0x010A,
    drev: 3,
};

const ON_OFF: Cluster<'static> = on_off::FULL_CLUSTER
    .with_revision(6)
    .with_features(0)
    .with_attrs(with!(required))
    .with_cmds(with!(
        on_off::CommandId::Off | on_off::CommandId::On | on_off::CommandId::Toggle
    ));

const BRIDGED_INFO: Cluster<'static> = bridged::FULL_CLUSTER
    .with_features(0)
    .with_attrs(with!(required; bridged::AttributeId::NodeLabel))
    .with_cmds(with!());

const ROOT_ENDPOINT: Endpoint<'static> = root_endpoint!(eth);

const AGGREGATOR_ENDPOINT: Endpoint<'static> = Endpoint::new(
    AGGREGATOR,
    devices!(DEV_TYPE_AGGREGATOR),
    clusters!(desc::DescHandler::CLUSTER),
);

const PIN_DEVICE_TYPES: &[DeviceType] =
    devices!(DEV_TYPE_ON_OFF_PLUG_IN_UNIT, DEV_TYPE_BRIDGED_NODE);
const PIN_CLUSTERS: &[Cluster<'static>] =
    clusters!(desc::DescHandler::CLUSTER, ON_OFF, BRIDGED_INFO);

fn endpoint_id(pin_id: i64) -> Option<EndptId> {
    u16::try_from(pin_id + 1)
        .ok()
        .filter(|&id| id > AGGREGATOR && id < u16::MAX)
}

fn pin_id(endpoint_id: EndptId) -> i64 {
    i64::from(endpoint_id) - 1
}

// The node's endpoints, which follow the pins in `Bridge`.
pub struct Pins<'a> {
    bridge: &'a Bridge,
    state: Mutex<RefCell<State>>,
}

struct State {
    node: DynamicNode<'static, MAX_ENDPOINTS>,
    // The pins as Matter last saw them, one per bridged endpoint.
    pins: BTreeMap<i64, BridgedPin>,
}

impl<'a> Pins<'a> {
    pub fn new(bridge: &'a Bridge) -> Self {
        let mut node = DynamicNode::new();
        for endpoint in [ROOT_ENDPOINT, AGGREGATOR_ENDPOINT] {
            assert!(node.add(endpoint).is_ok());
        }
        let pins = Self {
            bridge,
            state: Mutex::new(RefCell::new(State {
                node,
                pins: BTreeMap::new(),
            })),
        };
        pins.update();
        pins
    }

    // Mirrors every pin change into the node and reports it to subscribers.
    pub async fn sync(&self, notifier: impl AttrChangeNotifier) -> Result<(), Error> {
        loop {
            self.bridge.changed().await;
            for (endpoint, cluster, attr) in self.update() {
                notifier.notify_attr_changed(endpoint, cluster, attr);
            }
        }
    }

    // Brings the node in line with `Bridge` and returns the attributes that
    // changed.
    fn update(&self) -> Vec<(EndptId, ClusterId, AttrId)> {
        let pins = self.bridge.snapshot();
        let mut changed = Vec::new();
        let mut parts_changed = false;
        self.state.lock(|state| {
            let state = &mut *state.borrow_mut();
            state.pins.retain(|id, _| {
                let keep = pins.contains_key(id);
                if !keep {
                    state.node.remove(endpoint_id(*id).unwrap());
                    parts_changed = true;
                }
                keep
            });
            for (&id, pin) in &pins {
                let Some(endpoint) = endpoint_id(id) else {
                    continue;
                };
                match state.pins.get(&id) {
                    Some(old) => {
                        if old.on != pin.on {
                            changed.push((endpoint, ON_OFF.id, on_off::AttributeId::OnOff as _));
                        }
                        if old.name != pin.name {
                            let attr = bridged::AttributeId::NodeLabel as _;
                            changed.push((endpoint, BRIDGED_INFO.id, attr));
                        }
                    }
                    None => {
                        let added = Endpoint::new(endpoint, PIN_DEVICE_TYPES, PIN_CLUSTERS);
                        if state.node.add(added).is_err() {
                            tracing::warn!("pin {id} not shown in Matter: over {MAX_PINS} pins");
                            continue;
                        }
                        parts_changed = true;
                    }
                }
                state.pins.insert(id, pin.clone());
            }
        });
        if parts_changed {
            let parts = desc::AttributeId::PartsList as _;
            changed.push((endpoints::ROOT_ENDPOINT_ID, desc::DescHandler::CLUSTER.id, parts));
            changed.push((AGGREGATOR, desc::DescHandler::CLUSTER.id, parts));
        }
        changed
    }

    fn pin(&self, endpoint: EndptId) -> Result<(i64, BridgedPin), Error> {
        let id = pin_id(endpoint);
        let pin = self.bridge.get(id).ok_or(ErrorCode::EndpointNotFound)?;
        Ok((id, pin))
    }

    async fn send(&self, id: i64, change: Change) -> Result<(), Error> {
        self.bridge.send(id, change).await.map_err(|status| {
            tracing::warn!("Matter couldn't change pin {id}: {status}");
            ErrorCode::Failure.into()
        })
    }
}

impl Metadata for Pins<'_> {
    fn access<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Node<'_>) -> R,
    {
        self.state.lock(|state| f(&state.borrow().node.node()))
    }
}

pub fn data_model<'a>(pins: &'a Pins<'a>, mut rand: impl Rng + Copy) -> impl DataModel + 'a {
    let handler = EthSysHandlerBuilder::new()
        .netif_diag(&SysNetifs)
        .build(rand)
        .chain(
            |e, c| e == AGGREGATOR && c == desc::DescHandler::CLUSTER.id,
            Async(desc::DescHandler::new_aggregator(Dataver::new_rand(&mut rand)).adapt()),
        )
        .chain(
            |e, c| e > AGGREGATOR && c == desc::DescHandler::CLUSTER.id,
            Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
        )
        .chain(
            |e, c| e > AGGREGATOR && c == ON_OFF.id,
            on_off::HandlerAsyncAdaptor(OnOff {
                dataver: Dataver::new_rand(&mut rand),
                pins,
            }),
        )
        .chain(
            |e, c| e > AGGREGATOR && c == BRIDGED_INFO.id,
            bridged::HandlerAsyncAdaptor(Info {
                dataver: Dataver::new_rand(&mut rand),
                pins,
            }),
        );
    (pins, handler)
}

// One On/Off cluster handler serves every pin endpoint.
struct OnOff<'a> {
    dataver: Dataver,
    pins: &'a Pins<'a>,
}

impl OnOff<'_> {
    async fn set(&self, endpoint: EndptId, on: Option<bool>) -> Result<(), Error> {
        let (id, pin) = self.pins.pin(endpoint)?;
        let on = on.unwrap_or(!pin.on);
        self.pins.send(id, Change::On(on)).await
    }
}

impl on_off::ClusterAsyncHandler for OnOff<'_> {
    const CLUSTER: Cluster<'static> = ON_OFF;

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    async fn on_off(&self, ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(self.pins.pin(ctx.attr().endpoint_id)?.1.on)
    }

    async fn handle_off(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, Some(false)).await
    }

    async fn handle_on(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, Some(true)).await
    }

    async fn handle_toggle(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, None).await
    }

    // Lighting commands, which `ON_OFF` doesn't advertise.
    async fn handle_off_with_effect(
        &self,
        _ctx: impl InvokeContext,
        _request: on_off::OffWithEffectRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_on_with_recall_global_scene(
        &self,
        _ctx: impl InvokeContext,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_on_with_timed_off(
        &self,
        _ctx: impl InvokeContext,
        _request: on_off::OnWithTimedOffRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

// Bridged Device Basic Information for every pin endpoint. Controllers use its
// node label as the device's default name.
struct Info<'a> {
    dataver: Dataver,
    pins: &'a Pins<'a>,
}

impl bridged::ClusterAsyncHandler for Info<'_> {
    const CLUSTER: Cluster<'static> = BRIDGED_INFO;

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    async fn reachable(&self, ctx: impl ReadContext) -> Result<bool, Error> {
        self.pins.pin(ctx.attr().endpoint_id).map(|_| true)
    }

    async fn unique_id<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        let (id, _) = self.pins.pin(ctx.attr().endpoint_id)?;
        builder.set(&format!("rustbox-pin-{id}"))
    }

    async fn node_label<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        let (id, pin) = self.pins.pin(ctx.attr().endpoint_id)?;
        let label = match pin.name {
            Some(name) if !name.trim().is_empty() => truncate(name, 32),
            _ => format!("Pin {id}"),
        };
        builder.set(&label)
    }

    async fn set_node_label(&self, ctx: impl WriteContext, label: &str) -> Result<(), Error> {
        if label.len() > 32 {
            return Err(ErrorCode::ConstraintError.into());
        }
        let (id, _) = self.pins.pin(ctx.attr().endpoint_id)?;
        self.pins.send(id, Change::Name(label.to_owned())).await
    }

    async fn handle_keep_active(
        &self,
        _ctx: impl InvokeContext,
        _request: KeepActiveRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

// Cuts `s` to at most `max` bytes on a character boundary.
fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let end = (0..=max).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
        s.truncate(end);
    }
    s
}
