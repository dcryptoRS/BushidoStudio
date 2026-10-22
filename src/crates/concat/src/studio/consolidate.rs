// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Selection consolidation: render first, then replace in one undo step.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_host::consolidate::Consolidation;
use concat_project::{consolidation_clips, why_not_merge};

use super::*;

pub(super) struct MergeJob {
    cancel: Arc<AtomicBool>,
    progress: f32,
}

/// Removes an unadopted render even when its project was closed before
/// the worker's callback arrived. Successful edits keep the generated media
/// through undo/redo and across cache clearing.
struct RenderedFile {
    folder: PathBuf,
    keep: bool,
}

impl Drop for RenderedFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(self.folder.join("merged.mp4"));
            let _ = std::fs::remove_dir(&self.folder);
        }
    }
}

/// What the document says of merging a selection, and when it said it.
pub(super) struct MergeChecked {
    revision: u64,
    selection: Vec<String>,
    rejoins: bool,
    refused: Option<String>,
}

impl Studio {
    pub fn merge_blocked(&self) -> Option<String> {
        if self.merge_job.is_some() {
            return None; // The same action cancels a running merge.
        }
        // Locks are the lanes' view, not the document: asked fresh.
        let locked = self.selection.iter().any(|id| {
            self.clip(id)
                .is_some_and(|clip| self.locked(&clip.track_id))
        });
        let (rejoins, refused) = self.merge_checks();
        if rejoins && !locked {
            return None;
        }
        if self.host.exporter.is_busy() {
            return Some(t("studio.mergeExportBusy"));
        }
        if locked {
            return Some(t("studio.mergeTrackLocked"));
        }
        refused
    }

    /// The document's half of [`Studio::merge_blocked`], made once per
    /// revision and selection.
    fn merge_checks(&self) -> (bool, Option<String>) {
        let mut memo = self.merge_checked.borrow_mut();
        let fresh = memo.as_ref().is_some_and(|checked| {
            checked.revision == self.revision && checked.selection == self.selection
        });
        if !fresh {
            let timeline = self.timeline();
            *memo = Some(MergeChecked {
                revision: self.revision,
                selection: self.selection.clone(),
                rejoins: rejoins_exactly(timeline, &self.selection),
                refused: consolidation_clips(timeline, &self.selection)
                    .err()
                    .map(|error| in_words(error.to_string())),
            });
        }
        let checked = memo.as_ref().expect("made above");
        (checked.rejoins, checked.refused.clone())
    }

    pub(super) fn merge_label(&self) -> String {
        match &self.merge_job {
            Some(job) => tf(
                "studio.mergeCancelProgress",
                &[&format!("{:.0}", job.progress * 100.0)],
            ),
            None => t("studio.mergeSelected"),
        }
    }

    pub(super) fn cancel_merge(&mut self) {
        if let Some(job) = self.merge_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn merge(&mut self) {
        if let Some(job) = &self.merge_job {
            job.cancel.store(true, Ordering::Relaxed);
            return;
        }
        self.flush_commit();
        if let Some(reason) = self.merge_blocked() {
            self.notify(&reason, true);
            return;
        }
        // Pieces of one clip cut apart and left as they were go back
        // together as they were: at once, one undo step, no render and no
        // generation lost - sound pieces too, which a render does not take
        // (audit 2026-10-15, M5). Anything else is rendered as before.
        if rejoins_exactly(self.timeline(), &self.selection) {
            self.apply(Command::MergeClips {
                clip_ids: self.selection.clone(),
            });
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let plan = match Consolidation::new(session.project(), &self.selection) {
            Ok(plan) => plan,
            Err(error) => {
                self.notify(&in_words(error), true);
                return;
            }
        };
        let job = match self.host.exporter.begin() {
            Ok(job) => job,
            Err(error) => {
                self.notify(&error, true);
                return;
            }
        };
        let root = PathBuf::from(session.path());
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let folder = root.join("media").join(format!("merged-{stamp}"));
        if let Err(error) =
            std::fs::create_dir_all(root.join("media")).and_then(|()| std::fs::create_dir(&folder))
        {
            self.notify(&tf("studio.mergeFailed", &[&error.to_string()]), true);
            return;
        }
        let rendered = RenderedFile {
            folder: folder.clone(),
            keep: false,
        };
        let output = folder.join("merged.mp4");
        let request = plan.request(session.project(), &root, &output);
        let compositor = self.host.monitor.off_thread();
        self.merge_job = Some(MergeJob {
            cancel: job.cancel_handle(),
            progress: 0.0,
        });
        self.pause();
        self.notify(&t("studio.mergeStarted"), false);
        let epoch = crate::host::project_epoch();
        spawn_in_project(
            move || {
                let mut last = -1.0;
                let result = concat_host::export::run_on(
                    &request,
                    compositor,
                    job.cancel_flag(),
                    |progress| {
                        let fraction =
                            (progress.frame as f32 / progress.total.max(1) as f32).clamp(0.0, 1.0);
                        if fraction - last >= 0.02 {
                            last = fraction;
                            on_ui_in_project(epoch, move |studio, _, _| {
                                if let Some(job) = studio.merge_job.as_mut() {
                                    job.progress = fraction;
                                }
                            });
                        }
                    },
                );
                let cancel = job.cancel_handle();
                (plan, rendered, output, cancel, result)
            },
            |studio, _, _, (plan, mut rendered, output, cancel, result)| {
                studio.merge_job = None;
                if cancel.load(Ordering::Relaxed) {
                    studio.notify(&t("studio.mergeCancelled"), false);
                    return;
                }
                let outcome = result.and_then(|_| {
                    let summary = media::probe(&output.to_string_lossy())?;
                    let mut item = summary.to_new_media();
                    item.name = t("studio.mergedClips");
                    item.origin = Some(model::MediaOrigin::Processed);
                    studio.flush_commit();
                    let command = plan.replacement(studio.project(), item)?;
                    // Locking a lane while rendering must also prevent adoption.
                    if studio.locked(plan.track_id()) {
                        return Err(t("studio.mergeTrackLocked"));
                    }
                    studio
                        .apply(command)
                        .ok_or_else(|| t("studio.mergeCouldNotReplace"))
                });
                match outcome {
                    Ok(id) => {
                        rendered.keep = true;
                        studio.selection = vec![id];
                        studio.transition_selected = None;
                        studio.request_media_art();
                        studio.request_preview();
                        studio.notify(&t("studio.mergeFinished"), false);
                    }
                    Err(error) => {
                        studio.notify(&tf("studio.mergeFailed", &[&in_words(error)]), true)
                    }
                }
            },
        );
    }
}

/// A merge refusal in the person's language. The engine words them in
/// English, for the API and the log as much as for the window, so the
/// window translates the ones it knows and passes the rest as they came.
fn in_words(reason: String) -> String {
    match reason.as_str() {
        "Select two or more distinct video clips to merge." => t("studio.mergeWhySelectVideo"),
        "A selected clip no longer exists." => t("studio.mergeWhyClipGone"),
        "Select video clips on the same track to merge." => t("studio.mergeWhySameTrack"),
        "Overlapping clips cannot be merged." => t("studio.mergeWhyOverlap"),
        "Reattach detached audio before merging its video clips." => t("studio.mergeWhyDetached"),
        "Select every clip inside the span to merge." => t("studio.mergeWhyWholeSpan"),
        "Align clip boundaries to the timeline frame grid before merging." => {
            t("studio.mergeWhyOffGrid")
        }
        "Clips above other visible content cannot be merged into an opaque video." => {
            t("studio.mergeWhyCovering")
        }
        "Include both sides of a transition in the merge." => t("studio.mergeWhyTransition"),
        "That track no longer exists." => t("studio.mergeWhyTrackGone"),
        "A selected clip's media is missing." => t("studio.mergeWhyMediaMissing"),
        "The selected clips changed while merging. Please try again." => {
            t("studio.mergeWhyChanged")
        }
        _ => reason,
    }
}

/// Whether `ids` are pieces of one clip cut apart and left as they were, so
/// that the engine's lossless rejoin ([`Command::MergeClips`]) gives back
/// exactly the clip they were cut from: one file in source order (what
/// [`why_not_merge`] checks), the same look, sound and placement on every
/// piece, and nothing at the joins - no transition, no fade - that a rejoin
/// would drop. Keys may differ: the rejoin keeps every piece's where it was.
fn rejoins_exactly(timeline: &model::Timeline, ids: &[String]) -> bool {
    if why_not_merge(timeline, ids).is_some() {
        return false;
    }
    let mut clips: Vec<&model::Clip> = ids.iter().filter_map(|id| timeline.clip(id)).collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    let last = clips.len().saturating_sub(1);
    let joins_clean = clips.iter().enumerate().all(|(index, clip)| {
        (index == 0 || (clip.transition_in.is_none() && clip.fade_in == 0.0))
            && (index == last || clip.fade_out == 0.0)
    });
    let as_cut = |clip: &model::Clip| {
        let mut clip = clip.clone();
        clip.id.clear();
        clip.name.clear();
        clip.start = 0.0;
        clip.duration = 0.0;
        clip.source_start = 0.0;
        clip.fade_in = 0.0;
        clip.fade_out = 0.0;
        clip.transition_in = None;
        clip.keys.clear();
        clip
    };
    joins_clean
        && clips
            .windows(2)
            .all(|pair| as_cut(pair[0]) == as_cut(pair[1]))
}

/// The lossless rejoin's test, apart from the render's above.
#[cfg(test)]
mod words_tests {
    use super::in_words;

    /// Every sentence the window translates is one the engine still says,
    /// and in English comes back as it went in: a reworded refusal in the
    /// engine would otherwise slip past the table in every other language.
    #[test]
    fn the_merge_refusals_translated_are_the_engines_own() {
        let engine = [
            include_str!("../../../concat-project/src/commands/clips.rs"),
            include_str!("../../../concat-project/src/commands/mod.rs"),
            include_str!("../../../concat-host/src/consolidate.rs"),
        ]
        .concat();
        for sentence in [
            "Select two or more distinct video clips to merge.",
            "A selected clip no longer exists.",
            "Select video clips on the same track to merge.",
            "Overlapping clips cannot be merged.",
            "Reattach detached audio before merging its video clips.",
            "Select every clip inside the span to merge.",
            "Align clip boundaries to the timeline frame grid before merging.",
            "Clips above other visible content cannot be merged into an opaque video.",
            "Include both sides of a transition in the merge.",
            "That track no longer exists.",
            "A selected clip's media is missing.",
            "The selected clips changed while merging. Please try again.",
        ] {
            assert!(
                engine.contains(sentence),
                "the engine no longer says {sentence:?}"
            );
            assert_eq!(in_words(sentence.to_owned()), sentence);
        }
    }
}

#[cfg(test)]
mod rejoin_tests {
    use super::rejoins_exactly;
    use concat_project::commands::{ClipPatch, NewMedia};
    use concat_project::model::MediaKind;
    use concat_project::{Command, Editor};

    /// A ten-second clip split at four seconds: the two pieces' ids.
    fn split() -> (Editor, Vec<String>) {
        let mut editor = Editor::new();
        let media_id = editor
            .apply(Command::AddMedia {
                item: NewMedia {
                    path: "/a.mp4".to_owned(),
                    name: "a.mp4".to_owned(),
                    duration: Some(10.0),
                    kind: MediaKind::Video,
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
            .expect("adds")
            .created_id
            .expect("id");
        let track_id = editor.project().active().tracks[0].id.clone();
        let clip_id = editor
            .apply(Command::AddClip {
                media_id,
                track_id,
                start: 0.0,
                ripple: false,
            })
            .expect("places")
            .created_id
            .expect("id");
        editor
            .apply(Command::SplitClips {
                clip_ids: vec![clip_id],
                time: 4.0,
            })
            .expect("splits");
        let ids = editor
            .project()
            .active()
            .clips
            .iter()
            .map(|clip| clip.id.clone())
            .collect();
        (editor, ids)
    }

    #[test]
    fn untouched_pieces_rejoin_and_touched_ones_are_rendered() {
        let (mut editor, ids) = split();
        assert_eq!(ids.len(), 2, "the split made two pieces");
        assert!(rejoins_exactly(editor.project().active(), &ids));

        // A piece turned down is no longer the clip it was cut from: a
        // lossless rejoin would take the first piece's level for both.
        editor
            .apply(Command::UpdateClip {
                clip_id: ids[1].clone(),
                patch: ClipPatch {
                    volume: Some(0.5),
                    ..ClipPatch::default()
                },
            })
            .expect("turns it down");
        assert!(!rejoins_exactly(editor.project().active(), &ids));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unadopted_render_is_removed_but_adopted_media_survives() {
        let folder =
            std::env::temp_dir().join(format!("concat-merge-cleanup-{}", std::process::id()));
        std::fs::create_dir(&folder).unwrap();
        let output = folder.join("merged.mp4");
        std::fs::write(&output, b"rendered source").unwrap();
        drop(RenderedFile {
            folder: folder.clone(),
            keep: false,
        });
        assert!(
            !folder.exists(),
            "cancel, failure and stale delivery clean up the source"
        );
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(&output, b"adopted source").unwrap();
        drop(RenderedFile {
            folder: folder.clone(),
            keep: true,
        });
        assert!(output.is_file(), "undo/redo still needs adopted media");
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(folder).unwrap();
    }
}
