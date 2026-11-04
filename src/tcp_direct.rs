// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The direct-send backend over TCP (private#1029 4d.7c, moved from
//! linux-gtk's `tcp_exchange.rs`). It reaches the peer at Core's endpoint
//! — the first mDNS answer within Core's browse time when the `mdns`
//! feature is built and Core gives one, else Core's fallback host and
//! port — and exchanges one frame each way with the contract's codec.
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

use vauchi_contract::DirectEndpoint;
use vauchi_contract::direct;

use crate::ports::DirectBackend;

/// TCP to the phone on the USB tether or the local network.
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpDirect;

impl DirectBackend for TcpDirect {
    fn exchange(
        &self,
        endpoint: &DirectEndpoint,
        payload: &[u8],
        is_initiator: bool,
    ) -> Result<Vec<u8>, String> {
        let address = match discover(endpoint) {
            Some(address) => address,
            None => {
                let host: IpAddr = endpoint
                    .fallback_host
                    .parse()
                    .map_err(|error| format!("bad address: {error}"))?;
                SocketAddr::new(host, endpoint.port)
            }
        };
        let timeout = Duration::from_millis(u64::from(endpoint.io_timeout_ms));
        let mut stream = TcpStream::connect_timeout(&address, timeout)
            .map_err(|error| format!("TCP connect failed: {error}"))?;
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|()| stream.set_write_timeout(Some(timeout)))
            .map_err(|error| format!("set timeout: {error}"))?;
        direct::exchange(&mut stream, payload, is_initiator)
            .map_err(|error| format!("exchange failed: {error}"))
    }
}

#[cfg(feature = "mdns")]
fn discover(endpoint: &DirectEndpoint) -> Option<SocketAddr> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    use std::time::Instant;

    if endpoint.browse_timeout_ms == 0 {
        return None;
    }
    let mdns = ServiceDaemon::new().ok()?;
    let receiver = mdns.browse(&endpoint.service_type).ok()?;
    let deadline = Instant::now() + Duration::from_millis(u64::from(endpoint.browse_timeout_ms));
    let mut found = None;
    while found.is_none() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                found = info
                    .get_addresses()
                    .iter()
                    .next()
                    .map(|address| SocketAddr::new(*address, info.get_port()));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = mdns.shutdown();
    found
}

#[cfg(not(feature = "mdns"))]
fn discover(_endpoint: &DirectEndpoint) -> Option<SocketAddr> {
    None
}
