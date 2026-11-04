// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `TcpDirect` is the direct-send backend over TCP (private#1029 4d.7c):
//! it reaches the peer at Core's endpoint (mDNS when Core gives a browse
//! time, else Core's fallback host and port) and exchanges one frame each
//! way with the contract's codec. A loopback listener stands in for the
//! phone, speaking the same codec.
use std::net::TcpListener;
use std::thread;
use vauchi_contract::DirectEndpoint;
use vauchi_contract::direct;
use vauchi_linux_hw::{DirectBackend, TcpDirect};

fn endpoint(port: u16) -> DirectEndpoint {
    DirectEndpoint {
        service_type: "_vauchi._tcp.local.".into(),
        port,
        fallback_host: "127.0.0.1".into(),
        browse_timeout_ms: 0,
        io_timeout_ms: 2_000,
    }
}

// @internal — the initiator sends first; the phone answers with its payload.
#[test]
fn the_initiator_reaches_the_fallback_host_and_gets_the_peers_payload() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let phone = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        direct::exchange(&mut stream, b"phone card", false).unwrap()
    });

    let received = TcpDirect
        .exchange(&endpoint(port), b"desktop card", true)
        .expect("exchange");

    assert_eq!(received, b"phone card");
    assert_eq!(phone.join().unwrap(), b"desktop card");
}

// @internal — as responder the desktop reads the phone's frame first.
#[test]
fn the_responder_reads_first_then_answers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let phone = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        direct::exchange(&mut stream, b"phone card", true).unwrap()
    });

    let received = TcpDirect
        .exchange(&endpoint(port), b"desktop card", false)
        .expect("exchange");

    assert_eq!(received, b"phone card");
    assert_eq!(phone.join().unwrap(), b"desktop card");
}

// @scenario: generic_presentation_protocol.feature :: Invalid boundary input fails safely
#[test]
fn a_bad_address_or_a_closed_port_is_an_error_not_a_hang() {
    let mut bad_host = endpoint(41_234);
    bad_host.fallback_host = "not an address".into();
    let error = TcpDirect.exchange(&bad_host, b"card", true).unwrap_err();
    assert!(error.contains("address"), "{error}");

    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let error = TcpDirect
        .exchange(&endpoint(port), b"card", true)
        .unwrap_err();
    assert!(error.contains("connect"), "{error}");
}

// @scenario: generic_presentation_protocol.feature :: Invalid boundary input fails safely
#[test]
fn a_peer_speaking_another_protocol_is_refused() {
    use std::io::Write;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let phone = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
    });

    let error = TcpDirect
        .exchange(&endpoint(port), b"card", false)
        .unwrap_err();

    assert!(error.contains("exchange failed"), "{error}");
    phone.join().unwrap();
}

// @scenario: generic_presentation_protocol.feature :: Invalid boundary input fails safely
#[test]
fn a_phone_answering_with_an_empty_frame_is_refused() {
    use std::io::Write;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let phone = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"VXCH\x01\x00\x00\x00\x00").unwrap();
        // Stay on the line for the reply, so only the empty frame can fail.
        let _ = direct::read_frame(&mut stream);
    });

    let error = TcpDirect
        .exchange(&endpoint(port), b"card", false)
        .unwrap_err();

    assert!(error.contains("exchange failed"), "{error}");
    phone.join().unwrap();
}
