// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Timelines: the tabs a project holds, their frames, names, order and which one is showing.
//!
//! One arm per command, exactly as [`super::apply`] routes them here;
//! everything these arms share lives in the parent module.

use std::collections::HashMap;

use super::*;
use crate::model::{ClipKey, KEY_EPSILON};

/// Applies one of this module's commands. Any other is a routing error.
pub(super) fn apply(
    project: &mut Project,
    mint: &mut IdMint,
    command: Command,
) -> Result<Outcome, CommandError> {
    match command {
        Command::AddTimeline => {
            let id = mint.next("tl");
            let name = next_numbered(
                "Timeline",
                project
                    .timelines
                    .iter()
                    .map(|timeline| timeline.name.clone()),
            );
            // Two sound lanes under two picture lanes when the timeline you
            // were looking at has lanes with kinds; four of any kind when not.
            let typed = project.active().is_typed();
            let tracks = (0..4)
                .map(|row| {
                    let kind = match (typed, row < 2) {
                        (false, _) => TrackKind::Any,
                        (true, true) => TrackKind::Audio,
                        (true, false) => TrackKind::Video,
                    };
                    Track::of_kind(mint.next("t"), kind)
                })
                .collect();
            // Born at the frame of the timeline you were looking at: the
            // likeliest second timeline is another cut of the same picture,
            // and the one that is not is a sheet away from being told so.
            let video = project.active().video;
            // And with its background, for the same reason: a second cut of
            // a vertical edit wants the same bars filled the same way.
            let background = project.active().background.clone();
            project.timelines.push(Arc::new(Timeline {
                id: id.clone(),
                name,
                video,
                background,
                tracks,
                clips: Vec::new(),
                markers: Vec::new(),
                extra: Default::default(),
            }));
            project.active_timeline_id = id.clone();
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::ReframeTimeline {
            timeline_id,
            name,
            video,
            follow,
        } => {
            if !video.is_sane() {
                return Ok(Outcome::default());
            }
            let Some(source) = project
                .timelines
                .iter()
                .find(|timeline| timeline.id == timeline_id)
                .cloned()
            else {
                return Ok(Outcome::default());
            };
            let id = mint.next("tl");
            let (mut copy, renamed) = copied(&source, mint);
            copy.id = id.clone();
            copy.name = match name.trim() {
                "" => source.name.clone(),
                name => name.to_owned(),
            };
            copy.video = video;
            let frame = (f64::from(video.width), f64::from(video.height));
            for clip in &mut copy.clips {
                if !matches!(clip.kind, ClipKind::Video | ClipKind::Image) {
                    continue;
                }
                let Some((Some(width), Some(height))) = project
                    .media_by_id(&clip.media_id)
                    .map(|media| (media.width, media.height))
                else {
                    continue;
                };
                let path = follow
                    .iter()
                    .find(|path| renamed.get(&path.clip_id) == Some(&clip.id))
                    .map(|path| path.points.as_slice());
                cover(
                    Arc::make_mut(clip),
                    (f64::from(width), f64::from(height)),
                    frame,
                    path,
                );
            }
            project.timelines.push(Arc::new(copy));
            project.active_timeline_id = id.clone();
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }

        Command::SetTimelineVideo { timeline_id, video } => {
            if !video.is_sane() {
                return Ok(Outcome::default());
            }
            let applied = project
                .timelines
                .iter_mut()
                .find(|timeline| timeline.id == timeline_id)
                .is_some_and(|timeline| assign(&mut Arc::make_mut(timeline).video, video));
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SetTimelineBackground {
            timeline_id,
            background,
        } => {
            let applied = project
                .timelines
                .iter_mut()
                .find(|timeline| timeline.id == timeline_id)
                .is_some_and(|timeline| {
                    assign(&mut Arc::make_mut(timeline).background, background.tidy())
                });
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::RemoveTimeline { timeline_id } => {
            if project.timelines.len() <= 1 {
                return Err(CommandError::LastTimeline);
            }
            let Some(index) = project
                .timelines
                .iter()
                .position(|timeline| timeline.id == timeline_id)
            else {
                return Ok(Outcome::default());
            };
            if project.active_timeline_id == timeline_id {
                let neighbour = if index + 1 < project.timelines.len() {
                    index + 1
                } else {
                    index - 1
                };
                project.active_timeline_id = project.timelines[neighbour].id.clone();
            }
            project.timelines.remove(index);
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::RenameTimeline { timeline_id, name } => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Ok(Outcome::default());
            }
            let applied = project
                .timelines
                .iter_mut()
                .find(|timeline| timeline.id == timeline_id)
                .is_some_and(|timeline| {
                    assign(&mut Arc::make_mut(timeline).name, trimmed.to_owned())
                });
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::SelectTimeline { timeline_id } => {
            // Short-circuiting is right here: an unknown id must not touch
            // the selection at all.
            let applied = project
                .timelines
                .iter()
                .any(|timeline| timeline.id == timeline_id)
                && assign(&mut project.active_timeline_id, timeline_id);
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        Command::MoveTimeline { timeline_id, index } => {
            let Some(from) = project
                .timelines
                .iter()
                .position(|timeline| timeline.id == timeline_id)
            else {
                return Ok(Outcome::default());
            };
            let to = index.min(project.timelines.len() - 1);
            if to == from {
                return Ok(Outcome::default());
            }
            let timeline = project.timelines.remove(from);
            project.timelines.insert(to, timeline);
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        _ => unreachable!("commands::apply routes only this module's commands here"),
    }
}

/// `timeline` copied under fresh ids (Royalty Studio fork): its lanes, its
/// clips - their lanes, links and the clips they were detached from
/// following them - and its markers. With each old clip id against its new.
fn copied(timeline: &Timeline, mint: &mut IdMint) -> (Timeline, HashMap<String, String>) {
    let lanes: HashMap<String, String> = timeline
        .tracks
        .iter()
        .map(|track| (track.id.clone(), mint.next("t")))
        .collect();
    let clips: HashMap<String, String> = timeline
        .clips
        .iter()
        .map(|clip| (clip.id.clone(), mint.next("c")))
        .collect();
    let mut links: HashMap<String, String> = HashMap::new();
    let mut copy = timeline.clone();
    copy.tracks = timeline
        .tracks
        .iter()
        .map(|track| Track {
            id: lanes[&track.id].clone(),
            ..track.clone()
        })
        .collect();
    copy.clips = timeline
        .clips
        .iter()
        .map(|clip| {
            let mut fresh = (**clip).clone();
            fresh.id = clips[&clip.id].clone();
            if let Some(lane) = lanes.get(&clip.track_id) {
                fresh.track_id = lane.clone();
            }
            fresh.link = clip.link.as_ref().map(|link| {
                links
                    .entry(link.clone())
                    .or_insert_with(|| mint.next("l"))
                    .clone()
            });
            fresh.detached_from = clip
                .detached_from
                .as_ref()
                .map(|from| clips.get(from).cloned().unwrap_or_else(|| from.clone()));
            Arc::new(fresh)
        })
        .collect();
    copy.markers = timeline
        .markers
        .iter()
        .map(|marker| Marker {
            id: mint.next("k"),
            ..marker.clone()
        })
        .collect();
    (copy, clips)
}

/// A picture of `size` pixels made to cover a frame of `frame` rather than
/// fit inside it (Royalty Studio fork; see [`Command::ReframeTimeline`]):
/// its scale and its scale's keys grown by as much as it takes, and moved
/// across the frame - keyed, where `path` says where its subject is as it
/// plays - so that what matters stays in view and no edge of it shows.
fn cover(clip: &mut Clip, size: (f64, f64), frame: (f64, f64), path: Option<&[[f64; 2]]>) {
    let (kept_w, kept_h) = match clip.crop {
        Some(crop) => (
            size.0 * (1.0 - crop.left - crop.right).max(0.1),
            size.1 * (1.0 - crop.top - crop.bottom).max(0.1),
        ),
        None => size,
    };
    if kept_w <= 0.0 || kept_h <= 0.0 || frame.0 <= 0.0 || frame.1 <= 0.0 {
        return;
    }
    let (source, target) = (kept_w / kept_h, frame.0 / frame.1);
    let grow = (source / target).max(target / source);
    clip.scale *= grow;
    for key in &mut clip.keys {
        if key.property == KeyProperty::Scale {
            key.value *= grow;
        }
    }
    // How many frames across and down the picture is drawn, and so how far
    // it may move before an edge of it shows.
    let fit = (frame.0 / kept_w).min(frame.1 / kept_h);
    let wide = clip.scale * clip.stretch_x * kept_w * fit / frame.0;
    let high = clip.scale * clip.stretch_y * kept_h * fit / frame.1;
    let reach_x = ((wide - 1.0) / 2.0).max(0.0);
    let reach_y = ((high - 1.0) / 2.0).max(0.0);
    // The subject at `x` of the picture in the frame's middle; a mirrored
    // picture shows its left on the right.
    let mirrored = clip.flip_h;
    let across = |x: f64| {
        let x = if mirrored { 1.0 - x } else { x };
        (-(x - 0.5) * wide).clamp(-reach_x, reach_x)
    };
    clip.keys.retain(|key| key.property != KeyProperty::OffsetX);
    let mut points: Vec<[f64; 2]> = path
        .unwrap_or_default()
        .iter()
        .filter(|point| point[0].is_finite() && point[1].is_finite())
        .map(|point| [point[0].clamp(0.0, 1.0), point[1].clamp(0.0, 1.0)])
        .collect();
    points.sort_by(|a, b| a[0].total_cmp(&b[0]));
    points.dedup_by(|later, earlier| (later[0] - earlier[0]).abs() < KEY_EPSILON);
    clip.offset_x = points.first().map_or(0.0, |first| across(first[1]));
    if points.len() > 1 {
        clip.keys.extend(points.iter().map(|[at, x]| ClipKey {
            property: KeyProperty::OffsetX,
            at: *at,
            value: across(*x),
            ease: KeyEase::IN_OUT,
        }));
    }
    clip.offset_y = clip.offset_y.clamp(-reach_y, reach_y);
    for key in &mut clip.keys {
        if key.property == KeyProperty::OffsetY {
            key.value = key.value.clamp(-reach_y, reach_y);
        }
    }
    clip.sort_keys();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    /// A new project's editor, on lanes with kinds, holding a ten-second
    /// video with sound.
    fn editor() -> (Editor, String) {
        let mut editor = Editor::new();
        assert!(editor.type_lanes());
        let media = editor
            .apply(Command::AddMedia {
                item: NewMedia {
                    path: "/clip.mp4".to_owned(),
                    name: "clip".to_owned(),
                    duration: Some(10.0),
                    kind: MediaKind::Video,
                    width: Some(1920),
                    height: Some(1080),
                    frame_rate: Some(30.0),
                    frame_rate_fraction: Some("30/1".to_owned()),
                    video_codec: None,
                    audio_codec: Some("aac".to_owned()),
                    has_audio: true,
                    audio_tracks: Vec::new(),
                    origin: None,
                    motion: None,
                    color_space: Default::default(),
                },
            })
            .expect("imports")
            .created_id
            .expect("an id");
        (editor, media)
    }

    #[test]
    fn a_vertical_copy_covers_its_frame_and_follows_the_subject() {
        let (mut editor, media) = editor();
        let placed = editor
            .apply(Command::PlaceRange {
                media_id: media,
                track_id: None,
                start: 0.0,
                source_in: 0.0,
                source_out: 4.0,
                overwrite: false,
            })
            .expect("places")
            .created_id
            .expect("an id");
        let source = editor.project().active().id.clone();
        let video = VideoSettings {
            width: 1080,
            height: 1920,
            ..editor.project().active().video
        };
        let made = editor
            .apply(Command::ReframeTimeline {
                timeline_id: source.clone(),
                name: "Vertical".to_owned(),
                video,
                follow: vec![Follow {
                    clip_id: placed.clone(),
                    points: vec![[0.0, 0.5], [1.0, 0.8]],
                }],
            })
            .expect("copies");
        let copy = editor.project().active().clone();
        assert_eq!(made.created_id.as_deref(), Some(copy.id.as_str()));
        assert_eq!(
            (copy.video.width, copy.video.height, copy.name.as_str()),
            (1080, 1920, "Vertical")
        );
        let original = editor
            .project()
            .timelines
            .iter()
            .find(|timeline| timeline.id == source)
            .expect("the original")
            .clone();
        assert_eq!(original.clip(&placed).expect("as it was").scale, 1.0);
        // Sixteen by nine covering nine by sixteen: its fit times 3.16.
        let picture = copy
            .clips
            .iter()
            .find(|clip| clip.kind == ClipKind::Video)
            .expect("the picture");
        assert_ne!(picture.id, placed);
        let wide = (16.0 / 9.0) / (9.0 / 16.0);
        assert!((picture.scale - wide).abs() < 1e-9, "{}", picture.scale);
        // Centred at the start; at the end the subject, at 0.8 of the
        // picture, brought to the middle - the picture moved left.
        let keys: Vec<&ClipKey> = picture
            .keys
            .iter()
            .filter(|key| key.property == KeyProperty::OffsetX)
            .collect();
        assert_eq!(keys.len(), 2);
        assert!(keys[0].value.abs() < 1e-9);
        assert!(
            (keys[1].value + 0.3 * wide).abs() < 1e-9,
            "{}",
            keys[1].value
        );
        // Never so far that an edge shows.
        assert!(keys[1].value >= -(wide - 1.0) / 2.0);
        // Its sound came with it, linked to it under a link of its own.
        let sound = copy
            .clips
            .iter()
            .find(|clip| clip.kind == ClipKind::Audio)
            .expect("the sound");
        assert!(sound.link.is_some() && sound.link == picture.link);
        assert_ne!(
            sound.link,
            original.clip(&placed).expect("the original").link
        );
        assert!(
            copy.tracks
                .iter()
                .all(|lane| original.tracks.iter().all(|old| old.id != lane.id))
        );
        // One step: undone, the project has its one timeline again.
        assert_eq!(editor.project().timelines.len(), 2);
        editor.undo();
        assert_eq!(editor.project().timelines.len(), 1);
    }

    #[test]
    fn a_picture_with_nowhere_to_follow_is_centred() {
        let (mut editor, media) = editor();
        editor
            .apply(Command::PlaceRange {
                media_id: media,
                track_id: None,
                start: 0.0,
                source_in: 0.0,
                source_out: 2.0,
                overwrite: false,
            })
            .expect("places");
        let source = editor.project().active().id.clone();
        let video = VideoSettings {
            width: 1080,
            height: 1920,
            ..editor.project().active().video
        };
        editor
            .apply(Command::ReframeTimeline {
                timeline_id: source,
                name: String::new(),
                video,
                follow: Vec::new(),
            })
            .expect("copies");
        let picture = editor
            .project()
            .active()
            .clips
            .iter()
            .find(|clip| clip.kind == ClipKind::Video)
            .cloned()
            .expect("the picture");
        assert_eq!(picture.offset_x, 0.0);
        assert!(picture.keys.is_empty());
        assert_eq!(editor.project().active().name, "Timeline 1");
    }
}
