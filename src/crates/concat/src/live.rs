// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The window's side of live mode (#337; `concat_api::live`): the window's
//! server reaching the project on screen.
//!
//! The server runs on threads of its own, and the session lives on the
//! event-loop thread with the rest of the window's state, so every call
//! crosses with `slint::invoke_from_event_loop` and waits for its answer on
//! a channel. Which folder is live is kept beside the server, set as a
//! project opens and cleared as it closes, so the server can tell without
//! asking the window.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use concat_api::{ApiError, ErrorCode, Live, LiveJob, Reply};
use concat_host::Session;

use crate::host::Shell;

/// How long a call waits for the window. An edit takes milliseconds; a
/// window this slow to answer is stuck, and the caller hears so rather
/// than hanging with it.
const WAIT: Duration = Duration::from_secs(60);

/// The folder of the project the window has open, shared with the server.
#[derive(Clone, Default)]
pub struct LiveFolder(Arc<Mutex<Option<String>>>);

impl LiveFolder {
    /// The window opened `folder`, or closed its project (`None`).
    pub fn set(&self, folder: Option<String>) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = folder;
    }

    fn get(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// What the window hands its server while live mode is on.
pub struct WindowLive {
    folder: LiveFolder,
}

impl WindowLive {
    pub fn new(folder: LiveFolder) -> Self {
        Self { folder }
    }
}

fn gone() -> ApiError {
    ApiError::new(
        ErrorCode::Failed,
        "the window did not answer: it is closing, or stuck".to_owned(),
    )
}

impl Live for WindowLive {
    fn folder(&self) -> Option<String> {
        self.folder.get()
    }

    fn run(&self, edits: bool, job: LiveJob) -> Result<Reply, ApiError> {
        let (answer, answered) = mpsc::channel();
        slint::invoke_from_event_loop(move || {
            Shell::with(|shell, app| {
                let reply = shell.studio.borrow_mut().run_live(edits, job);
                if edits {
                    shell.studio.borrow().publish(&app, &shell.models);
                }
                let _ = answer.send(reply);
            });
        })
        .map_err(|_| gone())?;
        answered.recv_timeout(WAIT).map_err(|_| gone())?
    }

    fn snapshot(&self) -> Result<Session, ApiError> {
        let (answer, answered) = mpsc::channel();
        slint::invoke_from_event_loop(move || {
            Shell::with(|shell, _| {
                let copy = shell
                    .studio
                    .borrow()
                    .session
                    .as_ref()
                    .map(Session::snapshot);
                let _ = answer.send(copy);
            });
        })
        .map_err(|_| gone())?;
        answered
            .recv_timeout(WAIT)
            .map_err(|_| gone())?
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::NotOpen,
                    "the window has no project open".to_owned(),
                )
            })
    }
}
