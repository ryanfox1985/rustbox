// Makes the pins controllable from Matter controllers such as Google Home. The
// Pi shows up as a Matter bridge with one on/off plug per pin. rs-matter runs
// on its own thread and talks to the rest of the app through `Bridge`.

mod mdns;
mod node;

use std::{fs, io, net::UdpSocket, path::Path, path::PathBuf, pin::pin, sync::Arc, thread};

use embassy_futures::select::{select, select4};
use rs_matter::{
    BasicCommData, MATTER_PORT, Matter,
    crypto::{Crypto, default_crypto},
    dm::{
        clusters::basic_info::BasicInfoConfig,
        devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_DET},
        networks::eth::EthNetwork,
    },
    error::Error,
    im::{EthInteractionModelState, InteractionModel},
    pairing::{DiscoveryCapabilities, qr::QrTextType},
    persist::DirKvBlobStore,
    respond::DefaultResponder,
    sc::pase::{
        MAX_COMM_WINDOW_TIMEOUT_SECS, Spake2pVerifierPassword, Spake2pVerifierPasswordRef,
    },
    transport::{MATTER_SOCKET_BIND_ADDR, exchange::MatterBuffers},
    utils::select::Coalesce,
};

use crate::bridge::Bridge;

// The setup code and discriminator rs-matter's test device uses.
const DEFAULT_PASSCODE: u32 = 20202021;
const DEFAULT_DISCRIMINATOR: u16 = 3840;

// rs-matter keeps its buffers on the stack, which outgrows the 2 MiB default.
const STACK_SIZE: usize = 8 << 20;

struct Config {
    dir: PathBuf,
    passcode: u32,
    discriminator: u16,
}

impl Config {
    // `None` when Matter is turned off with `MATTER=off`.
    fn from_env() -> Result<Option<Self>, String> {
        let env = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
        if matches!(env("MATTER").as_deref(), Some("off" | "false" | "0")) {
            return Ok(None);
        }
        let passcode = match env("MATTER_PASSCODE") {
            None => DEFAULT_PASSCODE,
            Some(p) => p
                .parse()
                .ok()
                .filter(|&p| valid_passcode(p))
                .ok_or(format!("MATTER_PASSCODE {p} isn't a valid Matter setup code"))?,
        };
        let discriminator = match env("MATTER_DISCRIMINATOR") {
            None => DEFAULT_DISCRIMINATOR,
            Some(d) => d
                .parse()
                .ok()
                .filter(|&d| d <= 0xFFF)
                .ok_or(format!("MATTER_DISCRIMINATOR {d} isn't between 0 and 4095"))?,
        };
        Ok(Some(Self {
            dir: env("MATTER_DIR").unwrap_or_else(|| "matter".into()).into(),
            passcode,
            discriminator,
        }))
    }
}

// The Matter spec rejects codes outside 1..=99999998 and these trivial ones.
fn valid_passcode(code: u32) -> bool {
    const TRIVIAL: [u32; 10] = [
        11111111, 22222222, 33333333, 44444444, 55555555, 66666666, 77777777, 88888888,
        12345678, 87654321,
    ];
    (1..=99999998).contains(&code) && !TRIVIAL.contains(&code)
}

pub fn spawn(bridge: Arc<Bridge>) {
    let config = match Config::from_env() {
        Ok(Some(config)) => config,
        Ok(None) => return tracing::info!("Matter is off"),
        Err(err) => return tracing::error!("Matter is off: {err}"),
    };
    let spawned = thread::Builder::new()
        .name("matter".into())
        .stack_size(STACK_SIZE)
        .spawn(move || {
            if let Err(err) = run(&config, &bridge) {
                tracing::error!("Matter stopped: {err}");
            }
        });
    if let Err(err) = spawned {
        tracing::error!("couldn't start Matter: {err}");
    }
}

fn run(config: &Config, bridge: &Bridge) -> Result<(), Error> {
    let hostname = hostname(&config.dir)?;

    // The vendor and product IDs must stay the test ones: the test device
    // attestation certificate is only valid for them.
    let info = BasicInfoConfig {
        vendor_name: "rustbox",
        product_name: "rustbox",
        device_name: "rustbox",
        ..TEST_DEV_DET
    };
    let passcode = config.passcode.to_le_bytes();
    let comm = BasicCommData {
        password: Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(
            &passcode,
        )),
        discriminator: config.discriminator,
    };
    let matter = Matter::new(&info, comm, &TEST_DEV_ATT, MATTER_PORT);
    let buffers: MatterBuffers = MatterBuffers::new();
    let state: EthInteractionModelState = EthInteractionModelState::new(EthNetwork::new_default());
    let kv = matter.kv(DirKvBlobStore::new(config.dir.clone()));
    matter.startup(&kv)?;

    let crypto = default_crypto(rand::rng(), DAC_PRIVKEY);
    let rand = crypto.rand()?;

    let pins = node::Pins::new(bridge);
    let im = InteractionModel::new(
        &matter,
        &crypto,
        &buffers,
        node::data_model(&pins, rand),
        &kv,
        &state,
    );
    futures_lite::future::block_on(im.startup())?;

    let responder = DefaultResponder::new(&im);
    let mut respond = pin!(responder.run::<4, 4>());
    let mut im_job = pin!(im.run());
    let socket = async_io::Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)?;
    let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));
    let mut mdns = pin!(mdns::run(&matter, &crypto, &hostname));
    let mut sync = pin!(pins.sync(&im));

    if matter.has_fabrics() {
        tracing::info!("Matter is running");
    } else {
        matter.print_standard_qr_text(DiscoveryCapabilities::IP)?;
        matter.print_standard_qr_code(QrTextType::Unicode, DiscoveryCapabilities::IP)?;
        matter.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, &())?;
        tracing::info!(
            "Matter pairing is open for {} minutes; restart rustbox to reopen it",
            MAX_COMM_WINDOW_TIMEOUT_SECS / 60
        );
    }

    let stack = select4(&mut transport, &mut mdns, &mut respond, &mut im_job).coalesce();
    futures_lite::future::block_on(select(stack, &mut sync).coalesce())
}

// The mDNS host name, 12 hex digits like a MAC address. Random but kept
// across restarts, so controllers can find the bridge again.
fn hostname(dir: &Path) -> io::Result<String> {
    let path = dir.join("hostname");
    if let Ok(name) = fs::read_to_string(&path) {
        let name = name.trim();
        if name.len() == 12 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(name.to_owned());
        }
    }
    let name = format!("{:012X}", rand::random::<u64>() & 0xFFFF_FFFF_FFFF);
    fs::create_dir_all(dir)?;
    fs::write(&path, &name)?;
    Ok(name)
}
