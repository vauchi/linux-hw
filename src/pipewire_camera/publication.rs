// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::preview_source::{PreviewDirectory, PreviewLease, RemoteOpener};
use pipewire as pw;
use std::{cell::RefCell, rc::Rc};

pub(super) struct Publication {
    lease: Rc<RefCell<Option<PreviewLease>>>,
    watcher: Option<Watcher>,
    directory: PreviewDirectory,
    preview_id: String,
    remote: Option<RemoteOpener>,
}

struct Watcher {
    _events: pw::registry::Listener,
    _registry: pw::registry::RegistryRc,
}

impl Publication {
    pub(super) fn new(
        directory: PreviewDirectory,
        preview_id: String,
        remote: Option<RemoteOpener>,
    ) -> Self {
        Self {
            lease: Rc::new(RefCell::new(None)),
            watcher: None,
            directory,
            preview_id,
            remote,
        }
    }

    pub(super) fn update(
        &mut self,
        core: &pw::core::CoreRc,
        output: &pw::stream::StreamRc,
    ) -> Result<(), String> {
        let node_id = output.node_id();
        if self.watcher.is_some() || node_id == u32::MAX {
            return Ok(());
        }
        let registry = core
            .get_registry_rc()
            .map_err(|_| "camera preview registry unavailable")?;
        let lease = self.lease.clone();
        let removed = self.lease.clone();
        let directory = self.directory.clone();
        let preview_id = self.preview_id.clone();
        let remote = self.remote.clone();
        let events = registry
            .add_listener_local()
            .global(move |object| {
                if object.id != node_id || object.type_ != pw::types::ObjectType::Node {
                    return;
                }
                let Some(props) = object.props else {
                    return;
                };
                if props.get("media.class") != Some("Video/Source") {
                    return;
                }
                let Some(serial) = props
                    .get("object.serial")
                    .and_then(|value| value.parse::<u64>().ok())
                else {
                    return;
                };
                if lease.borrow().is_none() {
                    *lease.borrow_mut() = directory
                        .publish(&preview_id, node_id, serial, remote.clone())
                        .ok();
                }
            })
            .global_remove(move |id| {
                if id == node_id {
                    removed.borrow_mut().take();
                }
            })
            .register();
        self.watcher = Some(Watcher {
            _events: events,
            _registry: registry,
        });
        Ok(())
    }

    pub(super) fn ready(&self) -> bool {
        self.lease.borrow().is_some()
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        self.lease.borrow_mut().take();
    }
}
