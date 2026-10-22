// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Taking a video's sound out onto its own lane, and putting it back.
//!
//! One arm per command, exactly as [`super::apply`] routes them here;
//! everything these arms share lives in the parent module.

use super::links;
use super::*;

/// Applies one of this module's commands. Any other is a routing error.
pub(super) fn apply(
    project: &mut Project,
    mint: &mut IdMint,
    command: Command,
) -> Result<Outcome, CommandError> {
    match command {
        Command::DetachAudio { clip_id } => {
            let index = links::active_index(project);
            let first_sound = links::detach_sound(project, index, mint, &clip_id, None);
            Ok(Outcome {
                applied: first_sound.is_some(),
                created_id: first_sound,
            })
        }

        Command::ReattachAudio { clip_id } => {
            let timeline = project.active_mut();
            let Some(clip) = timeline.clip(&clip_id) else {
                return Ok(Outcome::default());
            };
            let video_id = match (&clip.kind, &clip.detached_from) {
                (ClipKind::Audio, Some(source)) => source.clone(),
                _ => clip.id.clone(),
            };
            if timeline.clip(&video_id).is_none() {
                return Ok(Outcome::default());
            }
            let sounds: Vec<Clip> = timeline
                .clips
                .iter()
                .filter(|other| other.detached_from.as_deref() == Some(video_id.as_str()))
                .map(|other| Clip::clone(other))
                .collect();
            if sounds.is_empty() {
                return Ok(Outcome::default());
            }
            // A set, not a Vec: the retain below tests every clip on the
            // timeline against it.
            let doomed: HashSet<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
            timeline.clips.retain(|other| !doomed.contains(&other.id));
            // The picture keeps its link only while something else shares it.
            let shared = video_link(timeline, &video_id);
            let video = timeline.clip_mut(&video_id).expect("checked above");
            video.muted = None;
            video.filters = sounds[0].filters.clone();
            if !shared {
                video.link = None;
            }
            // Reaching here means at least one sound clip was deleted.
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        _ => unreachable!("commands::apply routes only this module's commands here"),
    }
}

/// Whether a clip other than `clip_id` shares its link.
fn video_link(timeline: &Timeline, clip_id: &str) -> bool {
    !timeline.partners(clip_id).is_empty()
}
