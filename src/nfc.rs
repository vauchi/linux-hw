// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The contract's NFC executor (ADR-077 migration plan, Phase 2).
//!
//! `NfcActivate` starts one operation: wait for a tag on the reader port,
//! transmit Core's frames, then keep the tag for later `NfcSendApdu`
//! frames, which arrive over a channel. `NfcDeactivate` cancels the wait
//! or ends the connection. Each response is an `NfcApduReceived`, each
//! failure an `NfcFailed`; the session interprets no status word.
use crate::delivery::EventSender;
use crate::executor::Executor;
use crate::ports::{ExecutorError, NfcCard, NfcReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use vauchi_contract::Event;
use vauchi_contract::executor::{DeliveryError, InboxLimits};

const MAX_QUEUED_FRAMES: usize = 64;
const MAX_FRAMES_PER_REQUEST: usize = 64;
const MAX_APDU: usize = 65_535;

/// The frames of later `NfcSendApdu`s, handed to the running activation.
type FrameQueue = Arc<Mutex<Option<mpsc::Receiver<Vec<Vec<u8>>>>>>;

/// The connected tag and the frames sent on it; pure, so the reader
/// session tests run without a runtime.
pub struct ReaderSession<C> {
    card: Option<C>,
}

impl<C: NfcCard> Default for ReaderSession<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: NfcCard> ReaderSession<C> {
    pub fn new() -> Self {
        Self { card: None }
    }

    pub fn activate(
        &mut self,
        connect: impl FnOnce() -> Result<C, String>,
        apdus: &[Vec<u8>],
    ) -> Vec<Event> {
        self.card = None;
        match connect() {
            Ok(card) => {
                self.card = Some(card);
                self.send(apdus)
            }
            Err(reason) => vec![Event::NfcFailed { reason }],
        }
    }

    pub fn send(&mut self, apdus: &[Vec<u8>]) -> Vec<Event> {
        let Some(card) = self.card.as_mut() else {
            return vec![no_tag()];
        };
        let mut events = Vec::with_capacity(apdus.len());
        for apdu in apdus {
            match card.transmit(apdu) {
                Ok(bytes) => events.push(Event::NfcApduReceived { bytes }),
                Err(reason) => {
                    self.card = None;
                    events.push(Event::NfcFailed { reason });
                    break;
                }
            }
        }
        events
    }

    pub fn deactivate(&mut self) {
        self.card = None;
    }
}

fn no_tag() -> Event {
    Event::NfcFailed {
        reason: "no NFC tag is connected".into(),
    }
}

#[derive(Clone)]
enum Request {
    /// Wait for a tag, transmit the frames, then serve later frames.
    Activate {
        apdus: Vec<Vec<u8>>,
        frames: FrameQueue,
        cancelled: Arc<AtomicBool>,
    },
    /// Frames with no activation: Core learns there is no tag.
    NoTag,
}

pub struct NfcSession {
    executor: Executor<Request>,
    frames: Option<mpsc::Sender<Vec<Vec<u8>>>>,
    cancelled: Option<Arc<AtomicBool>>,
}

impl NfcSession {
    pub fn with_backend(
        backend: Arc<dyn NfcReader>,
        runtime: Handle,
        limits: InboxLimits,
    ) -> Result<Self, ExecutorError> {
        let native = move |request: Request, events: EventSender| {
            let backend = backend.clone();
            Box::pin(async move {
                match request {
                    Request::NoTag => report(events, vec![no_tag()]).await,
                    Request::Activate {
                        apdus,
                        frames,
                        cancelled,
                    } => {
                        let frames = frames.lock().ok().and_then(|mut slot| slot.take());
                        serve(backend, apdus, frames, cancelled, events).await
                    }
                }
            }) as _
        };
        Ok(Self {
            executor: Executor::new(native, &runtime, limits)?,
            frames: None,
            cancelled: None,
        })
    }

    /// `NfcActivate`: wait for a tag, then transmit Core's frames.
    pub fn activate(&mut self, apdus: Vec<Vec<u8>>) -> Result<(), DeliveryError> {
        self.executor.ensure_open()?;
        check_frames(&apdus)?;
        self.cancel_wait();
        let (sender, receiver) = mpsc::channel(MAX_QUEUED_FRAMES);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.executor.start(Request::Activate {
            apdus,
            frames: Arc::new(Mutex::new(Some(receiver))),
            cancelled: cancelled.clone(),
        })?;
        self.frames = Some(sender);
        self.cancelled = Some(cancelled);
        Ok(())
    }

    /// `NfcSendApdu`: transmit Core's frames on the connected tag.
    pub fn send(&mut self, apdus: Vec<Vec<u8>>) -> Result<(), DeliveryError> {
        self.executor.ensure_open()?;
        check_frames(&apdus)?;
        match self.frames.as_ref() {
            Some(frames) => frames.try_send(apdus).map_err(|_| DeliveryError::Full),
            None => self.executor.start(Request::NoTag),
        }
    }

    /// `NfcDeactivate`: end the wait or the connection, reporting nothing.
    pub fn deactivate(&mut self) {
        self.cancel_wait();
        self.frames = None;
        self.executor.stop();
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.executor.pop()
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.executor.next_event().await
    }

    pub async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        self.cancel_wait();
        self.executor.shutdown().await
    }

    fn cancel_wait(&mut self) {
        if let Some(cancelled) = self.cancelled.take() {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for NfcSession {
    fn drop(&mut self) {
        self.cancel_wait();
    }
}

fn check_frames(apdus: &[Vec<u8>]) -> Result<(), DeliveryError> {
    if apdus.len() > MAX_FRAMES_PER_REQUEST || apdus.iter().any(|apdu| apdu.len() > MAX_APDU) {
        return Err(DeliveryError::PayloadTooLarge);
    }
    Ok(())
}

async fn report(
    mut events: EventSender,
    batch: Vec<Event>,
) -> Result<(), crate::ports::HardwareFailure> {
    for event in batch {
        events
            .send(event)
            .await
            .map_err(|_| crate::ports::HardwareFailure::Failed)?;
    }
    Ok(())
}

/// The activation: the blocking reader work runs on a blocking thread;
/// the session lives there, and frames reach it over the channel.
async fn serve(
    backend: Arc<dyn NfcReader>,
    apdus: Vec<Vec<u8>>,
    frames: Option<mpsc::Receiver<Vec<Vec<u8>>>>,
    cancelled: Arc<AtomicBool>,
    mut events: EventSender,
) -> Result<(), crate::ports::HardwareFailure> {
    let wait = cancelled.clone();
    let connected = tokio::task::spawn_blocking(move || backend.wait_for_tag(&wait))
        .await
        .map_err(|_| crate::ports::HardwareFailure::Failed)?;
    if cancelled.load(Ordering::SeqCst) {
        return Ok(());
    }
    let mut session = ReaderSession::<Box<dyn NfcCard>>::new();
    let first = session.activate(|| connected, &apdus);
    for event in first {
        events
            .send(event)
            .await
            .map_err(|_| crate::ports::HardwareFailure::Failed)?;
    }
    let Some(mut frames) = frames else {
        return Ok(());
    };
    while let Some(apdus) = frames.recv().await {
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        let (next, responses) = tokio::task::spawn_blocking(move || {
            let responses = session.send(&apdus);
            (session, responses)
        })
        .await
        .map_err(|_| crate::ports::HardwareFailure::Failed)?;
        session = next;
        for event in responses {
            events
                .send(event)
                .await
                .map_err(|_| crate::ports::HardwareFailure::Failed)?;
        }
    }
    Ok(())
}

impl NfcCard for Box<dyn NfcCard> {
    fn transmit(&mut self, apdu: &[u8]) -> Result<Vec<u8>, String> {
        (**self).transmit(apdu)
    }
}
