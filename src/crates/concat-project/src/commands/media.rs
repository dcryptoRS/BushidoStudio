// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The bin and the fonts: what a project can place, before anything is placed.
//!
//! One arm per command, exactly as [`super::apply`] routes them here;
//! everything these arms share lives in the parent module.

use super::*;

/// How near a clip's in-point and length must be to the render's for the
/// clip to count as showing the whole of it, in seconds: well under a frame.
const WHOLE_TOLERANCE: f64 = 1e-3;

/// Applies one of this module's commands. Any other is a routing error.
pub(super) fn apply(
    project: &mut Project,
    mint: &mut IdMint,
    command: Command,
) -> Result<Outcome, CommandError> {
    match command {
        Command::AddMedia { item } => {
            if project
                .media
                .iter()
                .any(|existing| existing.path == item.path)
            {
                return Ok(Outcome::default());
            }
            let id = mint.next("m");
            project.media.push(MediaItem {
                id: id.clone(),
                path: item.path,
                name: item.name,
                duration: item.duration,
                kind: item.kind,
                width: item.width,
                height: item.height,
                frame_rate: item.frame_rate,
                frame_rate_fraction: item.frame_rate_fraction,
                video_codec: item.video_codec,
                audio_codec: item.audio_codec,
                has_audio: item.has_audio,
                audio_tracks: item.audio_tracks,
                origin: item.origin,
                motion: item.motion,
                placeholder: false,
                color_range: None,
                color_space: item.color_space,
                extra: Default::default(),
            });
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::SetMediaPlaceholder {
            media_id,
            placeholder,
        } => {
            let applied = project
                .media
                .iter_mut()
                .find(|item| item.id == media_id)
                .is_some_and(|item| assign(&mut item.placeholder, placeholder));
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::FillSlot { media_id, item } => {
            let media = project
                .media
                .iter_mut()
                .find(|existing| existing.id == media_id)
                .ok_or(CommandError::SlotGone)?;
            if !media.placeholder {
                return Err(CommandError::NotASlot);
            }

            // The slot keeps its id, so every clip that references it keeps
            // working; only the identity behind the id changes.
            media.path = item.path;
            media.name = item.name.clone();
            media.duration = item.duration;
            media.kind = item.kind;
            media.width = item.width;
            media.height = item.height;
            media.frame_rate = item.frame_rate;
            media.frame_rate_fraction = item.frame_rate_fraction;
            media.video_codec = item.video_codec;
            media.audio_codec = item.audio_codec;
            media.has_audio = item.has_audio;
            media.audio_tracks = item.audio_tracks;
            media.color_space = item.color_space;
            media.placeholder = false;
            let kind = match item.kind {
                MediaKind::Video => ClipKind::Video,
                MediaKind::Audio => ClipKind::Audio,
                MediaKind::Image => ClipKind::Image,
            };

            // Slot timing is the template's: start, duration and speed stay
            // put, which is what keeps cuts on the beat. The in-point resets
            // because it referred to the old footage; a clip shorter than its
            // slot freeze-frames on its last frame downstream, which is the
            // renderer's existing behaviour for a trim past the media's end.
            // All timelines, like RemoveMedia: slots are not per-timeline.
            for timeline in project.timelines.iter_mut() {
                // A timeline with no clip of this media stays the
                // snapshot's.
                if !timeline.clips.iter().any(|clip| clip.media_id == media_id) {
                    continue;
                }
                let timeline = Arc::make_mut(timeline);
                for clip in timeline.clips_where(|clip| clip.media_id == media_id) {
                    {
                        clip.source_start = 0.0;
                        clip.kind = kind;
                        clip.name = item.name.clone();
                        // A stream index named against the old file means
                        // nothing against the new one.
                        clip.audio_stream = None;
                    }
                }
            }
            // Filling always changes the project: the target was a
            // placeholder and is one no longer.
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::ReplaceMotion { media_id, item } => {
            let media = project
                .media
                .iter_mut()
                .find(|existing| existing.id == media_id)
                .ok_or(CommandError::MediaGone)?;
            let before = media.clone();
            let old_length = media.duration;
            // The id stays, so every clip of it stays placed; what is behind
            // the id is the new render. The bin name stays the user's: each
            // render's file name is stamped, and a rename is theirs to keep.
            media.path = item.path;
            media.duration = item.duration;
            media.kind = item.kind;
            media.width = item.width;
            media.height = item.height;
            media.frame_rate = item.frame_rate;
            media.frame_rate_fraction = item.frame_rate_fraction;
            media.video_codec = item.video_codec;
            media.audio_codec = item.audio_codec;
            media.has_audio = item.has_audio;
            media.audio_tracks = item.audio_tracks;
            media.color_space = item.color_space;
            media.origin = Some(MediaOrigin::Motion);
            if item.motion.is_some() {
                media.motion = item.motion;
            }
            let mut applied = *media != before;
            let new_length = media.duration;
            let kind = match media.kind {
                MediaKind::Video => ClipKind::Video,
                MediaKind::Audio => ClipKind::Audio,
                MediaKind::Image => ClipKind::Image,
            };

            // All timelines, as a fill is: the bin is the project's.
            for timeline in project.timelines.iter_mut() {
                if !timeline.clips.iter().any(|clip| clip.media_id == media_id) {
                    continue;
                }
                let timeline = Arc::make_mut(timeline);
                let ids: Vec<String> = timeline
                    .clips
                    .iter()
                    .filter(|clip| clip.media_id == media_id)
                    .map(|clip| clip.id.clone())
                    .collect();
                for id in ids {
                    let (_, next) = timeline.room_around(&id).unwrap_or((0.0, f64::INFINITY));
                    let Some(clip) = timeline.clip_mut(&id) else {
                        continue;
                    };
                    applied |= assign(&mut clip.kind, kind);
                    // A clip whose speed curves keeps its cut: its length
                    // in source seconds is the curve's, not one division.
                    let (Some(new_length), None) = (new_length, &clip.speed_curve) else {
                        continue;
                    };
                    let speed = clip.speed;
                    // The whole render, as clip_add places it, follows the
                    // new one's length; a cut from it keeps its in- and
                    // out-points as far as the new length allows.
                    let whole = old_length.is_some_and(|old| {
                        clip.source_start <= WHOLE_TOLERANCE
                            && (clip.duration * speed - old).abs() <= WHOLE_TOLERANCE
                    });
                    let source_start = clip
                        .source_start
                        .min((new_length - MIN_CLIP_DURATION * speed).max(0.0));
                    let room = (new_length - source_start) / speed;
                    let duration = if whole { room } else { clip.duration.min(room) }
                        .min(next - clip.start)
                        .max(MIN_CLIP_DURATION);
                    let old = clip.duration;
                    let changed = assign(&mut clip.source_start, source_start)
                        | assign(&mut clip.duration, duration);
                    if changed {
                        clip.rewindow_keys(old, 0.0, duration);
                        applied = true;
                    }
                }
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::RemoveMedia { media_id } => {
            // All timelines, not just the active one: a shelved clip whose
            // media is gone would linger as a dead reference.
            let media_count = project.media.len();
            project.media.retain(|item| item.id != media_id);
            let mut applied = project.media.len() != media_count;
            for timeline in project.timelines.iter_mut().map(Arc::make_mut) {
                let clip_count = timeline.clips.len();
                timeline.clips.retain(|clip| clip.media_id != media_id);
                applied |= timeline.clips.len() != clip_count;
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::AddFont { family, path } => {
            if project.fonts.iter().any(|font| font.path == path) {
                return Ok(Outcome::default());
            }
            project.fonts.push(CustomFont { family, path });
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::RemoveFont { family } => {
            // Clips keep the family name: the face may come back when the
            // file does.
            let font_count = project.fonts.len();
            project.fonts.retain(|font| font.family != family);
            let applied = project.fonts.len() != font_count;
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::UpdateMediaPath { media_id, new_path } => {
            let applied = project
                .media
                .iter_mut()
                .find(|item| item.id == media_id)
                .is_some_and(|item| assign(&mut item.path, new_path));
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SetMediaColorRange { media_id, range } => {
            let applied = project
                .media
                .iter_mut()
                .find(|item| item.id == media_id)
                .is_some_and(|item| assign(&mut item.color_range, range));
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        _ => unreachable!("commands::apply routes only this module's commands here"),
    }
}
