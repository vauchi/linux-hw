// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::Response;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use zbus::{Connection, Message, OwnedObjectPath, OwnedVariant};

pub(super) async fn respond(
    service: &Connection,
    message: &Message,
    response: Response,
    accesses: &AtomicUsize,
    opened: &Notify,
) {
    accesses.fetch_add(1, Ordering::SeqCst);
    if matches!(response, Response::HoldReply) {
        opened.notify_one();
        return;
    }
    let header = message.header();
    let options: HashMap<String, OwnedVariant> = message.body().deserialize().unwrap();
    let token = String::try_from(options["handle_token"].try_clone().unwrap()).unwrap();
    let sender = header.sender().unwrap().as_str().to_owned();
    let path = format!(
        "/org/freedesktop/portal/desktop/request/{}/{}",
        sender.trim_start_matches(':').replace('.', "_"),
        token
    );
    let path = OwnedObjectPath::try_from(path).unwrap();
    if matches!(response, Response::Malformed) {
        service
            .emit_signal(
                Some(sender.as_str()),
                path.as_str(),
                "org.freedesktop.portal.Request",
                "Response",
                &"invalid",
            )
            .await
            .unwrap();
    } else if !matches!(response, Response::Wait) {
        let code = if matches!(response, Response::Grant | Response::NativeGrant) {
            0u32
        } else {
            1u32
        };
        service
            .emit_signal(
                Some(sender.as_str()),
                path.as_str(),
                "org.freedesktop.portal.Request",
                "Response",
                &(code, HashMap::<String, OwnedVariant>::new()),
            )
            .await
            .unwrap();
    }
    service.reply(&header, &path).await.unwrap();
    opened.notify_one();
}
