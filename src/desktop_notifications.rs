// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The freedesktop notification service on the session bus
//! (`org.freedesktop.Notifications`), behind the `Notifier` port.
//! Core prepares title, body, urgency and link; this only carries them.

use crate::ports::Notifier;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Mutex;
use tokio::sync::{OnceCell, mpsc};
use vauchi_contract::{NotificationSpec, NotificationUrgency};
use zbus::message::Type;
use zbus::{Connection, MatchRule, MessageStream, Variant};

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const APP_NAME: &str = "Vauchi";
const APP_ICON: &str = "app.vauchi.Vauchi";
/// The action a click on the notification body invokes.
const DEFAULT_ACTION: &str = "default";
/// Notifications shown and not yet closed; bounds both maps.
const MAX_TRACKED: usize = 64;
const MAX_QUEUED_SIGNALS: NonZeroUsize = NonZeroUsize::new(64).unwrap();

#[derive(Default)]
pub struct DesktopNotifications {
    connection: OnceCell<Connection>,
    /// Core's `notification_id` → the server's id, so a repost replaces.
    replaces: Mutex<HashMap<String, u32>>,
    /// The server's id → the deep link a click opens.
    links: Mutex<HashMap<u32, String>>,
}

impl DesktopNotifications {
    async fn connection(&self) -> zbus::Result<&Connection> {
        self.connection.get_or_try_init(Connection::session).await
    }

    async fn show(&self, notification: NotificationSpec) -> zbus::Result<()> {
        let replaces_id = notification
            .notification_id
            .as_ref()
            .and_then(|id| self.replaces.lock().ok()?.get(id).copied())
            .unwrap_or(0);
        let actions: Vec<&str> = if notification.deep_link_uri.is_some() {
            vec![DEFAULT_ACTION, ""]
        } else {
            Vec::new()
        };
        let urgency: u8 = match notification.urgency {
            NotificationUrgency::Default => 1,
            NotificationUrgency::High | NotificationUrgency::Urgent => 2,
        };
        let hints = HashMap::from([("urgency", Variant::from(urgency))]);
        let reply = self
            .connection()
            .await?
            .call_method(
                Some(SERVICE),
                PATH,
                Some(SERVICE),
                "Notify",
                &(
                    APP_NAME,
                    replaces_id,
                    APP_ICON,
                    notification.title.as_str(),
                    notification.body.as_str(),
                    actions,
                    hints,
                    -1_i32,
                ),
            )
            .await?;
        let server_id: u32 = reply.body().deserialize()?;
        if let Some(id) = notification.notification_id
            && let Ok(mut replaces) = self.replaces.lock()
        {
            if replaces.len() >= MAX_TRACKED && !replaces.contains_key(&id) {
                replaces.clear();
            }
            replaces.insert(id, server_id);
        }
        if let Ok(mut links) = self.links.lock() {
            if links.len() >= MAX_TRACKED {
                links.clear();
            }
            match notification.deep_link_uri {
                Some(uri) => links.insert(server_id, uri),
                None => links.remove(&server_id),
            };
        }
        Ok(())
    }

    async fn listen(&self, opened: mpsc::Sender<String>) -> zbus::Result<()> {
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(SERVICE)
            .member("ActionInvoked")
            .build()?;
        let connection = self.connection().await?;
        let mut signals =
            MessageStream::for_match_rule(rule, connection, Some(MAX_QUEUED_SIGNALS)).await?;
        while let Some(signal) = signals.next().await {
            let Ok((id, action)) = signal?.body().deserialize::<(u32, String)>() else {
                continue;
            };
            if action != DEFAULT_ACTION {
                continue;
            }
            let link = self
                .links
                .lock()
                .ok()
                .and_then(|links| links.get(&id).cloned());
            if let Some(uri) = link
                && opened.send(uri).await.is_err()
            {
                break;
            }
        }
        Ok(())
    }
}

impl Notifier for DesktopNotifications {
    fn post(
        &self,
        notification: NotificationSpec,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            self.show(notification)
                .await
                .map_err(|_| "the desktop notification service did not accept it".to_owned())
        })
    }

    fn activations(
        &self,
        opened: mpsc::Sender<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            self.listen(opened)
                .await
                .map_err(|_| "the desktop notification service is unavailable".to_owned())
        })
    }
}
