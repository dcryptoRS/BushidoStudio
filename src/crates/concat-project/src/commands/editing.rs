// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! The editor's craft verbs (Royalty Studio fork): markers, the roll, the
//! slip and the slide, the three-point insert and overwrite, and the lift.
//! What a cutting room does to a cut that is already on the timeline, as
//! Premiere, Resolve and Final Cut do it.
//!
//! Every one keeps to what the files have: a roll, a slip or a slide never
//! shows a frame before a file starts or after it ends, and a picture's
//! sound comes along wherever the two are linked, so the cut stays in sync.

use std::collections::HashMap;

use super::*;
use super::{clips, links};

/// How close two edges have to be to touch.
const TOUCH: f64 = crate::placement::TOUCH * 10.0;

/// Applies one of this module's commands. Any other is a routing error.
pub(super) fn apply(
    project: &mut Project,
    mint: &mut IdMint,
    command: Command,
    scope: Scope,
) -> Result<Outcome, CommandError> {
    let applied = |applied: bool| Outcome {
        created_id: None,
        applied,
    };
    match command {
        Command::AddMarker { time, name, color } => {
            let id = mint.next("k");
            let timeline = project.active_mut();
            timeline.markers.push(Marker {
                id: id.clone(),
                time: time.max(0.0),
                name: name.trim().to_owned(),
                color,
            });
            timeline.sort_markers();
            Ok(Outcome {
                created_id: Some(id),
                applied: true,
            })
        }
        Command::UpdateMarker {
            marker_id,
            time,
            name,
            color,
        } => {
            let timeline = project.active_mut();
            let Some(marker) = timeline.markers.iter_mut().find(|m| m.id == marker_id) else {
                return Ok(applied(false));
            };
            let before = marker.clone();
            if let Some(time) = time {
                marker.time = time.max(0.0);
            }
            if let Some(name) = name {
                marker.name = name.trim().to_owned();
            }
            if let Some(color) = color {
                marker.color = color;
            }
            let changed = *marker != before;
            timeline.sort_markers();
            Ok(applied(changed))
        }
        Command::RemoveMarkers { marker_ids } => {
            let timeline = project.active_mut();
            let before = timeline.markers.len();
            timeline
                .markers
                .retain(|marker| !marker_ids.contains(&marker.id));
            Ok(applied(timeline.markers.len() != before))
        }
        Command::LiftRange { start, end } => {
            let (start, end) = (start.min(end).max(0.0), start.max(end).max(0.0));
            let lanes: Vec<String> = project
                .active()
                .tracks
                .iter()
                .map(|track| track.id.clone())
                .collect();
            Ok(applied(clear(project, mint, &lanes, start, end, scope)?))
        }
        Command::PlaceRange {
            media_id,
            track_id,
            start,
            source_in,
            source_out,
            overwrite,
        } => place(
            project,
            mint,
            scope,
            Placement {
                media_id,
                track_id,
                start: start.max(0.0),
                source_in,
                source_out,
                overwrite,
            },
        ),
        Command::RollEdit {
            left_clip_id,
            right_clip_id,
            delta,
        } => roll(project, &left_clip_id, &right_clip_id, delta, scope).map(applied),
        Command::SlipClip { clip_id, delta } => slip(project, &clip_id, delta, scope).map(applied),
        Command::SlideClip { clip_id, delta } => {
            slide(project, &clip_id, delta, scope).map(applied)
        }
        _ => unreachable!("commands::apply routes only this module's commands here"),
    }
}

/// How far into its file each clip's media reaches, by media id: a video's
/// or a sound's length, and no end for a still.
fn lengths(project: &Project) -> HashMap<String, f64> {
    project
        .media
        .iter()
        .map(|media| {
            let length = match media.kind {
                MediaKind::Image => f64::INFINITY,
                _ => media.duration.unwrap_or(f64::INFINITY),
            };
            (media.id.clone(), length)
        })
        .collect()
}

/// Seconds of timeline a clip could grow by at its tail before its file
/// runs out; no end for a title, a shape or a still.
fn room_after(clip: &Clip, lengths: &HashMap<String, f64>) -> f64 {
    let length = lengths
        .get(&clip.media_id)
        .copied()
        .unwrap_or(f64::INFINITY);
    let speed = clip.speed.max(1e-6);
    ((length - clip.source_start) / speed - clip.duration).max(0.0)
}

/// Seconds of timeline a clip could grow by at its head before its file
/// runs out.
fn room_before(clip: &Clip) -> f64 {
    if clip.media_id.is_empty() {
        return f64::INFINITY;
    }
    (clip.source_start / clip.speed.max(1e-6)).max(0.0)
}

/// The clip on the same lane whose tail meets this one's head, and the one
/// whose head meets its tail.
fn touching(timeline: &Timeline, clip: &Clip) -> (Option<String>, Option<String>) {
    let end = clip.start + clip.duration;
    let on_lane = timeline
        .clips
        .iter()
        .filter(|other| other.track_id == clip.track_id && other.id != clip.id);
    let mut before = None;
    let mut after = None;
    for other in on_lane {
        if (other.start + other.duration - clip.start).abs() <= TOUCH {
            before = Some(other.id.clone());
        }
        if (other.start - end).abs() <= TOUCH {
            after = Some(other.id.clone());
        }
    }
    (before, after)
}

/// The clips an edit of `id` takes along: it alone when scoped so, else it
/// and the clips linked to it.
fn members(timeline: &Timeline, id: &str, scope: Scope) -> Vec<String> {
    if scope.alone {
        vec![id.to_owned()]
    } else {
        timeline.with_partners(&[id.to_owned()])
    }
}

/// Moves the head of `clip` by `by` seconds, keeping the frames under the
/// rest of it where they are.
fn move_head(clip: &mut Clip, by: f64) {
    let old = clip.duration;
    clip.start += by;
    clip.duration -= by;
    clip.source_start = (clip.source_start + by * clip.speed).max(0.0);
    clip.rewindow_keys(old, by, old);
}

/// Moves the tail of `clip` by `by` seconds.
fn move_tail(clip: &mut Clip, by: f64) {
    let old = clip.duration;
    clip.duration += by;
    clip.rewindow_keys(old, 0.0, old + by);
}

fn roll(
    project: &mut Project,
    left: &str,
    right: &str,
    delta: f64,
    scope: Scope,
) -> Result<bool, CommandError> {
    let lengths = lengths(project);
    let timeline = project.active_mut();
    let (Some(first), Some(second)) = (timeline.clip(left), timeline.clip(right)) else {
        return Err(CommandError::NotTouching);
    };
    if first.track_id != second.track_id
        || (first.start + first.duration - second.start).abs() > TOUCH
    {
        return Err(CommandError::NotTouching);
    }
    let cut = second.start;
    // The pairs that roll: the two named, and each pair of their partners
    // that meets at the same cut on a lane of its own.
    let mut pairs = vec![(left.to_owned(), right.to_owned())];
    if !scope.alone {
        let lefts = members(timeline, left, scope);
        let rights = members(timeline, right, scope);
        for l in &lefts {
            for r in &rights {
                if (l, r) == (&left.to_owned(), &right.to_owned()) {
                    continue;
                }
                let (Some(a), Some(b)) = (timeline.clip(l), timeline.clip(r)) else {
                    continue;
                };
                if a.track_id == b.track_id
                    && (a.start + a.duration - cut).abs() <= TOUCH
                    && (b.start - cut).abs() <= TOUCH
                {
                    pairs.push((l.clone(), r.clone()));
                }
            }
        }
    }
    // As far as every pair can go: each first clip keeps a frame and does
    // not run out of file, and each second clip the same.
    let (mut low, mut high) = (f64::NEG_INFINITY, f64::INFINITY);
    for (l, r) in &pairs {
        let (Some(a), Some(b)) = (timeline.clip(l), timeline.clip(r)) else {
            continue;
        };
        low = low.max(-(a.duration - MIN_CLIP_DURATION).min(room_before(b)));
        high = high.min((b.duration - MIN_CLIP_DURATION).min(room_after(a, &lengths)));
    }
    let by = delta.clamp(low.min(0.0), high.max(0.0));
    if by.abs() < 1e-9 {
        return Ok(false);
    }
    for (l, r) in pairs {
        if let Some(clip) = timeline.clip_mut(&l) {
            move_tail(clip, by);
        }
        if let Some(clip) = timeline.clip_mut(&r) {
            move_head(clip, by);
        }
    }
    Ok(true)
}

fn slip(project: &mut Project, id: &str, delta: f64, scope: Scope) -> Result<bool, CommandError> {
    let lengths = lengths(project);
    let timeline = project.active_mut();
    let moving = members(timeline, id, scope);
    // As far as every one of them can: no frame before its file's first,
    // none after its last.
    let (mut low, mut high) = (f64::NEG_INFINITY, f64::INFINITY);
    for member in &moving {
        let Some(clip) = timeline.clip(member) else {
            continue;
        };
        if clip.media_id.is_empty() {
            continue;
        }
        let length = lengths
            .get(&clip.media_id)
            .copied()
            .unwrap_or(f64::INFINITY);
        low = low.max(-clip.source_start);
        high = high.min(length - clip.source_start - clip.duration * clip.speed);
    }
    let by = delta.clamp(low.min(0.0), high.max(0.0));
    if by.abs() < 1e-9 {
        return Ok(false);
    }
    for member in moving {
        if let Some(clip) = timeline.clip_mut(&member)
            && !clip.media_id.is_empty()
        {
            clip.source_start = (clip.source_start + by).max(0.0);
        }
    }
    Ok(true)
}

fn slide(project: &mut Project, id: &str, delta: f64, scope: Scope) -> Result<bool, CommandError> {
    let lengths = lengths(project);
    let timeline = project.active_mut();
    let moving = members(timeline, id, scope);
    // Each moving clip with the neighbours that give way to it.
    let mut plans: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    let (mut low, mut high) = (f64::NEG_INFINITY, f64::INFINITY);
    for member in &moving {
        let Some(clip) = timeline.clip(member) else {
            continue;
        };
        let (before, after) = touching(timeline, clip);
        let (floor, ceiling) = timeline.room_around(member).unwrap_or((0.0, f64::INFINITY));
        // Left: the clip before shrinks, or the gap before closes; the
        // clip after grows back over its own head.
        let left = match before.as_deref().and_then(|b| timeline.clip(b)) {
            Some(b) => b.duration - MIN_CLIP_DURATION,
            None => clip.start - floor,
        };
        let left = match after.as_deref().and_then(|a| timeline.clip(a)) {
            Some(a) => left.min(room_before(a)),
            None => left,
        };
        // Right: the clip after shrinks, or the gap after closes; the clip
        // before grows over its own tail.
        let right = match after.as_deref().and_then(|a| timeline.clip(a)) {
            Some(a) => a.duration - MIN_CLIP_DURATION,
            None => ceiling - (clip.start + clip.duration),
        };
        let right = match before.as_deref().and_then(|b| timeline.clip(b)) {
            Some(b) => right.min(room_after(b, &lengths)),
            None => right,
        };
        low = low.max(-left.max(0.0));
        high = high.min(right.max(0.0));
        plans.push((member.clone(), before, after));
    }
    let by = delta.clamp(low.min(0.0), high.max(0.0));
    if by.abs() < 1e-9 || plans.is_empty() {
        return Ok(false);
    }
    for (member, before, after) in plans {
        if let Some(clip) = timeline.clip_mut(&member) {
            clip.start += by;
        }
        if let Some(clip) = before.as_deref().and_then(|b| timeline.clip_mut(b)) {
            move_tail(clip, by);
        }
        if let Some(clip) = after.as_deref().and_then(|a| timeline.clip_mut(a)) {
            move_head(clip, by);
        }
    }
    Ok(true)
}

/// Clears `start..end` on `lanes`: every clip across either end is cut
/// there, each clip on its own, and what lies between goes. Whether
/// anything changed.
fn clear(
    project: &mut Project,
    mint: &mut IdMint,
    lanes: &[String],
    start: f64,
    end: f64,
    scope: Scope,
) -> Result<bool, CommandError> {
    if end - start <= TOUCH {
        return Ok(false);
    }
    let alone = Scope {
        alone: true,
        ..scope
    };
    let mut applied = false;
    for time in [end, start] {
        let crossing: Vec<String> = project
            .active()
            .clips
            .iter()
            .filter(|clip| {
                lanes.contains(&clip.track_id)
                    && clip.start + MIN_CLIP_DURATION < time
                    && time < clip.start + clip.duration - MIN_CLIP_DURATION
            })
            .map(|clip| clip.id.clone())
            .collect();
        if !crossing.is_empty() {
            let cut = Command::SplitClips {
                clip_ids: crossing,
                time,
            };
            applied |= clips::apply(project, mint, cut, alone)?.applied;
        }
    }
    let timeline = project.active_mut();
    let before = timeline.clips.len();
    timeline.clips.retain(|clip| {
        !(lanes.contains(&clip.track_id)
            && clip.start >= start - TOUCH
            && clip.start + clip.duration <= end + TOUCH)
    });
    Ok(applied || timeline.clips.len() != before)
}

/// Opens `length` seconds at `at` on every lane: a clip across it is cut
/// there, and everything from it on moves later by the length.
fn open(
    project: &mut Project,
    mint: &mut IdMint,
    at: f64,
    length: f64,
    scope: Scope,
) -> Result<(), CommandError> {
    let crossing: Vec<String> = project
        .active()
        .clips
        .iter()
        .filter(|clip| {
            clip.start + MIN_CLIP_DURATION < at
                && at < clip.start + clip.duration - MIN_CLIP_DURATION
        })
        .map(|clip| clip.id.clone())
        .collect();
    if !crossing.is_empty() {
        let alone = Scope {
            alone: true,
            ..scope
        };
        let cut = Command::SplitClips {
            clip_ids: crossing,
            time: at,
        };
        clips::apply(project, mint, cut, alone)?;
    }
    for clip in project
        .active_mut()
        .clips_where(|clip| clip.start >= at - TOUCH)
    {
        clip.start += length;
    }
    Ok(())
}

/// What [`Command::PlaceRange`] asks for.
struct Placement {
    media_id: String,
    track_id: Option<String>,
    start: f64,
    source_in: f64,
    source_out: f64,
    overwrite: bool,
}

fn place(
    project: &mut Project,
    mint: &mut IdMint,
    scope: Scope,
    asked: Placement,
) -> Result<Outcome, CommandError> {
    let media = project
        .media_by_id(&asked.media_id)
        .ok_or(CommandError::MediaGone)?
        .clone();
    let reach = match media.kind {
        MediaKind::Image => f64::INFINITY,
        _ => media.duration.unwrap_or(f64::INFINITY),
    };
    let source_in = asked.source_in.min(asked.source_out).max(0.0);
    let source_out = asked.source_in.max(asked.source_out).min(reach);
    let length = source_out - source_in;
    if length < MIN_CLIP_DURATION {
        return Err(CommandError::EmptyRange);
    }
    let index = links::active_index(project);
    let kind = clips::kind_of(&media);
    let timeline = project.active();
    let typed = timeline.is_typed();
    // The picture's lane: the one named when it holds the kind, else the
    // first that does; and on lanes with kinds the first sound lane for a
    // video's sound.
    let lane = asked
        .track_id
        .filter(|id| timeline.track(id).is_some() && timeline.track_kind(id).accepts(kind))
        .or_else(|| links::lanes_for(timeline, kind).into_iter().next());
    let sound_lane = (typed && kind == ClipKind::Video && media.has_audio && !scope.sound_inside)
        .then(|| {
            links::lanes_for(timeline, ClipKind::Audio)
                .into_iter()
                .next()
        })
        .flatten();
    let lane = match lane {
        Some(lane) => lane,
        None => links::fresh_lane(project.active_mut(), mint, kind),
    };
    let start = asked.start;
    if asked.overwrite {
        let lanes: Vec<String> = std::iter::once(lane.clone())
            .chain(sound_lane.clone())
            .collect();
        clear(project, mint, &lanes, start, start + length, scope)?;
    } else {
        open(project, mint, start, length, scope)?;
    }
    let id = mint.next("c");
    let mut clip = clips::default_clip(id.clone(), lane, &media, start);
    clip.source_start = source_in;
    clip.duration = length;
    project.active_mut().clips.push(Arc::new(clip));
    if typed && !scope.sound_inside {
        links::detach_sound(project, index, mint, &id, sound_lane.as_deref());
    }
    Ok(Outcome {
        created_id: Some(id),
        applied: true,
    })
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

    fn run(editor: &mut Editor, command: Command) -> Outcome {
        editor.apply(command).expect("applies")
    }

    /// A stretch of the file placed, as the three-point edit does.
    fn place_at(
        editor: &mut Editor,
        media: &str,
        start: f64,
        range: (f64, f64),
        overwrite: bool,
    ) -> String {
        run(
            editor,
            Command::PlaceRange {
                media_id: media.to_owned(),
                track_id: None,
                start,
                source_in: range.0,
                source_out: range.1,
                overwrite,
            },
        )
        .created_id
        .unwrap()
    }

    fn clip(editor: &Editor, id: &str) -> Clip {
        editor
            .project()
            .active()
            .clip(id)
            .cloned()
            .expect("the clip")
    }

    fn sound_of(editor: &Editor, id: &str) -> Clip {
        editor
            .project()
            .active()
            .partners(id)
            .first()
            .map(|clip| (*clip).clone())
            .expect("its sound")
    }

    #[test]
    fn a_stretch_is_placed_with_its_sound_linked() {
        let (mut editor, media) = editor();
        let id = place_at(&mut editor, &media, 2.0, (3.0, 5.0), false);
        let picture = clip(&editor, &id);
        assert_eq!(
            (picture.start, picture.duration, picture.source_start),
            (2.0, 2.0, 3.0)
        );
        let sound = sound_of(&editor, &id);
        assert_eq!(
            (sound.start, sound.duration, sound.source_start),
            (2.0, 2.0, 3.0)
        );
        assert_eq!(
            editor.project().active().track_kind(&sound.track_id),
            TrackKind::Audio
        );
    }

    #[test]
    fn an_insert_pushes_everything_later_and_an_overwrite_covers() {
        let (mut editor, media) = editor();
        let first = place_at(&mut editor, &media, 0.0, (0.0, 4.0), false);
        // An insert at 1 s cuts the first clip there and moves its tail.
        let inserted = place_at(&mut editor, &media, 1.0, (6.0, 8.0), false);
        assert_eq!(clip(&editor, &first).duration, 1.0);
        let tail = editor
            .project()
            .active()
            .clips
            .iter()
            .find(|c| c.kind == ClipKind::Video && (c.start - 3.0).abs() < 1e-9)
            .cloned()
            .expect("the tail moved to 3 s");
        assert_eq!(tail.duration, 3.0);
        assert_eq!(clip(&editor, &inserted).start, 1.0);
        // An overwrite over 0.5..1.5 cuts and covers, moving nothing.
        let over = place_at(&mut editor, &media, 0.5, (9.0, 10.0), true);
        assert_eq!(clip(&editor, &first).duration, 0.5);
        // The inserted clip was cut at 1.5 s: its head, which keeps its id,
        // went under the overwrite, and its tail runs on from there.
        assert!(editor.project().active().clip(&inserted).is_none());
        assert!(editor.project().active().clips.iter().any(|c| {
            c.kind == ClipKind::Video
                && (c.start - 1.5).abs() < 1e-9
                && (c.duration - 1.5).abs() < 1e-9
        }));
        assert_eq!(clip(&editor, &over).start, 0.5);
        assert!(tail.start == 3.0 && clip(&editor, &tail.id).start == 3.0);
    }

    #[test]
    fn a_roll_moves_the_cut_and_keeps_to_the_files() {
        let (mut editor, media) = editor();
        let a = place_at(&mut editor, &media, 0.0, (0.0, 4.0), false);
        let b = place_at(&mut editor, &media, 4.0, (5.0, 8.0), false);
        run(
            &mut editor,
            Command::RollEdit {
                left_clip_id: a.clone(),
                right_clip_id: b.clone(),
                delta: 1.0,
            },
        );
        assert_eq!(clip(&editor, &a).duration, 5.0);
        let second = clip(&editor, &b);
        assert_eq!(
            (second.start, second.duration, second.source_start),
            (5.0, 2.0, 6.0)
        );
        // The sounds rolled with them.
        assert_eq!(sound_of(&editor, &a).duration, 5.0);
        assert_eq!(sound_of(&editor, &b).start, 5.0);
        // Back as far as the second clip's file allows: it starts at 6 s
        // of its file, so the cut can come back six seconds - but the
        // first clip keeps one frame.
        run(
            &mut editor,
            Command::RollEdit {
                left_clip_id: a.clone(),
                right_clip_id: b.clone(),
                delta: -100.0,
            },
        );
        let first = clip(&editor, &a);
        assert!(
            (first.duration - MIN_CLIP_DURATION).abs() < 1e-9,
            "{}",
            first.duration
        );
        assert!(clip(&editor, &b).source_start >= 0.0);
        let refused = editor.apply(Command::RollEdit {
            left_clip_id: b.clone(),
            right_clip_id: a,
            delta: 1.0,
        });
        assert_eq!(refused.unwrap_err(), CommandError::NotTouching);
    }

    #[test]
    fn a_slip_shows_other_frames_within_the_file() {
        let (mut editor, media) = editor();
        let a = place_at(&mut editor, &media, 1.0, (2.0, 5.0), false);
        run(
            &mut editor,
            Command::SlipClip {
                clip_id: a.clone(),
                delta: 4.0,
            },
        );
        let slipped = clip(&editor, &a);
        // The file has 10 s and the clip shows 3 of it: 7 is as far as it
        // goes.
        assert_eq!(
            (slipped.start, slipped.duration, slipped.source_start),
            (1.0, 3.0, 6.0)
        );
        assert_eq!(sound_of(&editor, &a).source_start, 6.0);
        run(
            &mut editor,
            Command::SlipClip {
                clip_id: a.clone(),
                delta: 4.0,
            },
        );
        assert_eq!(clip(&editor, &a).source_start, 7.0);
    }

    #[test]
    fn a_slide_moves_the_clip_and_its_neighbours_give_way() {
        let (mut editor, media) = editor();
        let a = place_at(&mut editor, &media, 0.0, (0.0, 2.0), false);
        let b = place_at(&mut editor, &media, 2.0, (4.0, 6.0), false);
        let c = place_at(&mut editor, &media, 4.0, (2.0, 5.0), false);
        run(
            &mut editor,
            Command::SlideClip {
                clip_id: b.clone(),
                delta: 1.0,
            },
        );
        assert_eq!(clip(&editor, &a).duration, 3.0);
        let middle = clip(&editor, &b);
        assert_eq!(
            (middle.start, middle.duration, middle.source_start),
            (3.0, 2.0, 4.0)
        );
        let last = clip(&editor, &c);
        assert_eq!(
            (last.start, last.duration, last.source_start),
            (5.0, 2.0, 3.0)
        );
        // The lane keeps its length.
        assert_eq!(last.start + last.duration, 7.0);
    }

    #[test]
    fn markers_are_added_kept_in_order_changed_and_removed() {
        let (mut editor, _) = editor();
        let late = run(
            &mut editor,
            Command::AddMarker {
                time: 5.0,
                name: " Drop ".into(),
                color: MarkerColor::Red,
            },
        )
        .created_id
        .unwrap();
        let early = run(
            &mut editor,
            Command::AddMarker {
                time: 1.0,
                name: String::new(),
                color: MarkerColor::default(),
            },
        )
        .created_id
        .unwrap();
        let markers = &editor.project().active().markers;
        assert_eq!(markers[0].id, early);
        assert_eq!(markers[1].name, "Drop");
        run(
            &mut editor,
            Command::UpdateMarker {
                marker_id: late.clone(),
                time: Some(0.5),
                name: None,
                color: Some(MarkerColor::Green),
            },
        );
        let markers = &editor.project().active().markers;
        assert_eq!(
            (markers[0].id.as_str(), markers[0].color),
            (late.as_str(), MarkerColor::Green)
        );
        run(
            &mut editor,
            Command::RemoveMarkers {
                marker_ids: vec![late],
            },
        );
        assert_eq!(editor.project().active().markers.len(), 1);
        // A marker id is adopted, so a reopened project mints past it.
        let mut fresh = IdMint::default();
        fresh.adopt_project(editor.project());
        assert_ne!(fresh.next("k"), early);
    }

    #[test]
    fn a_lift_leaves_the_gap() {
        let (mut editor, media) = editor();
        let a = place_at(&mut editor, &media, 0.0, (0.0, 6.0), false);
        run(
            &mut editor,
            Command::LiftRange {
                start: 2.0,
                end: 4.0,
            },
        );
        assert_eq!(clip(&editor, &a).duration, 2.0);
        let tail = editor
            .project()
            .active()
            .clips
            .iter()
            .find(|c| c.kind == ClipKind::Video && c.id != a)
            .cloned()
            .expect("the tail");
        assert_eq!((tail.start, tail.duration), (4.0, 2.0));
    }
}
