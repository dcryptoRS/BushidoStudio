// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The engine-owned editing session.
//!
//! Open a project folder and the engine holds the edit: every mutation is a
//! `concat_project` [`Command`], applied with undo recorded, and the new
//! state is what the window draws. The window never keeps a model of its
//! own; it renders the [`Project`] this session hands back.
//!
//! Saving reuses `projects::save`'s temp-file-and-rename, so the document on
//! disk is written by exactly one code path.

use concat_project::model::VideoSettings;
use concat_project::{Command, DocumentSettings, Editor, Project};
use serde::Serialize;

use crate::projects;

/// One open project: its folder, its settings and its undo history.
pub struct Session {
    /// The project folder, for saving.
    path: String,
    settings: DocumentSettings,
    editor: Editor,
    /// The links to retired effects the open rewrote, old id and new, each
    /// pair once: what the window names as the project opens.
    upgraded: Vec<(String, String)>,
    /// The clips whose retired fields the open took off.
    retired: concat_project::RetiredFields,
    /// The open gave a project made before lanes had kinds its picture and
    /// sound lanes, and took its sound out onto them (Royalty Studio fork):
    /// the first save keeps the document as it was beside it, first.
    converted: bool,
}

/// What the document of a project converted to picture and sound lanes is
/// kept as, beside it, before the first save writes over it.
pub const BEFORE_LANES: &str = "concat.before-av-tracks.json";

/// Keeps the document in `path` as it was before lanes had kinds, beside
/// it as [`BEFORE_LANES`]: once, and never over a copy already there - the
/// first is the one from before. A copy that cannot be made is logged and
/// does not stop the save, whose document holds every edit either way.
fn keep_before_lanes(path: &str) {
    let folder = std::path::Path::new(path);
    let (from, to) = (projects::manifest_path(folder), folder.join(BEFORE_LANES));
    if !from.is_file() || to.exists() {
        return;
    }
    if let Err(error) = std::fs::copy(&from, &to) {
        log::warn!("project: could not keep {}: {error}", to.display());
    }
}

/// What every mutating call returns: the authoritative state plus history
/// availability, so undo/redo affordances are never guessing.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EditorView {
    /// The whole project, as the engine holds it.
    pub project: Project,
    /// Whether there is something to undo.
    pub can_undo: bool,
    /// Whether there is something to redo.
    pub can_redo: bool,
    /// The settings as the session holds them - the document's own output
    /// size wins over the manifest's on open.
    pub settings: SettingsView,
    /// The id a creating command minted, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_id: Option<String>,
}

/// The session's settings, as the window shows them.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    /// Project name.
    pub name: String,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Frame rate numerator.
    pub rate_num: i64,
    /// Frame rate denominator.
    pub rate_den: i64,
}

impl Session {
    /// Opens a project folder as the editing session.
    ///
    /// A folder with no document yet opens as an empty project rather than
    /// failing, but a document that is there and cannot be read or parsed
    /// is an error, because silently replacing an edit with emptiness is
    /// how projects get lost. `settings` come from the manifest and seed the first timeline
    /// of a project that has no document yet; a document that loads brings
    /// every timeline's own frame with it, and those win, because that is
    /// where an edited frame was saved.
    pub fn open(path: &str, settings: DocumentSettings) -> Result<Session, String> {
        let editor = match projects::read_document(path) {
            Ok(Some(document)) => match Editor::from_document(&document) {
                Some(editor) => editor,
                // The settings-only manifest `create` writes: a project
                // closed before its first edit reopens empty, it is not
                // corrupt.
                None if projects::is_settings_only(&document) => {
                    Editor::with_video(settings.video())
                }
                None if concat_project::document_version(&document)
                    > concat_project::DOCUMENT_VERSION =>
                {
                    return Err(format!(
                        "{path} was saved by a newer Concat than this one: update to open it"
                    ));
                }
                None => {
                    return Err(format!("{path} holds a document this build cannot read"));
                }
            },
            // No document yet - a project created moments ago.
            Ok(None) => Editor::with_video(settings.video()),
            // Unreadable or not JSON: a save cut short, a disk in trouble.
            // The one thing open must not do is answer with an empty
            // project, since the next save would replace what was edited.
            Err(error) => return Err(error),
        };
        // A link to a retired effect opens as a link to the one that stands
        // in for it, its knobs carried over (`[[replaces]]`).
        let mut editor = editor;
        let catalogue = concat_effects::Catalogue::builtin();
        let mut upgraded: Vec<(String, String)> = Vec::new();
        editor.upgrade_links(|link| {
            let old = link.id.clone();
            let changed = catalogue.upgrade(link);
            let pair = (old, link.id.clone());
            if changed && !upgraded.contains(&pair) {
                upgraded.push(pair);
            }
            changed
        });
        // 0.2.4's mask effects, retired with no stand-in on the chain,
        // open as the clip's own mask (#239).
        for old in editor.upgrade_mask_effects() {
            upgraded.push((old, "mask".to_owned()));
        }
        // Fields an earlier version wrote and this one does not read, off
        // the clips, with the clips named for the window to say so.
        let retired = editor.drop_retired_fields();
        // Picture lanes over sound lanes (Royalty Studio fork): a new
        // project is born with them, an older one is given them here.
        let had_clips = editor
            .project()
            .timelines
            .iter()
            .any(|timeline| !timeline.clips.is_empty());
        let converted = editor.type_lanes() && had_clips;
        Ok(Session {
            path: path.to_owned(),
            settings,
            editor,
            upgraded,
            retired,
            converted,
        })
    }

    /// Opens the project a [`projects::ProjectInfo`] describes.
    pub fn open_info(info: &projects::ProjectInfo) -> Result<Session, String> {
        Session::open(
            &info.path,
            DocumentSettings {
                name: info.name.clone(),
                width: info.width,
                height: info.height,
                rate_num: info.rate_num,
                rate_den: info.rate_den,
            },
        )
    }

    /// The links to retired effects this open read as links to the
    /// effects that stand in for them, old id and new. Empty when the
    /// document named none; the change is the document's once it is saved.
    pub fn upgraded(&self) -> &[(String, String)] {
        &self.upgraded
    }

    /// The clips this open found with fields an earlier version wrote and
    /// this one no longer reads, now taken off: reversed ones that play
    /// forwards, animated ones that hold still. Empty for most documents.
    pub fn retired(&self) -> &concat_project::RetiredFields {
        &self.retired
    }

    /// The project folder.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The session's settings: the project's name, and the frame and rate of
    /// the timeline being edited.
    ///
    /// Built rather than stored, because the frame is the active timeline's
    /// and the active timeline changes under a tab click. Everything that
    /// renders - the monitor, the export - asks here and gets the frame of
    /// whatever it is about to draw.
    pub fn settings(&self) -> DocumentSettings {
        let video = self.editor.project().active().video;
        DocumentSettings {
            name: self.settings.name.clone(),
            width: video.width,
            height: video.height,
            rate_num: video.rate_num,
            rate_den: video.rate_den,
        }
    }

    /// The active timeline's frame and rate.
    pub fn video(&self) -> VideoSettings {
        self.editor.project().active().video
    }

    /// Sets the active timeline's frame and rate, as an edit - undoable,
    /// and this timeline's alone. Anything a frame could not be is ignored
    /// by the command, for the reason a zero dimension is in
    /// [`Session::prepare_save`].
    pub fn set_video(&mut self, video: VideoSettings) -> Result<EditorView, String> {
        let timeline_id = self.editor.project().active_timeline_id.clone();
        self.apply(Command::SetTimelineVideo { timeline_id, video })
    }

    /// The edit as it stands.
    pub fn project(&self) -> &Project {
        self.editor.project()
    }

    /// Whether there is something to undo.
    pub fn can_undo(&self) -> bool {
        self.editor.can_undo()
    }

    /// Whether there is something to redo.
    pub fn can_redo(&self) -> bool {
        self.editor.can_redo()
    }

    /// The current state without changing anything.
    pub fn view(&self) -> EditorView {
        self.view_with(None)
    }

    fn view_with(&self, created_id: Option<String>) -> EditorView {
        let settings = self.settings();
        EditorView {
            project: self.editor.project().clone(),
            can_undo: self.editor.can_undo(),
            can_redo: self.editor.can_redo(),
            settings: SettingsView {
                name: settings.name,
                width: settings.width,
                height: settings.height,
                rate_num: settings.rate_num,
                rate_den: settings.rate_den,
            },
            created_id,
        }
    }

    /// Applies one edit command and returns the new state.
    pub fn apply(&mut self, command: Command) -> Result<EditorView, String> {
        self.apply_within(None, command)
    }

    /// Applies one edit command as a move of `gesture`, so that a knob
    /// dragged through many values is one undo step; see
    /// `concat_project::Editor::apply_within`. `None` is a step of its own.
    pub fn apply_within(
        &mut self,
        gesture: Option<&str>,
        command: Command,
    ) -> Result<EditorView, String> {
        let outcome = self
            .editor
            .apply_within(gesture, command)
            .map_err(|error| error.to_string())?;
        Ok(self.view_with(outcome.created_id))
    }

    /// Ends the gesture in progress: the next command naming it starts a
    /// new undo step.
    pub fn end_gesture(&mut self) {
        self.editor.end_gesture();
    }

    /// Steps the history back one edit.
    pub fn undo(&mut self) -> EditorView {
        self.editor.undo();
        self.view()
    }

    /// Steps the history forward one edit.
    pub fn redo(&mut self) -> EditorView {
        self.editor.redo();
        self.view()
    }

    /// Takes a new name, if there is one, and hands back what a save must
    /// write: the folder and the document. The frame is not a parameter any
    /// more - it is the active timeline's, set through [`Session::set_video`]
    /// as an edit. The disk write is the caller's, so it can happen off the
    /// thread that owns the session; [`Session::save`] does both.
    pub fn prepare_save(&mut self, name: Option<&str>) -> (String, serde_json::Value) {
        if std::mem::take(&mut self.converted) {
            keep_before_lanes(&self.path);
        }
        if let Some(name) = name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                self.settings.name = trimmed.to_owned();
            }
        }
        (self.path.clone(), self.editor.to_document(&self.settings))
    }

    /// Writes the session's document to its project folder.
    pub fn save(&mut self, name: Option<&str>) -> Result<(), String> {
        let (path, document) = self.prepare_save(name);
        projects::save(&path, &document)
    }

    /// The document as it would be saved.
    pub fn document(&self) -> serde_json::Value {
        self.editor.to_document(&self.settings)
    }

    /// A copy of this session to read from: the same folder, settings and
    /// project, without the history. What the API reads the window's
    /// project through in live mode - previews, exports, the document - on
    /// a thread of its own, while edits go to the window's session itself.
    pub fn snapshot(&self) -> Session {
        let editor = Editor::from_document(&self.document())
            .unwrap_or_else(|| Editor::with_video(self.settings.video()));
        Session {
            path: self.path.clone(),
            settings: self.settings.clone(),
            editor,
            upgraded: Vec::new(),
            retired: concat_project::RetiredFields::default(),
            converted: false,
        }
    }

    /// Takes `document` as the project's, with no history: the recovery
    /// copy an open offered back and was told to restore (Royalty Studio
    /// fork). The edits in it are unsaved until the next save.
    pub fn adopt_document(&mut self, document: &serde_json::Value) -> Result<(), String> {
        let mut editor = Editor::from_document(document)
            .ok_or_else(|| "the recovery copy holds no project this build can read".to_owned())?;
        editor.type_lanes();
        self.editor = editor;
        Ok(())
    }

    /// Whether the open converted the project to picture and sound lanes
    /// and the copy of it as it was has not been kept yet: what the window
    /// says as the project opens.
    pub fn converted(&self) -> bool {
        self.converted
    }

    /// The active timeline flattened for rendering. This is what export and
    /// preview consume: the engine flattens its own session, so the pixels
    /// rendered are the model's, never a copy of it.
    pub fn flattened_clips(&self) -> Vec<concat_export::ExportClip> {
        concat_export::flatten::flatten_timeline_in(
            self.editor.project(),
            None,
            Some(std::path::Path::new(&self.path)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> DocumentSettings {
        DocumentSettings {
            name: "Test".to_owned(),
            width: 1920,
            height: 1080,
            rate_num: 30,
            rate_den: 1,
        }
    }

    /// A project saved before lanes had kinds opens with picture and sound
    /// lanes, its sound on a lane of its own, and the first save keeps the
    /// document as it was beside the new one - once (Royalty Studio fork).
    #[test]
    fn an_old_project_opens_converted_and_keeps_its_old_document_once() {
        let scratch =
            std::env::temp_dir().join(format!("concat-session-lanes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let folder = scratch.to_string_lossy().into_owned();
        // The document an older build writes: lanes of no kind, the sound
        // inside the video.
        let mut old = Editor::new();
        let media = old
            .apply(Command::AddMedia {
                item: concat_project::commands::NewMedia {
                    path: "/clips/a.mp4".to_owned(),
                    name: "a.mp4".to_owned(),
                    duration: Some(8.0),
                    kind: concat_project::model::MediaKind::Video,
                    width: Some(1920),
                    height: Some(1080),
                    frame_rate: Some(30.0),
                    frame_rate_fraction: Some("30/1".to_owned()),
                    video_codec: Some("h264".to_owned()),
                    audio_codec: Some("aac".to_owned()),
                    has_audio: true,
                    audio_tracks: Vec::new(),
                    origin: None,
                    motion: None,
                    color_space: Default::default(),
                },
            })
            .expect("adds media")
            .created_id
            .expect("an id");
        old.apply(Command::AddClipAtFirstFree {
            media_id: media,
            start: 0.0,
        })
        .expect("places it");
        let before = old.to_document(&settings());
        projects::save(&folder, &before).expect("writes the old document");

        let mut session = Session::open(&folder, settings()).expect("opens");
        assert!(session.converted());
        let timeline = session.project().active();
        assert!(timeline.is_typed());
        assert_eq!(
            timeline.clips.len(),
            2,
            "the sound came out onto a lane of its own"
        );
        session.save(None).expect("saves");
        let kept = std::fs::read_to_string(scratch.join(BEFORE_LANES)).expect("kept");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&kept).expect("json"),
            before,
            "the copy is the document from before"
        );
        // A second open, now with kinds, converts nothing and keeps nothing more.
        let mut again = Session::open(&folder, settings()).expect("reopens");
        assert!(!again.converted());
        again.save(None).expect("saves again");
        let still = std::fs::read_to_string(scratch.join(BEFORE_LANES)).expect("kept");
        assert_eq!(still, kept);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_fresh_project_opens_empty_and_round_trips_a_save() {
        let scratch =
            std::env::temp_dir().join(format!("concat-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let info = projects::create(&scratch.to_string_lossy(), "Fresh", 1920, 1080, 30, 1)
            .expect("creates");

        let mut session = Session::open_info(&info).expect("opens the settings-only manifest");
        assert!(!session.can_undo());
        assert_eq!(
            (session.settings().width, session.settings().height),
            (1920, 1080),
            "the manifest's frame seeds the first timeline"
        );
        let view = session
            .apply(Command::AddTrack { bottom: false })
            .expect("adds a track");
        assert!(view.can_undo);
        session
            .set_video(VideoSettings {
                width: 1280,
                height: 720,
                rate_num: 25,
                rate_den: 1,
                color_space: Default::default(),
            })
            .expect("sets the frame");
        session.save(Some("Renamed")).expect("saves");

        let reopened = Session::open(&info.path, settings()).expect("reopens");
        assert_eq!(
            reopened.settings().name,
            "Test",
            "the manifest's name is what open gets"
        );
        assert_eq!(
            (reopened.settings().width, reopened.settings().height),
            (1280, 720),
            "the document's frame wins over the manifest's"
        );
        assert_eq!(reopened.settings().rate_num, 25, "and so does its rate");
        assert_eq!(
            reopened.project().active().tracks.len(),
            session.project().active().tracks.len()
        );

        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_corrupt_document_is_refused() {
        let scratch =
            std::env::temp_dir().join(format!("concat-corrupt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        std::fs::write(scratch.join("concat.json"), br#"{"timelines": "garbage"}"#)
            .expect("writes");
        assert!(Session::open(&scratch.to_string_lossy(), settings()).is_err());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_truncated_document_is_refused_rather_than_replaced() {
        let scratch =
            std::env::temp_dir().join(format!("concat-truncated-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        // A save cut short by a power cut or an outside tool: the file is
        // there, and it is not JSON. Opening it as an empty project would
        // let the next save replace whatever was edited.
        std::fs::write(
            scratch.join("concat.json"),
            br#"{"version": 1, "timelines": ["#,
        )
        .expect("writes");
        let opened = Session::open(&scratch.to_string_lossy(), settings());
        assert!(opened.is_err(), "a half document is not an empty project");
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
