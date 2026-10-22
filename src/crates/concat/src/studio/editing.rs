// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! The cutting room's verbs on the window (Royalty Studio fork): markers,
//! the jump from cut to cut, in and out points, the three-point insert and
//! overwrite, lift and extract, J-K-L, and the roll, slip and slide tools.
//! The edits themselves are `concat_project`'s commands; here is when they
//! are asked for and how a drag shows them before it lets go.

use concat_project::Command;
use concat_project::commands::{self, IdMint};
use concat_project::model::{Clip, MarkerColor};

use super::{Gesture, Studio};
use crate::format::frames_timecode;
use crate::i18n::{t, tf};
use crate::ui::{MarkerData, MarkerSheetData, TimelineTool};

/// The fastest J or L goes, in times real time.
const SHUTTLE_TOP: f32 = 8.0;

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
