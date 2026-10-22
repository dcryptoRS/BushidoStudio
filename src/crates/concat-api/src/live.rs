// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Live mode (#337): a caller working on the project that is open in the
//! window, with its edits landing on screen and in the window's undo
//! history.
//!
//! Without it a request naming the window's project is refused, and an
//! agent can only edit projects of its own. With it, the window hands its
//! API a [`Live`]: a request that names the window's folder is routed to
//! the window instead of to a session of the API's own.
//!
//! - Edits - a command, undo and redo, an import, the frame and rate, a
//!   save - run on the window's thread against the window's session, as
//!   the window's own gestures do, one undo step per call.
//! - Reads that render or write a file - a preview frame, an export, the
//!   document, a template - run here, on a [`Session::snapshot`] of the
//!   window's session taken at the call, so a long render never holds the
//!   window's thread.
//! - Opening the folder answers with the window's project as it stands;
//!   closing it lets go of nothing, since the project stays the window's.

use std::sync::Arc;

use concat_host::Session;
use concat_host::session::EditorView;
use concat_project::Command;

use crate::{ApiError, Done, Reply, Request};

/// A job for the window's session, run on the window's thread.
pub type LiveJob = Box<dyn FnOnce(&mut Session) -> Result<Reply, ApiError> + Send>;

/// The window's side of live mode.
pub trait Live: Send + Sync {
    /// The folder of the project the window has open, while live mode is
    /// on; `None` otherwise, and requests are handled as they always were.
    fn folder(&self) -> Option<String>;

    /// Runs `job` against the window's session on the window's thread, and
    /// waits for what it returns. `edits` says whether it changes the
    /// project, so the window follows up as it does after its own edits.
    fn run(&self, edits: bool, job: LiveJob) -> Result<Reply, ApiError>;

    /// A copy of the window's session to read from; see [`Session::snapshot`].
    fn snapshot(&self) -> Result<Session, ApiError>;
}

/// The project folder a request names, if it names one.
pub(crate) fn project_of(request: &Request) -> Option<&str> {
    match request {
        Request::ProjectOpen { path }
        | Request::ProjectClose { path, .. }
        | Request::ProjectGet { path }
        | Request::ProjectDocument { path }
        | Request::ProjectSave { path, .. }
        | Request::ProjectSetVideo { path, .. }
        | Request::EditApply { path, .. }
        | Request::EditUndo { path }
        | Request::EditRedo { path }
        | Request::MediaImport { path, .. }
        | Request::MediaReplaceMotion { path, .. }
        | Request::TemplateSave { path, .. }
        | Request::ExportRun { path, .. }
        | Request::PreviewFrame { path, .. } => Some(path),
        _ => None,
    }
}

fn view(view: EditorView) -> Result<Reply, ApiError> {
    Ok(Reply::View(Box::new(view)))
}

/// Whether `request` is a read that renders or writes a file, which the
/// API runs itself on a [`Live::snapshot`] rather than on the window's
/// thread.
pub(crate) fn on_snapshot(request: &Request) -> bool {
    matches!(
        request,
        Request::ProjectDocument { .. }
            | Request::TemplateSave { .. }
            | Request::ExportRun { .. }
            | Request::PreviewFrame { .. }
    )
}

/// What a request that names the window's project comes to, for every
/// request but those [`on_snapshot`] takes.
pub(crate) fn route(live: &Arc<dyn Live>, request: Request) -> Result<Reply, ApiError> {
    let done = || Ok(Reply::Done(Done {}));
    match request {
        Request::ProjectOpen { .. } | Request::ProjectGet { .. } => {
            live.run(false, Box::new(|session| view(session.view())))
        }
        // The project stays the window's: a caller's close lets go of
        // nothing. Saving on the way out is still a save.
        Request::ProjectClose { save, .. } => {
            if save {
                live.run(
                    false,
                    Box::new(|session| {
                        session.save(None).map_err(ApiError::failed)?;
                        Ok(Reply::Done(Done {}))
                    }),
                )
            } else {
                done()
            }
        }
        Request::ProjectSave { name, .. } => live.run(
            false,
            Box::new(move |session| {
                session.save(name.as_deref()).map_err(ApiError::failed)?;
                Ok(Reply::Done(Done {}))
            }),
        ),
        Request::ProjectSetVideo { video, .. } => live.run(
            true,
            Box::new(move |session| view(session.set_video(video).map_err(crate::refused)?)),
        ),
        Request::EditApply { command, .. } => live.run(
            true,
            Box::new(move |session| view(session.apply(*command).map_err(crate::refused)?)),
        ),
        Request::EditUndo { .. } => live.run(true, Box::new(|session| view(session.undo()))),
        Request::EditRedo { .. } => live.run(true, Box::new(|session| view(session.redo()))),
        // Probed here, off the window's thread; only the adding is the
        // window's.
        Request::MediaImport { file, motion, .. } => {
            let item = crate::probed(&file, motion)?;
            live.run(
                true,
                Box::new(move |session| {
                    view(
                        session
                            .apply(Command::AddMedia { item })
                            .map_err(crate::refused)?,
                    )
                }),
            )
        }
        Request::MediaReplaceMotion {
            media_id,
            file,
            motion,
            ..
        } => {
            let item = crate::probed(&file, motion)?;
            live.run(
                true,
                Box::new(move |session| {
                    view(
                        session
                            .apply(Command::ReplaceMotion { media_id, item })
                            .map_err(crate::refused)?,
                    )
                }),
            )
        }
        other => Err(ApiError::invalid(format!(
            "{} does not name the window's project",
            serde_json::to_string(&other).unwrap_or_default()
        ))),
    }
}
