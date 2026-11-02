// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `NfcSession` is the contract's NFC executor: `NfcActivate` waits for a
//! tag on the reader port and transmits Core's frames, `NfcSendApdu` reuses
//! the connected tag, `NfcDeactivate` ends the wait or the connection.
//! Every response and failure comes back as an event through the inbox.
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vauchi_contract::Event;
use vauchi_contract::executor::InboxLimits;
use vauchi_linux_hw::{NfcCard, NfcReader, NfcSession};

type Answer = Result<Vec<u8>, String>;

#[derive(Clone, Default)]
struct ScriptedCard {
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
    answers: Arc<Mutex<VecDeque<Answer>>>,
}

impl NfcCard for ScriptedCard {
    fn transmit(&mut self, apdu: &[u8]) -> Result<Vec<u8>, String> {
        self.sent.lock().unwrap().push(apdu.to_vec());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted answer for every frame")
    }
}

/// A reader whose tag arrives when the test says so, or never.
struct ScriptedReader {
    card: Option<ScriptedCard>,
    tag_present: Arc<AtomicBool>,
    connections: AtomicUsize,
}

impl NfcReader for ScriptedReader {
    fn wait_for_tag(&self, cancelled: &AtomicBool) -> Result<Box<dyn NfcCard>, String> {
        while !cancelled.load(Ordering::SeqCst) {
            if self.tag_present.load(Ordering::SeqCst) {
                self.connections.fetch_add(1, Ordering::SeqCst);
                return match &self.card {
                    Some(card) => Ok(Box::new(card.clone())),
                    None => Err("No smart card reader".into()),
                };
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err("NFC reader deactivated".into())
    }
}

fn reader(answers: &[Result<&[u8], &str>]) -> (Arc<ScriptedReader>, ScriptedCard) {
    let card = ScriptedCard::default();
    *card.answers.lock().unwrap() = answers
        .iter()
        .map(|answer| answer.map(<[u8]>::to_vec).map_err(str::to_string))
        .collect();
    let reader = Arc::new(ScriptedReader {
        card: Some(card.clone()),
        tag_present: Arc::new(AtomicBool::new(true)),
        connections: AtomicUsize::new(0),
    });
    (reader, card)
}

fn session(reader: Arc<ScriptedReader>, runtime: &tokio::runtime::Runtime) -> NfcSession {
    let limits = InboxLimits {
        operations: NonZeroUsize::new(1).unwrap(),
        events: NonZeroUsize::new(16).unwrap(),
        event_bytes: NonZeroUsize::new(4096).unwrap(),
    };
    NfcSession::with_backend(reader, runtime.handle().clone(), limits).unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

fn drain(session: &mut NfcSession, expected: usize) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    while events.len() < expected && Instant::now() < deadline {
        match session.pop() {
            Some(event) => events.push(event),
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    events
}

fn received(bytes: &[u8]) -> Event {
    Event::NfcApduReceived {
        bytes: bytes.to_vec(),
    }
}

fn failed(reason: &str) -> Event {
    Event::NfcFailed {
        reason: reason.into(),
    }
}

const SELECT: &[u8] = &[0x00, 0xA4, 0x04, 0x00, 0x01, 0xF0];
const CARD_SEND: &[u8] = &[0x00, 0xE2, 0x00, 0x00, 0x01, 0x07];

// @scenario: contact_exchange.feature :: NFC active exchange between two phones
#[test]
fn activate_waits_for_the_tag_then_transmits_cores_frames_and_reports_each_response() {
    let runtime = runtime();
    let (reader, card) = reader(&[Ok(&[0x90, 0x00])]);
    reader.tag_present.store(false, Ordering::SeqCst);
    let mut session = session(reader.clone(), &runtime);
    session.activate(vec![SELECT.to_vec()]).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(
        session.pop().is_none(),
        "nothing is reported before a tag arrives"
    );
    reader.tag_present.store(true, Ordering::SeqCst);
    assert_eq!(drain(&mut session, 1), vec![received(&[0x90, 0x00])]);
    assert_eq!(card.sent.lock().unwrap().clone(), vec![SELECT.to_vec()]);
}

// @scenario: contact_exchange.feature :: NFC active exchange between two phones
#[test]
fn send_reuses_the_connected_tag() {
    let runtime = runtime();
    let (reader, card) = reader(&[Ok(&[0x90, 0x00]), Ok(&[0x01, 0x90, 0x00])]);
    let mut session = session(reader.clone(), &runtime);
    session.activate(vec![SELECT.to_vec()]).unwrap();
    assert_eq!(drain(&mut session, 1), vec![received(&[0x90, 0x00])]);
    session.send(vec![CARD_SEND.to_vec()]).unwrap();
    assert_eq!(drain(&mut session, 1), vec![received(&[0x01, 0x90, 0x00])]);
    assert_eq!(reader.connections.load(Ordering::SeqCst), 1);
    assert_eq!(
        card.sent.lock().unwrap().clone(),
        vec![SELECT.to_vec(), CARD_SEND.to_vec()]
    );
}

// @internal
#[test]
fn a_reader_that_cannot_connect_reports_why() {
    let runtime = runtime();
    let (reader, _) = reader(&[]);
    let reader = Arc::new(ScriptedReader {
        card: None,
        tag_present: reader.tag_present.clone(),
        connections: AtomicUsize::new(0),
    });
    let mut session = session(reader, &runtime);
    session.activate(vec![SELECT.to_vec()]).unwrap();
    assert_eq!(drain(&mut session, 1), vec![failed("No smart card reader")]);
}

// @internal
#[test]
fn deactivate_ends_the_wait_without_a_failure_core_no_longer_expects() {
    let runtime = runtime();
    let (reader, card) = reader(&[Ok(&[0x90, 0x00])]);
    reader.tag_present.store(false, Ordering::SeqCst);
    let mut session = session(reader.clone(), &runtime);
    session.activate(vec![SELECT.to_vec()]).unwrap();
    session.deactivate();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(session.pop(), None);
    assert!(card.sent.lock().unwrap().is_empty());
}

// @internal
#[test]
fn send_without_an_activation_reports_no_tag() {
    let runtime = runtime();
    let (reader, _) = reader(&[]);
    let mut session = session(reader, &runtime);
    session.send(vec![CARD_SEND.to_vec()]).unwrap();
    assert_eq!(
        drain(&mut session, 1),
        vec![failed("no NFC tag is connected")]
    );
}
