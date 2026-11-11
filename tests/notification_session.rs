// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! `PostNotification` is a hardware command (ADR-077); linux-hw shows Core's
//! prepared notification and reports a click as `DeepLinkOpened`, for every
//! Linux pairing (private#1029). Until now only gtk-display posted them, so
//! the Qt app showed none.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use vauchi_contract::{Command, Event, NotificationSpec, NotificationUrgency};
use vauchi_linux_hw::{ExecuteError, HardwareShell, NotificationSession, Notifier};

#[derive(Default)]
struct FakeNotifier {
    shown: Mutex<Vec<NotificationSpec>>,
    clicks: Mutex<Option<mpsc::Receiver<String>>>,
    refuse: bool,
}

impl Notifier for FakeNotifier {
    fn post(
        &self,
        notification: NotificationSpec,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            if self.refuse {
                return Err("no notification service".into());
            }
            self.shown.lock().unwrap().push(notification);
            Ok(())
        })
    }

    fn activations(
        &self,
        opened: mpsc::Sender<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        let clicks = self.clicks.lock().unwrap().take();
        Box::pin(async move {
            let Some(mut clicks) = clicks else {
                return Ok(());
            };
            while let Some(uri) = clicks.recv().await {
                if opened.send(uri).await.is_err() {
                    break;
                }
            }
            Ok(())
        })
    }
}

fn notification(id: &str, deep_link: Option<&str>) -> NotificationSpec {
    NotificationSpec {
        notification_id: Some(id.into()),
        title: "Card updated".into(),
        body: "Alice changed her phone number".into(),
        deep_link_uri: deep_link.map(Into::into),
        category_id: "card_update".into(),
        channel_id: "updates".into(),
        urgency: NotificationUrgency::Default,
        category_options: Vec::new(),
    }
}

fn shell_with(notifier: Arc<FakeNotifier>) -> HardwareShell {
    let session = NotificationSession::with_backend(notifier, tokio::runtime::Handle::current());
    HardwareShell::new().with_notifications(session)
}

async fn next(shell: &mut HardwareShell) -> Option<Event> {
    timeout(Duration::from_secs(5), shell.next_event())
        .await
        .ok()
        .flatten()
}

#[tokio::test]
async fn core_s_prepared_notification_is_shown_as_given() {
    let notifier = Arc::new(FakeNotifier::default());
    let mut shell = shell_with(notifier.clone());

    let sent = notification("n-1", None);
    shell
        .execute(Command::PostNotification {
            notification: sent.clone(),
        })
        .expect("linux-hw runs PostNotification");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while notifier.shown.lock().unwrap().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    assert_eq!(*notifier.shown.lock().unwrap(), vec![sent]);
    assert_eq!(shell.pop(), None, "showing reports nothing");
}

#[tokio::test]
async fn a_click_on_a_notification_opens_its_deep_link() {
    let (click, clicks) = mpsc::channel(1);
    let notifier = Arc::new(FakeNotifier {
        clicks: Mutex::new(Some(clicks)),
        ..FakeNotifier::default()
    });
    let mut shell = shell_with(notifier);

    click
        .send("vauchi://contacts/alice".into())
        .await
        .expect("listener runs");
    assert_eq!(
        next(&mut shell).await,
        Some(Event::DeepLinkOpened {
            uri: "vauchi://contacts/alice".into()
        })
    );
}

#[tokio::test]
async fn a_notification_service_that_refuses_is_reported_not_hidden() {
    let notifier = Arc::new(FakeNotifier {
        refuse: true,
        ..FakeNotifier::default()
    });
    let mut shell = shell_with(notifier);

    shell
        .execute(Command::PostNotification {
            notification: notification("n-1", None),
        })
        .expect("the command is accepted");
    assert_eq!(
        next(&mut shell).await,
        Some(Event::HardwareError {
            transport: "notification".into(),
            error: "no notification service".into()
        })
    );
}

#[test]
fn without_a_notifier_the_command_is_unsupported() {
    let mut shell = HardwareShell::new();
    assert_eq!(
        shell.execute(Command::PostNotification {
            notification: notification("n-1", None)
        }),
        Err(ExecuteError::Unsupported)
    );
}
