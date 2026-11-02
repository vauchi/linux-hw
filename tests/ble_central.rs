// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The outbound link: `BleConnect` dials the peer's GATT server through the
//! central port, Core's outbound writes and reads go to that link, and
//! `BleDisconnect` drops it. What the peer sends comes back as events.
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use vauchi_contract::executor::{DeliveryError, InboxLimits};
use vauchi_contract::{BleLinkDirection, Command, Event};
use vauchi_linux_hw::{
    BleCentral, Central, EventSender, ExecuteError, HardwareFailure, HardwareShell, LinkRequest,
    LinkRequests,
};

const PEER: &str = "AA:BB:CC:DD:EE:FF";
const DATA: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567892";

struct Released(mpsc::UnboundedSender<String>);

impl Drop for Released {
    fn drop(&mut self) {
        let _ = self.0.send("released".into());
    }
}

/// A peer that answers every read with the read's UUID as bytes.
struct EchoPeer {
    lifecycle: mpsc::UnboundedSender<String>,
    written: mpsc::UnboundedSender<(String, Vec<u8>)>,
}

impl Central for EchoPeer {
    fn connect(
        &self,
        device_id: String,
        mut requests: LinkRequests,
        mut events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>> {
        Box::pin(async move {
            let _released = Released(self.lifecycle.clone());
            self.lifecycle.send(format!("connect {device_id}")).unwrap();
            events
                .send(Event::BleConnected {
                    device_id: device_id.clone(),
                    direction: BleLinkDirection::Outbound,
                })
                .await
                .unwrap();
            while let Some(request) = requests.next().await {
                match request {
                    LinkRequest::Write { uuid, data } => self.written.send((uuid, data)).unwrap(),
                    LinkRequest::Read { uuid } => events
                        .send(Event::BleCharacteristicRead {
                            device_id: device_id.clone(),
                            direction: BleLinkDirection::Outbound,
                            data: uuid.clone().into_bytes(),
                            uuid,
                        })
                        .await
                        .unwrap(),
                }
            }
            std::future::pending().await
        })
    }
}

struct Fixture {
    shell: HardwareShell,
    lifecycle: mpsc::UnboundedReceiver<String>,
    written: mpsc::UnboundedReceiver<(String, Vec<u8>)>,
}

fn fixture() -> Fixture {
    let (lifecycle_tx, lifecycle) = mpsc::unbounded_channel();
    let (written_tx, written) = mpsc::unbounded_channel();
    let central = BleCentral::with_backend(
        Arc::new(EchoPeer {
            lifecycle: lifecycle_tx,
            written: written_tx,
        }),
        tokio::runtime::Handle::current(),
        InboxLimits {
            operations: NonZeroUsize::new(1).unwrap(),
            events: NonZeroUsize::new(4).unwrap(),
            event_bytes: NonZeroUsize::new(4096).unwrap(),
        },
    )
    .unwrap();
    Fixture {
        shell: HardwareShell::new().with_central(central),
        lifecycle,
        written,
    }
}

async fn next_event(shell: &mut HardwareShell) -> Event {
    loop {
        if let Some(event) = shell.pop() {
            return event;
        }
        tokio::task::yield_now().await;
    }
}

fn outbound(command: fn(String) -> Command) -> Command {
    command(PEER.into())
}

// @scenario: contact_exchange.feature :: Discover nearby Vauchi users via BLE
#[tokio::test]
async fn a_connect_dials_the_peer_and_core_hears_the_outbound_link() {
    let mut f = fixture();

    f.shell
        .execute(outbound(|device_id| Command::BleConnect { device_id }))
        .unwrap();

    assert_eq!(f.lifecycle.recv().await.unwrap(), format!("connect {PEER}"));
    assert_eq!(
        next_event(&mut f.shell).await,
        Event::BleConnected {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
        }
    );
    f.shell.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn outbound_writes_and_reads_go_to_the_connected_link() {
    let mut f = fixture();
    f.shell
        .execute(outbound(|device_id| Command::BleConnect { device_id }))
        .unwrap();
    next_event(&mut f.shell).await;

    f.shell
        .execute(Command::BleWriteCharacteristic {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
            uuid: DATA.into(),
            data: vec![1, 2, 3],
        })
        .unwrap();
    f.shell
        .execute(Command::BleReadCharacteristic {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
            uuid: DATA.into(),
        })
        .unwrap();

    assert_eq!(
        f.written.recv().await.unwrap(),
        (DATA.into(), vec![1, 2, 3])
    );
    assert_eq!(
        next_event(&mut f.shell).await,
        Event::BleCharacteristicRead {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
            uuid: DATA.into(),
            data: DATA.as_bytes().to_vec(),
        }
    );
    f.shell.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_disconnect_drops_the_outbound_link() {
    let mut f = fixture();
    f.shell
        .execute(outbound(|device_id| Command::BleConnect { device_id }))
        .unwrap();
    assert_eq!(f.lifecycle.recv().await.unwrap(), format!("connect {PEER}"));

    f.shell
        .execute(Command::BleDisconnect {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
        })
        .unwrap();

    assert_eq!(f.lifecycle.recv().await.unwrap(), "released");
    f.shell.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_request_for_another_device_is_refused_and_leaves_the_link_up() {
    let mut f = fixture();
    f.shell
        .execute(outbound(|device_id| Command::BleConnect { device_id }))
        .unwrap();
    next_event(&mut f.shell).await;

    let other = "11:22:33:44:55:66".to_string();
    assert_eq!(
        f.shell.execute(Command::BleWriteCharacteristic {
            device_id: other.clone(),
            direction: BleLinkDirection::Outbound,
            uuid: DATA.into(),
            data: vec![9],
        }),
        Err(ExecuteError::Delivery(DeliveryError::Inactive))
    );
    assert_eq!(
        f.shell.execute(Command::BleDisconnect {
            device_id: other,
            direction: BleLinkDirection::Outbound,
        }),
        Err(ExecuteError::Delivery(DeliveryError::Inactive))
    );
    f.shell
        .execute(Command::BleReadCharacteristic {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
            uuid: DATA.into(),
        })
        .unwrap();
    assert!(matches!(
        next_event(&mut f.shell).await,
        Event::BleCharacteristicRead { .. }
    ));
    f.shell.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn outbound_commands_without_a_central_are_unsupported() {
    let mut shell = HardwareShell::new();

    assert_eq!(
        shell.execute(outbound(|device_id| Command::BleConnect { device_id })),
        Err(ExecuteError::Unsupported)
    );
    assert_eq!(
        shell.execute(Command::BleReadCharacteristic {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
            uuid: DATA.into(),
        }),
        Err(ExecuteError::Unsupported)
    );
}
