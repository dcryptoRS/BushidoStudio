// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! Slow and fast motion in the inspector (Royalty Studio fork): speed ramps
//! from presets or drawn, and the slow motion a clip shot at a high frame
//! rate gives without a frame repeated.
//!
//! A ramp is the engine's speed curve - `(at, speed)` points over the clip,
//! joined by straight lines, the stretch of the file it shows kept as it was
//! so the clip's length follows the curve's mean. The inspector draws it on
//! the colour panel's curve editor, the rate up the side on a log scale:
//! an eighth at the foot, one in the middle, eight at the top, so a halving
//! and a doubling are as far from as shot.
//!
//! Slow motion repeats frames once the file has no more to give: a clip
//! shot at 30 frames a second slowed to half on a 30 timeline shows each
//! frame twice. One shot at 120 slows to a quarter with every frame its
//! own, which is what real slow motion is; the panel says how far a clip
//! goes and puts it there in one click.

use concat_project::Command;
use concat_project::model::{Clip, SpeedPoint, ranges};

use slint::{ModelRc, SharedString, VecModel};

use super::Studio;
use crate::i18n::tf;
use crate::ui::CurvePointData;

/// The presets, as the Speed panel lists them after "Constant": a name for
/// the strings and the curve's points, at a fraction of the clip and in
/// times as shot. Shapes a reels editor reaches for - CapCut names them the
/// same way.
pub const RAMPS: [(&str, &[(f64, f64)]); 6] = [
    // Fast, a slow beat in the middle, fast again.
    (
        "montage",
        &[(0.0, 1.6), (0.3, 1.6), (0.5, 0.45), (0.7, 1.6), (1.0, 1.6)],
    ),
    // In fast, held slow on the hero moment, out fast.
    ("hero", &[(0.0, 2.0), (0.38, 0.3), (0.62, 0.3), (1.0, 2.0)]),
    // Faster in and out, slower and shorter in the middle.
    (
        "bullet",
        &[(0.0, 3.0), (0.42, 0.2), (0.58, 0.2), (1.0, 3.0)],
    ),
    // As shot, a burst of speed, as shot.
    (
        "jump",
        &[
            (0.0, 1.0),
            (0.38, 1.0),
            (0.45, 4.0),
            (0.55, 4.0),
            (0.62, 1.0),
            (1.0, 1.0),
        ],
    ),
    // In fast, settling to as shot.
    ("flashIn", &[(0.0, 4.0), (0.3, 1.0), (1.0, 1.0)]),
    // As shot, out fast.
    ("flashOut", &[(0.0, 1.0), (0.7, 1.0), (1.0, 4.0)]),
];

/// The most points a drawn ramp may have.
const MAX_POINTS: usize = 10;

/// The rate range the curve editor draws, in octaves either side of as shot.
const OCTAVES: f64 = 3.0;

/// A rate as a height on the curve editor, 0 at the foot.
pub fn height_of(speed: f64) -> f64 {
    ((speed.max(1e-6).log2() + OCTAVES) / (2.0 * OCTAVES)).clamp(0.0, 1.0)
}

/// A height on the curve editor as a rate, within the engine's range.
pub fn speed_at_height(height: f64) -> f64 {
    2f64.powf(height.clamp(0.0, 1.0) * 2.0 * OCTAVES - OCTAVES)
        .clamp(ranges::MIN_SPEED, ranges::MAX_SPEED)
}

/// Which preset `curve` is, as the panel's row counts them: 0 none (a
/// constant rate), 1.. the presets, -1 one of the editor's own drawing.
pub fn preset_of(curve: Option<&[SpeedPoint]>) -> i32 {
    let Some(points) = curve else {
        return 0;
    };
    RAMPS
        .iter()
        .position(|(_, preset)| {
            preset.len() == points.len()
                && preset.iter().zip(points).all(|((at, speed), point)| {
                    (at - point.at).abs() < 1e-6 && (speed - point.speed).abs() < 1e-6
                })
        })
        .map_or(-1, |index| index as i32 + 1)
}

/// The curve drawn in a unit box, y downwards, as SVG path commands: the
/// constant rate's level line when there is no curve.
pub fn ramp_path(clip: &Clip) -> String {
    let curve = clip
        .speed_curve
        .as_deref()
        .and_then(concat_project::speed::curve_of);
    let mut path = String::new();
    for step in 0..=64 {
        let x = f64::from(step) / 64.0;
        let speed = curve.as_ref().map_or(clip.speed, |curve| curve.speed_at(x));
        let y = 1.0 - height_of(speed);
        let verb = if step == 0 { 'M' } else { 'L' };
        path.push_str(&format!("{verb} {x:.4} {y:.4} "));
    }
    path.trim_end().to_owned()
}

/// The curve's points, for the editor's handles; none for a constant rate.
pub fn ramp_points(clip: &Clip) -> Vec<CurvePointData> {
    clip.speed_curve
        .as_deref()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, point)| CurvePointData {
            index: index as i32,
            x: point.at as f32,
            y: height_of(point.speed) as f32,
        })
        .collect()
}

impl Studio {
    /// The Speed panel's ramp and slow motion for the selected clip, onto
    /// the editor: the constant rate's level line and nothing said when no
    /// one clip is selected.
    pub(super) fn publish_speed(&self, editor: &crate::ui::Editor) {
        let clip = self.sole_selection().and_then(|id| self.clip(&id).cloned());
        let Some(clip) = clip else {
            editor.set_speed_ramp(0);
            editor.set_speed_ramp_path(SharedString::default());
            editor.set_speed_ramp_points(ModelRc::default());
            editor.set_speed_note(SharedString::default());
            editor.set_speed_smooth(0.0);
            return;
        };
        editor.set_speed_ramp(preset_of(clip.speed_curve.as_deref()));
        editor.set_speed_ramp_path(ramp_path(&clip).into());
        editor.set_speed_ramp_points(ModelRc::new(VecModel::from(ramp_points(&clip))));
        editor.set_speed_note(self.smooth_note(&clip).into());
        // The button shows while the clip is not there already.
        let smooth = self
            .smooth_slowest(&clip)
            .filter(|smooth| clip.speed_curve.is_some() || (clip.speed - smooth).abs() > 1e-3);
        editor.set_speed_smooth(smooth.unwrap_or(0.0) as f32);
    }

    /// The slowest `clip` goes with every frame its own: the timeline's
    /// rate over its file's, when the file was shot fast enough to give
    /// slow motion at all - half the rate or less. None otherwise.
    pub(super) fn smooth_slowest(&self, clip: &Clip) -> Option<f64> {
        let shot = self.project().media_by_id(&clip.media_id)?.frame_rate?;
        let plays = f64::from(self.frame_rate());
        (shot >= plays * 1.9 && plays > 0.0).then(|| (plays / shot).clamp(ranges::MIN_SPEED, 1.0))
    }

    /// What the Speed panel says about slow motion: how far this clip
    /// slows with its own frames, or that slower than as shot repeats them.
    /// Empty when there is nothing worth saying.
    pub(super) fn smooth_note(&self, clip: &Clip) -> String {
        let shot = self
            .project()
            .media_by_id(&clip.media_id)
            .and_then(|media| media.frame_rate)
            .unwrap_or(0.0);
        let slowest = clip
            .speed_curve
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|point| point.speed)
            .fold(clip.speed, f64::min);
        match self.smooth_slowest(clip) {
            Some(smooth) if slowest + 1e-6 < smooth => tf(
                "speed.belowSmooth",
                &[&format!("{shot:.0}"), &format!("{:.0}", smooth * 100.0)],
            ),
            Some(smooth) => tf(
                "speed.smoothUpTo",
                &[&format!("{shot:.0}"), &format!("{:.0}", smooth * 100.0)],
            ),
            None if slowest < 0.999 && shot > 0.0 => {
                tf("speed.repeatsFrames", &[&format!("{shot:.0}")])
            }
            None => String::new(),
        }
    }

    /// A preset picked on the Speed panel's ramp row: `0` the constant rate
    /// at the clip's mean, `1..` the presets.
    pub fn choose_ramp(&mut self, index: i32) {
        let Some(id) = self.sole_selection() else {
            return;
        };
        let Some(clip) = self.clip(&id).cloned() else {
            return;
        };
        let command = match usize::try_from(index) {
            Ok(0) => Command::SetClipSpeed {
                clip_id: id,
                speed: clip.speed,
            },
            Ok(n) if n <= RAMPS.len() => Command::SetClipSpeedCurve {
                clip_id: id,
                curve: Some(
                    RAMPS[n - 1]
                        .1
                        .iter()
                        .map(|&(at, speed)| SpeedPoint { at, speed })
                        .collect(),
                ),
            },
            _ => return,
        };
        self.apply(command);
    }

    /// A point of the drawn ramp moved to `(x, y)` on the editor, or put
    /// down there when `point` is negative. A constant rate gets its two
    /// ends first, at its own rate, so the new point bends it; the ends
    /// stay at the clip's ends, and only go up and down.
    pub fn ramp_point(&mut self, point: i32, x: f32, y: f32) {
        let Some(id) = self.sole_selection() else {
            return;
        };
        let Some(clip) = self.clip(&id).cloned() else {
            return;
        };
        let mut points = clip.speed_curve.clone().unwrap_or_else(|| {
            vec![
                SpeedPoint {
                    at: 0.0,
                    speed: clip.speed,
                },
                SpeedPoint {
                    at: 1.0,
                    speed: clip.speed,
                },
            ]
        });
        let (x, speed) = (f64::from(x).clamp(0.0, 1.0), speed_at_height(f64::from(y)));
        match usize::try_from(point) {
            Ok(index) if index < points.len() => {
                let last = points.len() - 1;
                let at = match index {
                    0 => 0.0,
                    i if i == last => 1.0,
                    // Between its neighbours, so the points stay in order.
                    i => x.clamp(points[i - 1].at + 0.01, points[i + 1].at - 0.01),
                };
                points[index] = SpeedPoint { at, speed };
            }
            Ok(_) => return,
            Err(_) => {
                if points.len() >= MAX_POINTS || x <= 0.005 || x >= 0.995 {
                    return;
                }
                points.push(SpeedPoint { at: x, speed });
                points.sort_by(|a, b| a.at.total_cmp(&b.at));
                points.dedup_by(|later, earlier| (later.at - earlier.at).abs() < 0.01);
            }
        }
        self.apply(Command::SetClipSpeedCurve {
            clip_id: id,
            curve: Some(points),
        });
    }

    /// A point of the drawn ramp taken off; the ends stay.
    pub fn ramp_remove(&mut self, point: i32) {
        let Some(id) = self.sole_selection() else {
            return;
        };
        let Some(mut points) = self.clip(&id).and_then(|clip| clip.speed_curve.clone()) else {
            return;
        };
        match usize::try_from(point) {
            Ok(index) if index > 0 && index + 1 < points.len() => {
                points.remove(index);
            }
            _ => return,
        }
        self.apply(Command::SetClipSpeedCurve {
            clip_id: id,
            curve: Some(points),
        });
    }

    /// Real slow motion: the selected clip slowed as far as its file's frame
    /// rate allows with no frame shown twice.
    pub fn smooth_slow(&mut self) {
        let Some(id) = self.sole_selection() else {
            return;
        };
        let Some(speed) = self.clip(&id).and_then(|clip| self.smooth_slowest(clip)) else {
            return;
        };
        self.apply(Command::SetClipSpeed { clip_id: id, speed });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_is_drawn_on_octaves_with_as_shot_in_the_middle() {
        assert!((height_of(1.0) - 0.5).abs() < 1e-9);
        assert!((height_of(8.0) - 1.0).abs() < 1e-9);
        assert!((height_of(0.125)).abs() < 1e-9);
        for speed in [0.25, 0.5, 1.0, 2.0, 4.0] {
            assert!((speed_at_height(height_of(speed)) - speed).abs() < 1e-9);
        }
    }

    #[test]
    fn a_preset_is_known_again_and_a_drawing_is_its_own() {
        assert_eq!(preset_of(None), 0);
        for (n, (_, points)) in RAMPS.iter().enumerate() {
            let curve: Vec<SpeedPoint> = points
                .iter()
                .map(|&(at, speed)| SpeedPoint { at, speed })
                .collect();
            assert_eq!(preset_of(Some(&curve)), n as i32 + 1);
            // Every preset is a curve the engine takes.
            assert!(concat_project::speed::curve_of(&curve).is_some());
        }
        let drawn = [
            SpeedPoint {
                at: 0.0,
                speed: 1.0,
            },
            SpeedPoint {
                at: 1.0,
                speed: 0.5,
            },
        ];
        assert_eq!(preset_of(Some(&drawn)), -1);
    }
}
