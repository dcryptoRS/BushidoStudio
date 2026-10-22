// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! The cutting room's verbs on the window (Royalty Studio fork): markers,
//! the jump from cut to cut, in and out points, the three-point insert and
//! overwrite, lift and extract, J-K-L, and the roll, slip and slide tools.
//! The edits themselves are `concat_project`'s commands; here is when they
//! are asked for and how a drag shows them before it lets go.

use concat_host::reframe::FollowRequest;
use concat_project::commands::{self, IdMint};
use concat_project::model::{Clip, ClipKind, MarkerColor, MediaKind, VideoSettings};
use concat_project::{Command, Follow};

use super::{Gesture, Studio};
use crate::format::frames_timecode;
use crate::i18n::{t, tf};
use crate::ui::{MarkerData, MarkerSheetData, TimelineTool};

/// The fastest J or L goes, in times real time.
const SHUTTLE_TOP: f32 = 8.0;

/// How far music goes down under a voice, in dB: about 9, so it sits well
/// under the words and is still heard.
const DUCK_DEPTH: f64 = 9.0;

/// The step the voice is listened for at, in seconds.
const VOICE_STEP: f64 = 0.05;

/// Frames a J or an L cut leads or trails the picture by: ten, a third of
/// a second at 30 - the first length an editor reaches for.
const SPLIT_FRAMES: f64 = 10.0;

/// The colour a marker is drawn in.
pub fn marker_tint(color: MarkerColor) -> slint::Color {
    let rgb = match color {
        MarkerColor::Blue => 0x0a84ff,
        MarkerColor::Red => 0xff453a,
        MarkerColor::Green => 0x30d158,
        MarkerColor::Yellow => 0xffd60a,
        MarkerColor::Purple => 0xbf5af2,
        MarkerColor::Cyan => 0x64d2ff,
    };
    slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The marker being named in its sheet, and the name as it is typed.
pub struct MarkerSheet {
    pub id: String,
    pub name: String,
}

impl Studio {
    // ── markers ──

    /// M: a marker at the playhead, or - on one already there - its sheet,
    /// to name it.
    pub fn marker_key(&mut self) {
        if self.session.is_none() {
            return;
        }
        let at = f64::from(self.playhead);
        let frame = 0.5 / f64::from(self.frame_rate().max(1.0));
        if let Some(id) = self
            .timeline()
            .markers
            .iter()
            .find(|marker| (marker.time - at).abs() <= frame)
            .map(|marker| marker.id.clone())
        {
            self.open_marker(&id);
            return;
        }
        self.apply(Command::AddMarker {
            time: at,
            name: String::new(),
            color: MarkerColor::default(),
        });
    }

    /// A marker dragged along the ruler.
    pub fn marker_moved(&mut self, id: &str, to: f32) {
        self.apply(Command::UpdateMarker {
            marker_id: id.to_owned(),
            time: Some(f64::from(to.max(0.0))),
            name: None,
            color: None,
        });
    }

    /// The sheet that names a marker, colours it and takes it off.
    pub fn open_marker(&mut self, id: &str) {
        if let Some(marker) = self.timeline().markers.iter().find(|m| m.id == id) {
            self.marker_sheet = Some(MarkerSheet {
                id: marker.id.clone(),
                name: marker.name.clone(),
            });
        }
    }

    /// The sheet put away: the name typed in it kept, as one step.
    pub fn close_marker(&mut self) {
        let Some(sheet) = self.marker_sheet.take() else {
            return;
        };
        let changed = self
            .timeline()
            .markers
            .iter()
            .any(|marker| marker.id == sheet.id && marker.name != sheet.name.trim());
        if changed {
            self.apply(Command::UpdateMarker {
                marker_id: sheet.id,
                time: None,
                name: Some(sheet.name),
                color: None,
            });
        }
    }

    /// A colour picked in the sheet, at once.
    pub fn colour_marker(&mut self, index: i32) {
        let Some(id) = self.marker_sheet.as_ref().map(|sheet| sheet.id.clone()) else {
            return;
        };
        let color = MarkerColor::ALL[(index.max(0) as usize).min(MarkerColor::ALL.len() - 1)];
        self.apply(Command::UpdateMarker {
            marker_id: id,
            time: None,
            name: None,
            color: Some(color),
        });
    }

    /// The sheet's marker taken off.
    pub fn delete_marker(&mut self) {
        let Some(sheet) = self.marker_sheet.take() else {
            return;
        };
        self.apply(Command::RemoveMarkers {
            marker_ids: vec![sheet.id],
        });
    }

    /// The marker sheet as it is drawn.
    pub fn marker_sheet_data(&self) -> MarkerSheetData {
        let Some(sheet) = self.marker_sheet.as_ref() else {
            return MarkerSheetData::default();
        };
        let marker = self.timeline().markers.iter().find(|m| m.id == sheet.id);
        MarkerSheetData {
            open: marker.is_some(),
            name: sheet.name.as_str().into(),
            color: marker
                .and_then(|m| MarkerColor::ALL.iter().position(|c| *c == m.color))
                .unwrap_or(0) as i32,
            time: marker
                .map(|m| frames_timecode(m.time as f32, self.frame_rate()))
                .unwrap_or_default()
                .into(),
        }
    }

    /// The ruler's markers, as it draws them.
    pub fn marker_data(&self) -> Vec<MarkerData> {
        self.timeline()
            .markers
            .iter()
            .map(|marker| MarkerData {
                id: marker.id.as_str().into(),
                time: marker.time as f32,
                name: marker.name.as_str().into(),
                tint: marker_tint(marker.color),
            })
            .collect()
    }

    // ── going from cut to cut ──

    /// Up and Down: the playhead to the cut before or after it, on any
    /// lane - every clip's start and end, as Premiere's arrows go.
    pub fn jump_edit(&mut self, forward: bool) {
        let mut cuts: Vec<f64> = self
            .timeline()
            .clips
            .iter()
            .flat_map(|clip| [clip.start, clip.start + clip.duration])
            .collect();
        cuts.push(0.0);
        self.jump_to(cuts, forward);
    }

    /// Shift+Up and Shift+Down: to the marker before or after the playhead.
    pub fn jump_marker(&mut self, forward: bool) {
        let marks: Vec<f64> = self.timeline().markers.iter().map(|m| m.time).collect();
        self.jump_to(marks, forward);
    }

    fn jump_to(&mut self, mut times: Vec<f64>, forward: bool) {
        times.sort_by(f64::total_cmp);
        let at = f64::from(self.playhead);
        let frame = 0.5 / f64::from(self.frame_rate().max(1.0));
        let target = if forward {
            times.into_iter().find(|time| *time > at + frame)
        } else {
            times.into_iter().rev().find(|time| *time < at - frame)
        };
        if let Some(time) = target {
            self.pause();
            self.seek(time as f32);
        }
    }

    // ── in and out ──

    /// I or O: on the source when one is up, else on the timeline.
    pub fn mark(&mut self, out: bool) {
        if let Some(source) = self.source.as_mut() {
            let at = source.time;
            if out {
                source.mark_out = Some(at);
            } else {
                source.mark_in = Some(at);
            }
            if let (Some(a), Some(b)) = (source.mark_in, source.mark_out)
                && b < a
            {
                if out {
                    source.mark_in = None;
                } else {
                    source.mark_out = None;
                }
            }
            return;
        }
        if self.session.is_none() {
            return;
        }
        let id = self.project().active_timeline_id.clone();
        let at = f64::from(self.playhead);
        let marks = self.marks.entry(id).or_insert((None, None));
        if out {
            marks.1 = Some(at);
            if marks.0.is_some_and(|a| a > at) {
                marks.0 = None;
            }
        } else {
            marks.0 = Some(at);
            if marks.1.is_some_and(|b| b < at) {
                marks.1 = None;
            }
        }
    }

    /// Alt+X: the marks off, the source's when one is up.
    pub fn clear_marks(&mut self) {
        if let Some(source) = self.source.as_mut() {
            source.mark_in = None;
            source.mark_out = None;
            return;
        }
        let id = self.project().active_timeline_id.clone();
        self.marks.remove(&id);
    }

    /// The active timeline's in and out points.
    pub fn timeline_marks(&self) -> (Option<f64>, Option<f64>) {
        self.marks
            .get(&self.project().active_timeline_id)
            .copied()
            .unwrap_or((None, None))
    }

    /// The source's marks as the chip shows them; empty with none.
    pub fn source_marks_text(&self) -> String {
        let Some(source) = self.source.as_ref() else {
            return String::new();
        };
        let fps = self.frame_rate();
        let at = |seconds: f64| frames_timecode(seconds as f32, fps);
        match (source.mark_in, source.mark_out) {
            (Some(a), Some(b)) => tf("marks.inOut", &[&at(a), &at(b)]),
            (Some(a), None) => tf("marks.in", &[&at(a)]),
            (None, Some(b)) => tf("marks.out", &[&at(b)]),
            (None, None) => String::new(),
        }
    }

    // ── the three-point edit ──

    /// Comma inserts and period overwrites: the source's marked stretch -
    /// all of it when nothing is marked - goes onto the timeline at its in
    /// point, or the playhead. With the timeline's in and out set and only
    /// the source's in, the timeline's length is taken from the source's
    /// in, as the three-point rule has it. The monitor then goes back to
    /// the timeline, the playhead after the new clip.
    pub fn three_point(&mut self, overwrite: bool) {
        let Some(source) = self.source.as_ref() else {
            self.notify(&t("marks.needSource"), false);
            return;
        };
        if !source.card.is_empty() {
            self.notify(&t("marks.sourceNotInBin"), false);
            return;
        }
        let media_id = source.media_id.clone();
        let (timeline_in, timeline_out) = self.timeline_marks();
        let source_in = source.mark_in.unwrap_or(0.0);
        let source_out = match (source.mark_out, timeline_in, timeline_out) {
            (Some(out), _, _) => out,
            (None, Some(a), Some(b)) if source.mark_in.is_some() => source_in + (b - a),
            _ => source.duration,
        };
        let start = timeline_in.unwrap_or(f64::from(self.playhead));
        if source_out - source_in < 1.0 / 60.0 {
            self.notify(&t("marks.emptyStretch"), false);
            return;
        }
        self.close_source();
        let placed = self.apply(Command::PlaceRange {
            media_id,
            track_id: None,
            start,
            source_in,
            source_out,
            overwrite,
        });
        if let Some(id) = placed {
            self.selection = vec![id];
            let id = self.project().active_timeline_id.clone();
            self.marks.remove(&id);
            self.seek((start + (source_out - source_in)) as f32);
        }
    }

    /// Semicolon lifts and apostrophe extracts the timeline's marked
    /// stretch: lifted leaves the gap, extracted closes it on every lane.
    pub fn lift_or_extract(&mut self, extract: bool) {
        let (Some(start), Some(end)) = self.timeline_marks() else {
            self.notify(&t("marks.needInOut"), false);
            return;
        };
        let command = if extract {
            Command::RemoveTime {
                ranges: vec![[start, end]],
            }
        } else {
            Command::LiftRange { start, end }
        };
        self.apply(command);
        let id = self.project().active_timeline_id.clone();
        self.marks.remove(&id);
        self.seek(start as f32);
    }

    // ── J, K and L ──

    /// L plays, and faster with each press; J plays backwards the same way;
    /// K stops. Past real time forwards, and backwards at any speed, the
    /// picture is stepped by the clock without its sound, as an editor's
    /// shuttle does.
    pub fn shuttle(&mut self, direction: i32) {
        if direction == 0 {
            self.stop_shuttle();
            self.pause();
            return;
        }
        let current = if self.playing { 1.0 } else { self.shuttle_rate };
        let wanted = direction.signum() as f32;
        let rate = if current != 0.0 && current.signum() == wanted {
            (current * 2.0).clamp(-SHUTTLE_TOP, SHUTTLE_TOP)
        } else {
            wanted
        };
        if rate == 1.0 {
            self.stop_shuttle();
            if !self.playing {
                self.play_toggle();
            }
            return;
        }
        self.pause();
        if self.session.is_none() {
            return;
        }
        self.shuttle_rate = rate;
        let mut last = std::time::Instant::now();
        self.shuttle_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(16),
            move || {
                crate::host::Shell::with(|shell, app| {
                    {
                        let mut studio = shell.studio.borrow_mut();
                        let elapsed = last.elapsed().as_secs_f32();
                        last = std::time::Instant::now();
                        let end = studio.duration();
                        let at = studio.playhead + studio.shuttle_rate * elapsed;
                        studio.playhead = at.clamp(0.0, end);
                        if at <= 0.0 || at >= end {
                            studio.stop_shuttle();
                        }
                        studio.request_preview();
                    }
                    shell.studio.borrow().publish_lanes(&app, &shell.models);
                });
            },
        );
    }

    /// The shuttle stopped, the playhead where it got to.
    pub fn stop_shuttle(&mut self) {
        self.shuttle_rate = 0.0;
        self.shuttle_timer.stop();
    }

    // ── ducking ──

    /// Duck under the voice, from a sound's menu: the sound right-clicked -
    /// or the selection, when it is one of it - ridden down wherever the
    /// rest of the sound on the timeline speaks, as volume keys that show
    /// on the clip and can be moved.
    pub fn duck_under_voice(&mut self, id: &str) {
        let named = if self.selection.iter().any(|held| held == id) {
            self.selection.clone()
        } else {
            vec![id.to_owned()]
        };
        let music: Vec<String> = named
            .into_iter()
            .filter(|id| {
                self.clip(id)
                    .is_some_and(|clip| clip.kind == ClipKind::Audio)
            })
            .collect();
        if music.is_empty() {
            self.notify(&t("duck.needSound"), false);
            return;
        }
        let voices: Vec<Clip> = self
            .timeline()
            .clips
            .iter()
            .filter(|clip| {
                self.clip_has_sound(clip)
                    && !music.contains(&clip.id)
                    && clip.muted != Some(true)
                    && clip.volume > 0.0
            })
            .map(|clip| (**clip).clone())
            .collect();
        let mut ranges = Vec::new();
        let mut unread = false;
        for clip in &voices {
            match self.speech_of(clip) {
                Some(found) => ranges.extend(found),
                None => unread = true,
            }
        }
        if ranges.is_empty() {
            let why = if unread {
                t("duck.wavesNotReady")
            } else {
                t("duck.noVoice")
            };
            self.notify(&why, false);
            return;
        }
        self.apply(Command::DuckClips {
            clip_ids: music,
            ranges,
            depth: DUCK_DEPTH,
            attack: 0.25,
            release: 0.6,
        });
        self.notify(&tf("duck.done", &[&format!("{DUCK_DEPTH:.0}")]), false);
    }

    // ── a vertical copy ──

    /// A vertical copy of the active timeline, for the phone (Royalty Studio
    /// fork): at 1080 x 1920 - 2160 x 3840 for a 4K one - and its own rate,
    /// each picture covering the frame and following its subject. The
    /// subjects are looked for on a thread of their own, by the person model
    /// on each picture twice a second; the copy is then made in one step by
    /// `Command::ReframeTimeline`, and becomes the timeline shown.
    pub fn reframe_vertical(&mut self) {
        if self.reframing {
            self.notify(&t("reframe.busy"), false);
            return;
        }
        let timeline = self.timeline().clone();
        let video = timeline.video;
        if video.height >= video.width {
            self.notify(&t("reframe.alreadyVertical"), false);
            return;
        }
        let requests: Vec<FollowRequest> = timeline
            .clips
            .iter()
            .filter(|clip| matches!(clip.kind, ClipKind::Video | ClipKind::Image))
            .filter_map(|clip| {
                let media = self.project().media_by_id(&clip.media_id)?;
                let (Some(width), Some(height)) = (media.width, media.height) else {
                    return None;
                };
                Some(FollowRequest {
                    clip_id: clip.id.clone(),
                    media_path: media.path.clone(),
                    media_size: (width, height),
                    still: clip.kind == ClipKind::Image || media.kind == MediaKind::Image,
                    source_start: clip.source_start,
                    source_span: clip.duration * clip.speed.max(1e-3),
                    crop: clip.crop.map_or([0.0; 4], |crop| {
                        [crop.left, crop.top, crop.right, crop.bottom]
                    }),
                })
            })
            .collect();
        let vertical = if video.height >= 2160 {
            VideoSettings {
                width: 2160,
                height: 3840,
                ..video
            }
        } else {
            VideoSettings {
                width: 1080,
                height: 1920,
                ..video
            }
        };
        self.reframing = true;
        self.notify(&tf("reframe.looking", &[&requests.len()]), false);
        let epoch = crate::host::project_epoch();
        let timeline_id = timeline.id.clone();
        let name = format!("{} · 9:16", timeline.name);
        let spawned = std::thread::Builder::new()
            .name("concat reframe".to_owned())
            .spawn(move || {
                let cancel = std::sync::atomic::AtomicBool::new(false);
                let found = concat_host::reframe::follow(&requests, &cancel, &mut |_| {});
                crate::host::on_ui(move |studio, _, _| {
                    studio.reframe_arrived(epoch, timeline_id, name, vertical, found);
                });
            });
        if let Err(error) = spawned {
            log::warn!("reframe: {error}");
            self.reframing = false;
        }
    }

    /// The subjects found for a vertical copy: the copy made, unless the
    /// project it was asked in has closed since.
    fn reframe_arrived(
        &mut self,
        epoch: u64,
        timeline_id: String,
        name: String,
        video: VideoSettings,
        found: Result<Vec<Follow>, String>,
    ) {
        self.reframing = false;
        if crate::host::project_epoch() != epoch {
            return;
        }
        let follow = match found {
            Ok(follow) => follow,
            Err(error) => {
                log::warn!("reframe: {error}");
                self.notify(&tf("reframe.failed", &[&error]), true);
                return;
            }
        };
        let followed = follow.len();
        log::info!("reframe: a vertical copy, {followed} pictures following their subject");
        self.apply(Command::ReframeTimeline {
            timeline_id,
            name,
            video,
            follow,
        });
        self.notify(&tf("reframe.done", &[&followed]), false);
    }

    // ── J and L cuts ──

    /// A J cut (`early`) or an L cut on the cut into `id` (Royalty Studio
    /// fork): the sound's cut rolled [`SPLIT_FRAMES`] earlier, so the next
    /// shot is heard before it is seen, or later, so the last one is still
    /// heard over it - the picture's cut left where it is. What the roll
    /// tool does with Alt held, in one click. `id` is the incoming picture
    /// or its sound.
    pub fn split_edit(&mut self, id: &str, early: bool) {
        let Some((left, right)) = self.sound_cut_into(id) else {
            self.notify(&t("splitEdit.noCut"), false);
            return;
        };
        let frames = SPLIT_FRAMES * self.frame_seconds();
        self.apply(Command::Scoped {
            scope: concat_project::Scope {
                alone: true,
                ..Default::default()
            },
            command: Box::new(Command::RollEdit {
                left_clip_id: left,
                right_clip_id: right,
                delta: if early { -frames } else { frames },
            }),
        });
    }

    /// The cut on a sound lane where `id`'s sound begins: the sound clip
    /// that ends there and the one that starts there. `id` is that sound,
    /// or a picture linked to it.
    pub(super) fn sound_cut_into(&self, id: &str) -> Option<(String, String)> {
        let clip = self.clip(id)?;
        let timeline = self.timeline();
        let sounds: Vec<&Clip> = if clip.kind == ClipKind::Audio {
            vec![clip]
        } else {
            let link = clip.link.as_deref()?;
            timeline
                .clips
                .iter()
                .map(|other| &**other)
                .filter(|other| {
                    other.kind == ClipKind::Audio && other.link.as_deref() == Some(link)
                })
                .collect()
        };
        sounds.into_iter().find_map(|sound| {
            timeline
                .clips
                .iter()
                .find(|other| {
                    other.id != sound.id
                        && other.track_id == sound.track_id
                        && (other.start + other.duration - sound.start).abs() < 1e-5
                })
                .map(|before| (before.id.clone(), sound.id.clone()))
        })
    }

    /// Where `clip` speaks, in timeline seconds, read off its waveform: the
    /// stretches louder than a tenth of its loud parts (20 dB under them,
    /// and never under -40 dBFS), gaps under 0.4 s closed and blips under
    /// 0.15 s dropped. None while its waveform is not read yet.
    fn speech_of(&self, clip: &Clip) -> Option<Vec<[f64; 2]>> {
        let peaks = self
            .peaks
            .get(&super::art_key(&clip.media_id, clip.audio_stream))?;
        let finest = peaks.finest();
        let steps = (clip.duration / VOICE_STEP).floor() as usize;
        let speed = clip.speed.max(1e-3);
        let levels: Vec<f32> = (0..steps)
            .map(|step| {
                let from = clip.source_start + step as f64 * VOICE_STEP * speed;
                let (low, high) = finest.extremes(from as f32, (from + VOICE_STEP * speed) as f32);
                high.max(-low) * clip.volume as f32
            })
            .collect();
        if levels.is_empty() {
            return Some(Vec::new());
        }
        let mut sorted = levels.clone();
        sorted.sort_by(f32::total_cmp);
        let loud = sorted[((sorted.len() as f32 * 0.9) as usize).min(sorted.len() - 1)];
        let threshold = (loud * 0.1).max(0.01);
        let mut runs: Vec<(f64, f64)> = Vec::new();
        for (step, level) in levels.iter().enumerate() {
            if *level <= threshold {
                continue;
            }
            let at = clip.start + step as f64 * VOICE_STEP;
            match runs.last_mut() {
                Some(last) if at - last.1 <= 0.4 => last.1 = at + VOICE_STEP,
                _ => runs.push((at, at + VOICE_STEP)),
            }
        }
        Some(
            runs.into_iter()
                .filter(|(start, end)| end - start >= 0.15)
                .map(|(start, end)| [start, end])
                .collect(),
        )
    }

    // ── roll, slip and slide ──

    /// A tool's tool: R, Y, U, and V or the same key again for the arrow.
    pub fn choose_tool(&mut self, tool: TimelineTool) {
        self.lanes.tool = if self.lanes.tool == tool {
            TimelineTool::Select
        } else {
            tool
        };
    }

    /// What a press on `clip` starts with the roll, slip or slide tool: a
    /// roll needs the edge pressed to meet a clip on the lane; the slip and
    /// the slide take the clip wherever it is pressed.
    pub(super) fn tool_gesture(&self, clip: &Clip, edge: i32) -> Option<Gesture> {
        match self.lanes.tool {
            TimelineTool::Roll => {
                let touching = |left: &Clip, right: &Clip| {
                    left.track_id == right.track_id
                        && (left.start + left.duration - right.start).abs() < 1e-5
                };
                let lane = self.timeline().clips.iter();
                let pair = match edge {
                    0 => lane
                        .filter(|other| touching(other, clip))
                        .map(|other| (other.id.clone(), clip.id.clone()))
                        .next(),
                    1 => lane
                        .filter(|other| touching(clip, other))
                        .map(|other| (clip.id.clone(), other.id.clone()))
                        .next(),
                    _ => None,
                };
                pair.map(|(left, right)| Gesture::Roll {
                    left,
                    right,
                    delta: 0.0,
                })
            }
            TimelineTool::Slip => Some(Gesture::Slip {
                clip: clip.id.clone(),
                speed: clip.speed,
                delta: 0.0,
            }),
            TimelineTool::Slide => Some(Gesture::Slide {
                clip: clip.id.clone(),
                delta: 0.0,
            }),
            _ => None,
        }
    }

    /// The command a tool gesture asks for, `delta` seconds of drag in.
    fn tool_command(gesture: &Gesture, delta: f64) -> Option<Command> {
        match gesture {
            Gesture::Roll { left, right, .. } => Some(Command::RollEdit {
                left_clip_id: left.clone(),
                right_clip_id: right.clone(),
                delta,
            }),
            // The picture follows the pointer: dragged right, it shows what
            // came before.
            Gesture::Slip { clip, speed, .. } => Some(Command::SlipClip {
                clip_id: clip.clone(),
                delta: -delta * speed,
            }),
            Gesture::Slide { clip, .. } => Some(Command::SlideClip {
                clip_id: clip.clone(),
                delta,
            }),
            _ => None,
        }
    }

    /// A tool gesture dragged `seconds` from its press: the edit worked out
    /// on a copy of the project, which the lanes and the monitor show.
    pub(super) fn tool_drag(&mut self, seconds: f32) {
        let delta = f64::from(seconds);
        let Some(command) = Self::tool_command(&self.gesture, delta) else {
            return;
        };
        // From the project as it is, not the echo: `project()` is the echo
        // while one is up, and each step of the drag would add to the last.
        let Some(mut echo) = self
            .session
            .as_ref()
            .map(|session| session.project().clone())
        else {
            return;
        };
        let mut mint = IdMint::default();
        mint.adopt_project(&echo);
        let command = self.scoped(command);
        if commands::apply(&mut echo, &mut mint, command).is_ok() {
            self.echo = Some(echo);
        }
        match &mut self.gesture {
            Gesture::Roll { delta: held, .. }
            | Gesture::Slip { delta: held, .. }
            | Gesture::Slide { delta: held, .. } => *held = delta,
            _ => {}
        }
        // The monitor shows the frame the edit lands on.
        self.request_preview();
    }

    /// A tool gesture let go: its edit, once, as one step.
    pub(super) fn tool_release(&mut self) {
        let gesture = std::mem::replace(&mut self.gesture, Gesture::None);
        self.echo = None;
        let delta = match &gesture {
            Gesture::Roll { delta, .. }
            | Gesture::Slip { delta, .. }
            | Gesture::Slide { delta, .. } => *delta,
            _ => return,
        };
        if delta.abs() < 1e-6 {
            return;
        }
        if let Some(command) = Self::tool_command(&gesture, delta) {
            self.apply(command);
        }
    }
}
