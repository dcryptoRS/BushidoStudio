// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Lanes with kinds and linked clips (Royalty Studio fork).
//!
//! A timeline whose lanes have kinds holds its pictures on picture lanes
//! over its sound on sound lanes, as Premiere and Resolve do; a video with
//! sound placed there lands as a picture and a sound, linked
//! ([`Clip::link`]), and the edits that place, move, trim, cut, retime and
//! delete clips take the clips linked to the ones they name along with them
//! unless told otherwise ([`Scope::alone`]). What is shared by those edits
//! lives here: which lanes hold which clips, the sound taken out of a
//! picture, linking and unlinking, and the conversion of a timeline from
//! before lanes had kinds.

use super::*;

/// Applies one of this module's commands. Any other is a routing error.
pub(super) fn apply(
    project: &mut Project,
    mint: &mut IdMint,
    command: Command,
) -> Result<Outcome, CommandError> {
    match command {
        Command::LinkClips { clip_ids } => {
            let timeline = project.active_mut();
            let named: Vec<String> = clip_ids
                .iter()
                .filter(|id| timeline.clip(id).is_some())
                .cloned()
                .collect();
            if named.len() < 2 {
                return Ok(Outcome::default());
            }
            let members: HashSet<String> = timeline.with_partners(&named).into_iter().collect();
            let link = mint.next("l");
            for clip in timeline.clips_where(|clip| members.contains(&clip.id)) {
                clip.link = Some(link.clone());
            }
            Ok(Outcome {
                created_id: None,
                applied: true,
            })
        }

        Command::UnlinkClips { clip_ids } => {
            let timeline = project.active_mut();
            let links: HashSet<String> = clip_ids
                .iter()
                .filter_map(|id| timeline.clip(id).and_then(|clip| clip.link.clone()))
                .collect();
            let mut applied = false;
            for clip in timeline
                .clips_where(|clip| clip.link.as_ref().is_some_and(|link| links.contains(link)))
            {
                clip.link = None;
                applied = true;
            }
            Ok(Outcome {
                created_id: None,
                applied,
            })
        }

        _ => unreachable!("commands::apply routes only this module's commands here"),
    }
}

/// The lanes that hold a clip of `kind`, in the order a free one is looked
/// for: picture lanes from V1 up, sound lanes from A1 down - the lane
/// nearest the line between them first. On a timeline without kinds every
/// lane, from the bottom, as the walk always was.
pub(super) fn lanes_for(timeline: &Timeline, kind: ClipKind) -> Vec<String> {
    let mut lanes: Vec<&Track> = timeline
        .tracks
        .iter()
        .filter(|track| track.kind.accepts(kind))
        .collect();
    if kind == ClipKind::Audio && timeline.is_typed() {
        lanes.reverse();
    }
    lanes.into_iter().map(|track| track.id.clone()).collect()
}

/// The first lane holding `kind` with nothing over `[start, start +
/// duration)`.
pub(super) fn free_lane(
    timeline: &Timeline,
    kind: ClipKind,
    start: f64,
    duration: f64,
) -> Option<String> {
    lanes_for(timeline, kind)
        .into_iter()
        .find(|id| timeline.is_free(id, start, duration, &[]))
}

/// A new, empty lane for a clip of `kind`: a picture lane on top of the
/// stack, a sound lane under every other one. On a timeline without kinds,
/// a lane of any kind on top, as before.
pub(super) fn fresh_lane(timeline: &mut Timeline, mint: &mut IdMint, kind: ClipKind) -> String {
    let id = mint.next("t");
    if !timeline.is_typed() {
        timeline
            .tracks
            .push(Track::of_kind(id.clone(), TrackKind::Any));
    } else if kind == ClipKind::Audio {
        timeline
            .tracks
            .insert(0, Track::of_kind(id.clone(), TrackKind::Audio));
    } else {
        timeline
            .tracks
            .push(Track::of_kind(id.clone(), TrackKind::Video));
    }
    id
}

/// The lane a clip of `kind` named for `track_id` goes on: that lane when
/// it holds the kind, else the first lane of the kind free over the span,
/// else a new one.
pub(super) fn lane_holding(
    timeline: &mut Timeline,
    mint: &mut IdMint,
    track_id: &str,
    kind: ClipKind,
    start: f64,
    duration: f64,
) -> String {
    if timeline.track(track_id).is_some() && timeline.track_kind(track_id).accepts(kind) {
        return track_id.to_owned();
    }
    free_lane(timeline, kind, start, duration).unwrap_or_else(|| fresh_lane(timeline, mint, kind))
}

/// Moves the clips linked to the ones in `moved` by as much as each of
/// those moved, when they did not move themselves: a ripple on a picture
/// lane carries the sound of every picture it pushed, so a magnetic insert
/// does not leave the cut after it out of sync.
pub(super) fn carry_partners(timeline: &mut Timeline, moved: &[(String, f64)]) {
    let shifted: HashSet<&str> = moved.iter().map(|(id, _)| id.as_str()).collect();
    let mut follow: Vec<(String, f64)> = Vec::new();
    for (id, by) in moved {
        for partner in timeline.partners(id) {
            if !shifted.contains(partner.id.as_str())
                && !follow.iter().any(|(other, _)| *other == partner.id)
            {
                follow.push((partner.id.clone(), *by));
            }
        }
    }
    for (id, by) in follow {
        if let Some(clip) = timeline.clip_mut(&id) {
            clip.start = (clip.start + by).max(0.0);
        }
    }
}

/// What a video clip's sound comes off as: one sound per audio track when
/// the file lists several - a recording that kept the desktop and the
/// microphone apart stays apart, each on a lane of its own - else the one
/// stream the clip plays, as `(stream, name)`. Empty when the clip is not
/// an unmuted video with sound, or its sound is off already.
fn sounds_of(
    timeline: &Timeline,
    project_media: &[MediaItem],
    clip_id: &str,
) -> Vec<(Option<u32>, String)> {
    let Some(clip) = timeline.clip(clip_id) else {
        return Vec::new();
    };
    let media = project_media.iter().find(|item| item.id == clip.media_id);
    let has_audio = clip.kind == ClipKind::Video
        && clip.muted != Some(true)
        && media.is_some_and(|media| media.has_audio)
        && !timeline
            .clips
            .iter()
            .any(|other| other.detached_from.as_deref() == Some(clip_id));
    if !has_audio {
        return Vec::new();
    }
    match media {
        Some(media) if media.audio_tracks.len() > 1 => media
            .audio_tracks
            .iter()
            .enumerate()
            .map(|(position, track)| {
                let label = if track.title.is_empty() {
                    format!("Track {}", position + 1)
                } else {
                    track.title.clone()
                };
                (Some(track.index), format!("{} · {label}", clip.name))
            })
            .collect(),
        _ => vec![(clip.audio_stream, clip.name.clone())],
    }
}

/// Takes the sound out of the video clip `clip_id` on the timeline at
/// `index`: a sound clip per stream, each on a lane that holds sound and is
/// free for the clip's whole span (a new one when none is), the picture
/// muted and its audio filters handed to the sound. On a timeline with
/// kinds the picture and its sound are linked. `prefer` is a sound lane to
/// try first - the one a video was dropped on. Returns the first sound's
/// id, or None when there was no sound to take.
pub(super) fn detach_sound(
    project: &mut Project,
    index: usize,
    mint: &mut IdMint,
    clip_id: &str,
    prefer: Option<&str>,
) -> Option<String> {
    let sounds = sounds_of(&project.timelines[index], &project.media, clip_id);
    if sounds.is_empty() {
        return None;
    }
    let timeline = Arc::make_mut(&mut project.timelines[index]);
    let clip = timeline.clip(clip_id)?.clone();
    let link = timeline
        .is_typed()
        .then(|| clip.link.clone().unwrap_or_else(|| mint.next("l")));
    let mut first = None;
    for (stream, name) in sounds {
        // Each sound placed takes its lane, so the next looks past it.
        let preferred = prefer.filter(|lane| {
            timeline.track_kind(lane) == TrackKind::Audio
                && timeline.is_free(lane, clip.start, clip.duration, &[])
        });
        let lane = match preferred {
            Some(lane) => lane.to_owned(),
            None => free_lane(timeline, ClipKind::Audio, clip.start, clip.duration)
                .unwrap_or_else(|| fresh_lane(timeline, mint, ClipKind::Audio)),
        };
        let mut sound = clip.clone();
        sound.id = mint.next("c");
        sound.track_id = lane;
        sound.name = name;
        sound.kind = ClipKind::Audio;
        sound.video_effects = Vec::new();
        sound.transition_in = None;
        sound.detached_from = Some(clip_id.to_owned());
        sound.muted = None;
        sound.audio_stream = stream;
        sound.link = link.clone();
        first.get_or_insert_with(|| sound.id.clone());
        timeline.clips.push(Arc::new(sound));
    }
    let video = timeline.clip_mut(clip_id)?;
    video.muted = Some(true);
    video.filters = Vec::new();
    if link.is_some() {
        video.link = link;
    }
    first
}

/// The index of the active timeline in `project.timelines`.
pub(super) fn active_index(project: &Project) -> usize {
    project
        .timelines
        .iter()
        .position(|timeline| timeline.id == project.active_timeline_id)
        .unwrap_or(0)
}

/// Gives every timeline from before lanes had kinds its picture and sound
/// lanes (Royalty Studio fork), as a document from then is read: not a step
/// to undo, since it is how the document is edited now. Returns whether any
/// timeline changed.
///
/// - A lane holding nothing but sound becomes a sound lane; every other a
///   picture lane, its sound clips moved to sound lanes where they are.
/// - The sound lanes go under the picture lanes, each kind in the order it
///   had, so the pictures composite exactly as they did.
/// - Every video still carrying its sound has it taken out onto a sound
///   lane, linked to the picture, and a sound detached before is linked to
///   the picture it came from: what plays is what played.
/// - An empty timeline gets two of each, as a new one does.
pub fn type_lanes(project: &mut Project, mint: &mut IdMint) -> bool {
    let mut changed = false;
    for index in 0..project.timelines.len() {
        if project.timelines[index].is_typed() {
            continue;
        }
        changed = true;
        type_timeline(project, index, mint);
    }
    changed
}

fn type_timeline(project: &mut Project, index: usize, mint: &mut IdMint) {
    {
        let timeline = Arc::make_mut(&mut project.timelines[index]);
        if timeline.clips.is_empty() {
            let count = timeline.tracks.len().max(4);
            while timeline.tracks.len() < count {
                timeline
                    .tracks
                    .push(Track::of_kind(mint.next("t"), TrackKind::Any));
            }
            for (row, track) in timeline.tracks.iter_mut().enumerate() {
                track.kind = if row < 2 {
                    TrackKind::Audio
                } else {
                    TrackKind::Video
                };
            }
            return;
        }
        let holds = |kind: ClipKind, track: &Track| {
            timeline
                .clips
                .iter()
                .filter(|clip| clip.track_id == track.id)
                .any(|clip| (clip.kind == ClipKind::Audio) == (kind == ClipKind::Audio))
        };
        let kinds: Vec<TrackKind> = timeline
            .tracks
            .iter()
            .map(|track| {
                if holds(ClipKind::Audio, track) && !holds(ClipKind::Video, track) {
                    TrackKind::Audio
                } else {
                    TrackKind::Video
                }
            })
            .collect();
        for (track, kind) in timeline.tracks.iter_mut().zip(kinds) {
            track.kind = kind;
        }
        // Sound under picture, each kind in its own order.
        let (mut sound, picture): (Vec<Track>, Vec<Track>) = std::mem::take(&mut timeline.tracks)
            .into_iter()
            .partition(|track| track.kind == TrackKind::Audio);
        if sound.is_empty() {
            sound.push(Track::of_kind(mint.next("t"), TrackKind::Audio));
        }
        timeline.tracks = sound.into_iter().chain(picture).collect();
        // Sound on a picture lane goes to a sound lane, where it was in time.
        let strays: Vec<String> = timeline
            .clips
            .iter()
            .filter(|clip| {
                clip.kind == ClipKind::Audio
                    && !timeline.track_kind(&clip.track_id).accepts(clip.kind)
            })
            .map(|clip| clip.id.clone())
            .collect();
        for id in strays {
            let Some((start, duration)) =
                timeline.clip(&id).map(|clip| (clip.start, clip.duration))
            else {
                continue;
            };
            let lane = free_lane(timeline, ClipKind::Audio, start, duration)
                .unwrap_or_else(|| fresh_lane(timeline, mint, ClipKind::Audio));
            if let Some(clip) = timeline.clip_mut(&id) {
                clip.track_id = lane;
            }
        }
        // A sound detached before kinds is linked to its picture.
        let pairs: Vec<(String, String)> = timeline
            .clips
            .iter()
            .filter_map(|clip| {
                let picture = clip.detached_from.as_ref()?;
                timeline.clip(picture)?;
                Some((clip.id.clone(), picture.clone()))
            })
            .collect();
        for (sound, picture) in pairs {
            let link = timeline
                .clip(&picture)
                .and_then(|clip| clip.link.clone())
                .unwrap_or_else(|| mint.next("l"));
            for id in [&sound, &picture] {
                if let Some(clip) = timeline.clip_mut(id) {
                    clip.link = Some(link.clone());
                }
            }
        }
    }
    // Every picture still carrying its sound has it taken out, in time order
    // so the sound lanes fill the way a cut reads.
    let mut pictures: Vec<(f64, String)> = project.timelines[index]
        .clips
        .iter()
        .filter(|clip| clip.kind == ClipKind::Video)
        .map(|clip| (clip.start, clip.id.clone()))
        .collect();
    pictures.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, id) in pictures {
        detach_sound(project, index, mint, &id, None);
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::{ClipMove, Command, NewMedia, Scope, TrimEdge};
    use crate::editor::Editor;
    use crate::model::{Clip, ClipKind, MediaKind, TrackKind};

    fn item(path: &str, kind: MediaKind, duration: f64, has_audio: bool) -> Command {
        Command::AddMedia {
            item: NewMedia {
                path: path.to_owned(),
                name: path.to_owned(),
                duration: Some(duration),
                kind,
                width: (kind != MediaKind::Audio).then_some(1920),
                height: (kind != MediaKind::Audio).then_some(1080),
                frame_rate: Some(30.0),
                frame_rate_fraction: Some("30/1".to_owned()),
                video_codec: None,
                audio_codec: has_audio.then(|| "aac".to_owned()),
                has_audio,
                audio_tracks: Vec::new(),
                origin: None,
                motion: None,
                color_space: Default::default(),
            },
        }
    }

    /// A new project's editor, as the host opens one: lanes with kinds.
    fn typed() -> Editor {
        let mut editor = Editor::new();
        assert!(editor.type_lanes());
        editor
    }

    fn add(editor: &mut Editor, command: Command) -> String {
        editor
            .apply(command)
            .expect("applies")
            .created_id
            .expect("creates")
    }

    fn clip<'a>(editor: &'a Editor, id: &str) -> &'a Clip {
        editor.project().active().clip(id).expect("on the timeline")
    }

    fn lane_kind(editor: &Editor, id: &str) -> TrackKind {
        let timeline = editor.project().active();
        timeline.track_kind(&clip(editor, id).track_id)
    }

    /// The video `id` placed and its one sound.
    fn placed(editor: &mut Editor, path: &str, start: f64) -> (String, String) {
        let media = add(editor, item(path, MediaKind::Video, 10.0, true));
        let picture = add(
            editor,
            Command::AddClipAtFirstFree {
                media_id: media,
                start,
            },
        );
        let partners = editor.project().active().partners(&picture);
        assert_eq!(partners.len(), 1, "one sound for one stream");
        (picture, partners[0].id.clone())
    }

    #[test]
    fn a_new_timeline_has_two_sound_lanes_under_two_picture_lanes() {
        let editor = typed();
        let kinds: Vec<TrackKind> = editor
            .project()
            .active()
            .tracks
            .iter()
            .map(|track| track.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                TrackKind::Audio,
                TrackKind::Audio,
                TrackKind::Video,
                TrackKind::Video
            ]
        );
    }

    #[test]
    fn a_video_with_sound_lands_as_a_picture_and_a_linked_sound() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 2.0);
        let (p, s) = (clip(&editor, &picture), clip(&editor, &sound));
        assert_eq!(p.kind, ClipKind::Video);
        assert_eq!(p.muted, Some(true), "the picture plays no sound of its own");
        assert_eq!(s.kind, ClipKind::Audio);
        assert_eq!((s.start, s.duration), (p.start, p.duration));
        assert_eq!(lane_kind(&editor, &picture), TrackKind::Video);
        assert_eq!(lane_kind(&editor, &sound), TrackKind::Audio);
        // A1 is the sound lane next to the line: the last sound lane stored.
        assert_eq!(s.track_id, editor.project().active().tracks[1].id);
        assert!(p.link.is_some() && p.link == s.link);
        assert_eq!(s.detached_from.as_deref(), Some(picture.as_str()));
    }

    #[test]
    fn sound_inside_keeps_one_clip() {
        let mut editor = typed();
        let media = add(&mut editor, item("a.mp4", MediaKind::Video, 10.0, true));
        let picture = add(
            &mut editor,
            Command::Scoped {
                scope: Scope {
                    sound_inside: true,
                    ..Scope::default()
                },
                command: Box::new(Command::AddClipAtFirstFree {
                    media_id: media,
                    start: 0.0,
                }),
            },
        );
        assert_eq!(editor.project().active().clips.len(), 1);
        assert_eq!(clip(&editor, &picture).muted, None);
    }

    #[test]
    fn each_kind_lands_on_its_own_lanes_whatever_lane_it_is_dropped_on() {
        let mut editor = typed();
        let timeline = editor.project().active().clone();
        let a2 = timeline.tracks[0].id.clone();
        let v1 = timeline.tracks[2].id.clone();
        // A video dropped on A2: its sound there, its picture on V1.
        let video = add(&mut editor, item("v.mp4", MediaKind::Video, 4.0, true));
        let picture = add(
            &mut editor,
            Command::AddClip {
                media_id: video,
                track_id: a2.clone(),
                start: 0.0,
                ripple: false,
            },
        );
        assert_eq!(clip(&editor, &picture).track_id, v1);
        let sound = editor.project().active().partners(&picture)[0].id.clone();
        assert_eq!(clip(&editor, &sound).track_id, a2);
        // Music dropped on a picture lane goes to a sound lane.
        let music = add(&mut editor, item("m.wav", MediaKind::Audio, 4.0, true));
        let song = add(
            &mut editor,
            Command::AddClip {
                media_id: music,
                track_id: v1,
                start: 10.0,
                ripple: false,
            },
        );
        assert_eq!(lane_kind(&editor, &song), TrackKind::Audio);
        // A title never lands on a sound lane.
        let title = add(
            &mut editor,
            Command::AddTextClip {
                track_id: Some(a2),
                above: false,
                start: 20.0,
                style: None,
                duration: None,
                offset_y: None,
            },
        );
        assert_eq!(lane_kind(&editor, &title), TrackKind::Video);
    }

    #[test]
    fn a_picture_moves_with_its_sound_unless_alone() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 0.0);
        let lane = clip(&editor, &picture).track_id.clone();
        editor
            .apply(Command::MoveClips {
                moves: vec![ClipMove {
                    clip_id: picture.clone(),
                    start: 3.0,
                    track_id: lane.clone(),
                }],
            })
            .unwrap();
        assert_eq!(clip(&editor, &sound).start, 3.0);
        editor
            .apply(Command::Scoped {
                scope: Scope {
                    alone: true,
                    ..Scope::default()
                },
                command: Box::new(Command::MoveClips {
                    moves: vec![ClipMove {
                        clip_id: picture.clone(),
                        start: 5.0,
                        track_id: lane,
                    }],
                }),
            })
            .unwrap();
        assert_eq!(clip(&editor, &picture).start, 5.0);
        assert_eq!(clip(&editor, &sound).start, 3.0, "alone leaves the sound");
    }

    #[test]
    fn a_picture_asked_onto_a_sound_lane_stays_on_its_own() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 0.0);
        let sound_lane = clip(&editor, &sound).track_id.clone();
        let picture_lane = clip(&editor, &picture).track_id.clone();
        editor
            .apply(Command::MoveClips {
                moves: vec![ClipMove {
                    clip_id: picture.clone(),
                    start: 1.0,
                    track_id: sound_lane,
                }],
            })
            .unwrap();
        assert_eq!(clip(&editor, &picture).track_id, picture_lane);
        assert_eq!(clip(&editor, &picture).start, 1.0);
    }

    #[test]
    fn trims_cuts_retimes_and_deletes_take_the_linked_sound() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 0.0);
        editor
            .apply(Command::TrimClip {
                clip_id: picture.clone(),
                edge: TrimEdge::End,
                delta: -2.0,
                ripple: false,
            })
            .unwrap();
        assert_eq!(clip(&editor, &sound).duration, 8.0);
        editor
            .apply(Command::TrimClip {
                clip_id: sound.clone(),
                edge: TrimEdge::Start,
                delta: 1.0,
                ripple: false,
            })
            .unwrap();
        assert_eq!(clip(&editor, &picture).start, 1.0);
        assert_eq!(clip(&editor, &picture).source_start, 1.0);

        editor
            .apply(Command::SplitClips {
                clip_ids: vec![picture.clone()],
                time: 4.0,
            })
            .unwrap();
        let timeline = editor.project().active();
        assert_eq!(timeline.clips.len(), 4, "the sound was cut with it");
        let tails: Vec<&Clip> = timeline
            .clips
            .iter()
            .filter(|c| c.start == 4.0)
            .map(|c| c.as_ref())
            .collect();
        assert_eq!(tails.len(), 2);
        assert_eq!(tails[0].link, tails[1].link);
        assert_ne!(
            tails[0].link,
            clip(&editor, &picture).link,
            "the tails are a pair of their own"
        );
        let (picture_tail, sound_tail) = if tails[0].kind == ClipKind::Video {
            (tails[0], tails[1])
        } else {
            (tails[1], tails[0])
        };
        assert_eq!(
            sound_tail.detached_from.as_deref(),
            Some(picture_tail.id.as_str())
        );

        editor
            .apply(Command::SetClipSpeed {
                clip_id: picture.clone(),
                speed: 2.0,
            })
            .unwrap();
        assert_eq!(clip(&editor, &sound).speed, 2.0);
        assert_eq!(
            clip(&editor, &sound).duration,
            clip(&editor, &picture).duration
        );

        editor
            .apply(Command::RemoveClips {
                clip_ids: vec![picture.clone()],
                ripple: false,
            })
            .unwrap();
        assert!(editor.project().active().clip(&sound).is_none());
    }

    #[test]
    fn a_trim_stops_where_the_more_hemmed_in_of_the_pair_must() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 0.0);
        // Music right after the sound: the sound's tail can grow by nothing.
        let music = add(&mut editor, item("m.wav", MediaKind::Audio, 5.0, true));
        let sound_lane = clip(&editor, &sound).track_id.clone();
        editor
            .apply(Command::AddClip {
                media_id: music,
                track_id: sound_lane,
                start: 10.0,
                ripple: false,
            })
            .unwrap();
        editor
            .apply(Command::TrimClip {
                clip_id: picture.clone(),
                edge: TrimEdge::End,
                delta: -4.0,
                ripple: false,
            })
            .unwrap();
        editor
            .apply(Command::TrimClip {
                clip_id: picture.clone(),
                edge: TrimEdge::End,
                delta: 6.0,
                ripple: false,
            })
            .unwrap();
        assert_eq!(clip(&editor, &picture).duration, 10.0);
        assert_eq!(clip(&editor, &sound).duration, 10.0, "still in sync");
    }

    #[test]
    fn unlinked_they_go_their_own_ways_and_link_again() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 0.0);
        editor
            .apply(Command::UnlinkClips {
                clip_ids: vec![sound.clone()],
            })
            .unwrap();
        assert!(clip(&editor, &picture).link.is_none());
        let lane = clip(&editor, &picture).track_id.clone();
        editor
            .apply(Command::MoveClips {
                moves: vec![ClipMove {
                    clip_id: picture.clone(),
                    start: 2.0,
                    track_id: lane,
                }],
            })
            .unwrap();
        assert_eq!(clip(&editor, &sound).start, 0.0);
        editor
            .apply(Command::LinkClips {
                clip_ids: vec![picture.clone(), sound.clone()],
            })
            .unwrap();
        assert_eq!(editor.project().active().partners(&picture).len(), 1);
    }

    #[test]
    fn a_magnetic_insert_keeps_the_cut_after_it_in_sync() {
        let mut editor = typed();
        let (first, _) = placed(&mut editor, "a.mp4", 0.0);
        let (second, second_sound) = placed(&mut editor, "b.mp4", 10.0);
        let lane = clip(&editor, &first).track_id.clone();
        let media = add(&mut editor, item("c.mp4", MediaKind::Video, 3.0, true));
        let inserted = add(
            &mut editor,
            Command::AddClip {
                media_id: media,
                track_id: lane,
                start: 10.0,
                ripple: true,
            },
        );
        assert_eq!(clip(&editor, &second).start, 13.0);
        assert_eq!(
            clip(&editor, &second_sound).start,
            13.0,
            "the sound moved with it"
        );
        let sound = editor.project().active().partners(&inserted)[0].id.clone();
        assert_eq!(clip(&editor, &sound).start, 10.0);
    }

    #[test]
    fn a_freeze_holds_the_sound_back_as_long_as_the_picture() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 0.0);
        let still = NewMedia {
            path: "still.jpg".to_owned(),
            name: "still.jpg".to_owned(),
            duration: None,
            kind: MediaKind::Image,
            width: Some(1920),
            height: Some(1080),
            frame_rate: None,
            frame_rate_fraction: None,
            video_codec: None,
            audio_codec: None,
            has_audio: false,
            audio_tracks: Vec::new(),
            origin: None,
            motion: None,
            color_space: Default::default(),
        };
        editor
            .apply(Command::FreezeFrame {
                clip_id: picture.clone(),
                time: 4.0,
                duration: Some(2.0),
                still: Some(still),
            })
            .unwrap();
        let timeline = editor.project().active();
        assert_eq!(clip(&editor, &sound).duration, 4.0);
        let tails: Vec<&Clip> = timeline
            .clips
            .iter()
            .filter(|c| c.start == 6.0)
            .map(|c| c.as_ref())
            .collect();
        assert_eq!(
            tails.len(),
            2,
            "picture and sound both resume after the hold"
        );
        assert_eq!(tails[0].link, tails[1].link);
    }

    #[test]
    fn rejoining_the_pieces_rejoins_their_sound() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 0.0);
        let tail = editor
            .apply(Command::SplitClips {
                clip_ids: vec![picture.clone()],
                time: 5.0,
            })
            .unwrap()
            .created_id
            .unwrap();
        let picture_tail = if clip(&editor, &tail).kind == ClipKind::Video {
            tail
        } else {
            editor.project().active().partners(&tail)[0].id.clone()
        };
        editor
            .apply(Command::MergeClips {
                clip_ids: vec![picture.clone(), picture_tail],
            })
            .unwrap();
        assert_eq!(editor.project().active().clips.len(), 2);
        assert_eq!(clip(&editor, &sound).duration, 10.0);
    }

    #[test]
    fn a_videos_sound_can_be_placed_without_its_picture() {
        let mut editor = typed();
        let media = add(&mut editor, item("v.mp4", MediaKind::Video, 4.0, true));
        let v1 = editor.project().active().tracks[2].id.clone();
        let sound = add(
            &mut editor,
            Command::AddSoundClip {
                media_id: media,
                track_id: v1,
                start: 1.0,
            },
        );
        assert_eq!(editor.project().active().clips.len(), 1);
        assert_eq!(clip(&editor, &sound).kind, ClipKind::Audio);
        assert_eq!(lane_kind(&editor, &sound), TrackKind::Audio);
    }

    #[test]
    fn a_rendered_merge_leaves_one_picture_and_one_sound_linked() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 0.0);
        let sound_lane = clip(&editor, &sound).track_id.clone();
        editor
            .apply(Command::SplitClips {
                clip_ids: vec![picture.clone()],
                time: 4.0,
            })
            .unwrap();
        let pictures: Vec<String> = editor
            .project()
            .active()
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Video)
            .map(|clip| clip.id.clone())
            .collect();
        let Command::AddMedia { item: render } = item("merged.mp4", MediaKind::Video, 10.0, true)
        else {
            unreachable!()
        };
        editor
            .apply(Command::ConsolidateClips {
                clip_ids: pictures,
                item: render,
            })
            .unwrap();
        let timeline = editor.project().active();
        assert_eq!(timeline.clips.len(), 2, "one picture, one sound");
        let merged = clip(&editor, &picture);
        assert_eq!(merged.duration, 10.0);
        let sounds = timeline.partners(&picture);
        assert_eq!(sounds.len(), 1);
        assert_eq!(sounds[0].track_id, sound_lane, "back on its lane");
        assert_eq!(sounds[0].duration, 10.0);
    }

    #[test]
    fn taking_time_out_cuts_every_lane_and_keeps_picture_sound_and_words_together() {
        let mut editor = typed();
        let (picture, sound) = placed(&mut editor, "a.mp4", 0.0);
        let title = add(
            &mut editor,
            Command::AddTextClip {
                track_id: None,
                above: true,
                start: 6.0,
                style: None,
                duration: Some(2.0),
                offset_y: None,
            },
        );
        editor
            .apply(Command::RemoveTime {
                ranges: vec![[2.0, 3.0], [4.0, 5.0]],
            })
            .unwrap();
        let timeline = editor.project().active();
        let pictures: Vec<&Clip> = timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Video)
            .map(|clip| clip.as_ref())
            .collect();
        let sounds: Vec<&Clip> = timeline
            .clips
            .iter()
            .filter(|clip| clip.kind == ClipKind::Audio)
            .map(|clip| clip.as_ref())
            .collect();
        assert_eq!(pictures.len(), 3, "kept: 0-2, 3-4, 5-10");
        assert_eq!(sounds.len(), 3);
        let total: f64 = pictures.iter().map(|clip| clip.duration).sum();
        assert!((total - 8.0).abs() < 1e-9);
        // Every picture piece has its sound piece, where it is, linked.
        for piece in &pictures {
            let partners = timeline.partners(&piece.id);
            assert_eq!(partners.len(), 1);
            assert_eq!(partners[0].start, piece.start);
            assert_eq!(partners[0].source_start, piece.source_start);
        }
        assert_eq!(
            clip(&editor, &title).start,
            4.0,
            "the words moved up with the cut"
        );
        assert_eq!(clip(&editor, &picture).duration, 2.0);
        assert!(editor.project().active().clip(&sound).is_some());
    }

    #[test]
    fn a_lane_added_on_top_holds_pictures_and_at_the_bottom_sound() {
        let mut editor = typed();
        let top = add(&mut editor, Command::AddTrack { bottom: false });
        let bottom = add(&mut editor, Command::AddTrack { bottom: true });
        let timeline = editor.project().active();
        assert_eq!(timeline.track_kind(&top), TrackKind::Video);
        assert_eq!(timeline.track_kind(&bottom), TrackKind::Audio);
        assert_eq!(timeline.tracks[0].id, bottom);
    }

    #[test]
    fn an_old_timeline_is_given_kinds_and_its_sound_taken_out_as_it_played() {
        let mut editor = Editor::new();
        let lanes: Vec<String> = editor
            .project()
            .active()
            .tracks
            .iter()
            .map(|t| t.id.clone())
            .collect();
        let video = add(&mut editor, item("v.mp4", MediaKind::Video, 6.0, true));
        let music = add(&mut editor, item("m.wav", MediaKind::Audio, 6.0, true));
        let picture = add(
            &mut editor,
            Command::AddClip {
                media_id: video,
                track_id: lanes[0].clone(),
                start: 0.0,
                ripple: false,
            },
        );
        assert_eq!(
            editor.project().active().clips.len(),
            1,
            "no kinds, no split"
        );
        let song = add(
            &mut editor,
            Command::AddClip {
                media_id: music,
                track_id: lanes[1].clone(),
                start: 0.0,
                ripple: false,
            },
        );
        let title = add(
            &mut editor,
            Command::AddTextClip {
                track_id: Some(lanes[2].clone()),
                above: false,
                start: 0.0,
                style: None,
                duration: None,
                offset_y: None,
            },
        );
        assert!(editor.type_lanes());
        assert!(!editor.type_lanes(), "once is enough");
        let timeline = editor.project().active();
        assert_eq!(lane_kind(&editor, &picture), TrackKind::Video);
        assert_eq!(lane_kind(&editor, &title), TrackKind::Video);
        assert_eq!(lane_kind(&editor, &song), TrackKind::Audio);
        let row = |id: &str| {
            timeline
                .tracks
                .iter()
                .position(|t| t.id == clip(&editor, id).track_id)
                .unwrap()
        };
        assert!(
            row(&title) > row(&picture),
            "the pictures composite as they did"
        );
        let sounds = timeline.partners(&picture);
        assert_eq!(sounds.len(), 1);
        assert_eq!(timeline.track_kind(&sounds[0].track_id), TrackKind::Audio);
        assert_eq!(clip(&editor, &picture).muted, Some(true));
        let last_sound = timeline
            .tracks
            .iter()
            .rposition(|t| t.kind == TrackKind::Audio)
            .unwrap();
        let first_picture = timeline
            .tracks
            .iter()
            .position(|t| t.kind == TrackKind::Video)
            .unwrap();
        assert!(
            last_sound < first_picture,
            "sound lanes under picture lanes"
        );
    }

    #[test]
    fn kinds_and_links_come_back_from_the_document() {
        let mut editor = typed();
        let (picture, _) = placed(&mut editor, "a.mp4", 0.0);
        let settings = crate::doc::DocumentSettings {
            name: "T".to_owned(),
            width: 1920,
            height: 1080,
            rate_num: 30,
            rate_den: 1,
        };
        let document = editor.to_document(&settings);
        let mut back = Editor::from_document(&document).expect("reads");
        assert!(!back.type_lanes(), "already has kinds");
        assert_eq!(back.project().active().partners(&picture).len(), 1);
        // A link minted after reading cannot collide with one read.
        let (other, _) = placed(&mut back, "b.mp4", 20.0);
        assert_ne!(clip(&back, &other).link, clip(&back, &picture).link);
    }
}
