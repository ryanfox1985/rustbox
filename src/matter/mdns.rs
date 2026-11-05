// Advertises the bridge over mDNS so controllers can find it, using
// rs-matter's built-in responder.

use std::net::{Ipv4Addr, Ipv6Addr, UdpSocket};

use rs_matter::{
    Matter,
    crypto::Crypto,
    error::{Error, ErrorCode},
    transport::network::mdns::{
        MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_SOCKET_DEFAULT_BIND_ADDR,
        builtin::{BuiltinMdns, Host},
    },
};
use if_addrs::IfAddr;
use socket2::{Domain, Protocol, Socket, Type};

pub async fn run(matter: &Matter<'_>, crypto: impl Crypto, hostname: &str) -> Result<(), Error> {
    let netif = Netif::find()?;
    tracing::info!(
        "advertising Matter on {} ({}, {:?})",
        netif.name,
        netif.ipv4,
        netif.ipv6
    );
    if netif.ipv6.is_empty() {
        tracing::warn!("{} has no IPv6 address, which Matter needs", netif.name);
    }

    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    // avahi-daemon usually listens on the mDNS port too.
    socket.set_reuse_address(true)?;
    socket.set_only_v6(false)?;
    socket.bind(&MDNS_SOCKET_DEFAULT_BIND_ADDR.into())?;
    let socket = async_io::Async::<UdpSocket>::new_nonblocking(socket.into())?;
    socket
        .get_ref()
        .join_multicast_v6(&MDNS_IPV6_BROADCAST_ADDR, netif.index)?;
    socket
        .get_ref()
        .join_multicast_v4(&MDNS_IPV4_BROADCAST_ADDR, &netif.ipv4)?;

    let ipv6: Vec<_> = netif.ipv6.iter().map(|ip| ip.octets().into()).collect();
    let host = Host {
        hostname,
        ip: netif.ipv4.octets().into(),
        ipv6: &ipv6,
    };
    BuiltinMdns::new()
        .run(
            &socket,
            &socket,
            &host,
            Some(netif.ipv4.octets().into()),
            Some(netif.index),
            matter,
            crypto,
        )
        .await
}

struct Netif {
    name: String,
    index: u32,
    ipv4: Ipv4Addr,
    ipv6: Vec<Ipv6Addr>,
}

impl Netif {
    // The first interface with an IPv4 address, preferring one with an IPv6
    // link-local address too. That skips loopback and, usually, docker0.
    fn find() -> Result<Self, Error> {
        let addrs: Vec<_> = if_addrs::get_if_addrs()?
            .into_iter()
            .filter(|addr| !addr.is_loopback())
            .collect();
        let mut netifs: Vec<Self> = Vec::new();
        for addr in &addrs {
            if let IfAddr::V4(v4) = &addr.addr
                && !netifs.iter().any(|n| n.name == addr.name)
            {
                netifs.push(Self {
                    name: addr.name.clone(),
                    index: addr.index.unwrap_or(0),
                    ipv4: v4.ip,
                    ipv6: Vec::new(),
                });
            }
        }
        for addr in &addrs {
            if let IfAddr::V6(v6) = &addr.addr
                && let Some(netif) = netifs.iter_mut().find(|n| n.name == addr.name)
            {
                netif.ipv6.push(v6.ip);
            }
        }
        if netifs.is_empty() {
            tracing::error!("no network interface with an IPv4 address for mDNS");
            return Err(ErrorCode::StdIoError.into());
        }
        let pick = netifs
            .iter()
            .position(|n| n.ipv6.iter().any(Ipv6Addr::is_unicast_link_local))
            .unwrap_or(0);
        Ok(netifs.swap_remove(pick))
    }
}
