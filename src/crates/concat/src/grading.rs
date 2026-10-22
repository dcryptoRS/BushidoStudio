// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The colour wheels and curves, as the document holds them.
//!
//! A wheel is three numbers of its link's knobs - its puck, `<key>.x` and
//! `<key>.y` in the unit disc, and its master, `<key>.m` - keyed together as
//! one knob. A curve is its points, `<key>.<n>.x` and `<key>.<n>.y` in the
//! unit square, numbered from 0 with no gaps; a curve with none stored is
//! the line from black to white. What the shader makes of them is
//! concat-effects' `grade_wheels` and `grade_curves`; here is only how the
//! inspector reads and edits them.

use std::collections::BTreeMap;

use concat_effects::manifest::MAX_CURVE_POINTS;
use concat_effects::shader::{curve_points_resting, curve_value, hue_curve_value, monotone_slopes};
use concat_effects::{CurveAcross, Param};

/// What a curve is drawn across and where it rests with no points (Royalty
/// Studio fork): the line from black to white, or, for a curve whose
/// manifest says so, flat at a level and round the colour wheel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CurveShape {
    /// The level it rests flat at; `None` rests on the diagonal.
    pub rest: Option<f64>,
    /// Across hues, wrapping from its right end to its left.
    pub hue: bool,
}

impl CurveShape {
    /// The shape `param`'s manifest gives it.
    pub fn of(param: &Param) -> CurveShape {
        CurveShape {
            rest: param.rest,
            hue: param.across == CurveAcross::Hue,
        }
    }
}

/// The shape of curve `key` of the package `package` answers to; the line
/// from black to white for a package or a key the catalogue does not know.
pub fn shape_in(package: &str, key: &str) -> CurveShape {
    concat_effects::Catalogue::builtin()
        .packages()
        .find(|candidate| candidate.answers_to(package))
        .and_then(|found| found.manifest.param(key))
        .map(CurveShape::of)
        .unwrap_or_default()
}

/// Points a curve across hues starts with when it is first touched: one on
/// each primary and secondary, red to magenta, flat at its rest - where a
/// grading suite puts them, so a hue is pulled without bending its
/// neighbours.
const HUE_POINTS: usize = 6;

/// The document keys a wheel is kept under: its puck across and up, and
/// its master.
pub fn wheel_keys(key: &str) -> [String; 3] {
    [format!("{key}.x"), format!("{key}.y"), format!("{key}.m")]
}

/// The points a curve holds, each with the number it is stored under, in
/// that order; none when the curve is the straight line.
pub fn stored_points(params: &BTreeMap<String, f64>, key: &str) -> Vec<(usize, f64, f64)> {
    (0..MAX_CURVE_POINTS)
        .filter_map(|n| {
            let x = params.get(&format!("{key}.{n}.x"))?;
            let y = params.get(&format!("{key}.{n}.y"))?;
            Some((n, *x, *y))
        })
        .collect()
}

/// The curve drawn in a unit box, y downwards: SVG path commands through
/// 65 steps of what the shader works out, the line from black to white - or
/// the flat line `shape` rests at - for a curve with no points.
pub fn curve_path(params: &BTreeMap<String, f64>, key: &str, shape: CurveShape) -> String {
    let points = curve_points_resting(params, key, shape.rest);
    let slopes = monotone_slopes(&points);
    let mut path = String::new();
    for step in 0..=64 {
        let x = f64::from(step) / 64.0;
        let at = if shape.hue {
            hue_curve_value(&points, x)
        } else {
            curve_value(&points, &slopes, x)
        };
        let y = 1.0 - at.clamp(0.0, 1.0);
        let verb = if step == 0 { 'M' } else { 'L' };
        path.push_str(&format!("{verb} {x:.4} {y:.4} "));
    }
    path.trim_end().to_owned()
}

/// Puts point `point` of the curve at `(x, y)`, held to the unit square,
/// or a new point there when `point` is negative. A curve that is still the
/// straight line gets its two ends first, so the new point bends it rather
/// than making the whole curve - its flat line's ends for a curve that
/// rests, and for one across hues the six of [`HUE_POINTS`]; a curve with
/// every point it may have takes no more. Returns the number the point is
/// stored under.
pub fn set_curve_point(
    params: &mut BTreeMap<String, f64>,
    key: &str,
    point: i32,
    x: f64,
    y: f64,
    shape: CurveShape,
) -> Option<usize> {
    // Across hues the right end is red again, the left one's: a point is
    // kept short of it, so the two never stand on one hue.
    let right = if shape.hue { 1.0 - 1.0 / 512.0 } else { 1.0 };
    let (x, y) = (x.clamp(0.0, right), y.clamp(0.0, 1.0));
    let index = match usize::try_from(point) {
        Ok(index) if index < MAX_CURVE_POINTS => index,
        Ok(_) => return None,
        Err(_) => {
            let mut stored = stored_points(params, key);
            if stored.len() < 2 {
                clear_curve(params, key);
                let level = shape.rest.unwrap_or(0.0);
                let starts: Vec<(f64, f64)> = if shape.hue {
                    (0..HUE_POINTS)
                        .map(|n| (n as f64 / HUE_POINTS as f64, level))
                        .collect()
                } else {
                    vec![(0.0, level), (1.0, shape.rest.unwrap_or(1.0))]
                };
                for (n, (px, py)) in starts.into_iter().enumerate() {
                    params.insert(format!("{key}.{n}.x"), px);
                    params.insert(format!("{key}.{n}.y"), py);
                }
                stored = stored_points(params, key);
            }
            (0..MAX_CURVE_POINTS).find(|n| !stored.iter().any(|(held, ..)| held == n))?
        }
    };
    params.insert(format!("{key}.{index}.x"), x);
    params.insert(format!("{key}.{index}.y"), y);
    Some(index)
}

/// Takes point `point` off the curve and numbers the rest from 0 again, in
/// the order they were stored; a curve left with fewer than two points is
/// the straight line again.
pub fn remove_curve_point(params: &mut BTreeMap<String, f64>, key: &str, point: i32) {
    let kept: Vec<(f64, f64)> = stored_points(params, key)
        .into_iter()
        .filter(|(n, ..)| i32::try_from(*n).ok() != Some(point))
        .map(|(_, x, y)| (x, y))
        .collect();
    clear_curve(params, key);
    if kept.len() < 2 {
        return;
    }
    for (n, (x, y)) in kept.into_iter().enumerate() {
        params.insert(format!("{key}.{n}.x"), x);
        params.insert(format!("{key}.{n}.y"), y);
    }
}

/// Every point of the curve taken off: the straight line.
pub fn clear_curve(params: &mut BTreeMap<String, f64>, key: &str) {
    let prefix = format!("{key}.");
    params.retain(|held, _| {
        !(held.starts_with(&prefix)
            && held[prefix.len()..]
                .split_once('.')
                .is_some_and(|(n, _)| n.parse::<usize>().is_ok()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A first point bends a straight curve between its two ends; a moved
    /// point stays where it was stored; a removed one leaves the rest
    /// numbered from 0; and a curve left with one point is straight again.
    #[test]
    fn a_curve_is_edited_point_by_point() {
        let mut params = BTreeMap::from([("gain.m".to_owned(), 0.5)]);
        assert_eq!(
            set_curve_point(&mut params, "luma", -1, 0.5, 0.7, CurveShape::default()),
            Some(2)
        );
        assert_eq!(
            stored_points(&params, "luma"),
            [(0, 0.0, 0.0), (1, 1.0, 1.0), (2, 0.5, 0.7)]
        );
        assert_eq!(
            set_curve_point(&mut params, "luma", 1, 1.2, 0.9, CurveShape::default()),
            Some(1)
        );
        assert_eq!(
            stored_points(&params, "luma")[1],
            (1, 1.0, 0.9),
            "held to the square"
        );
        remove_curve_point(&mut params, "luma", 0);
        assert_eq!(
            stored_points(&params, "luma"),
            [(0, 1.0, 0.9), (1, 0.5, 0.7)],
            "numbered from 0 again"
        );
        remove_curve_point(&mut params, "luma", 1);
        assert!(stored_points(&params, "luma").is_empty(), "straight again");
        assert_eq!(
            params.get("gain.m"),
            Some(&0.5),
            "the other knobs untouched"
        );
        for _ in 0..MAX_CURVE_POINTS {
            set_curve_point(&mut params, "red", -1, 0.3, 0.3, CurveShape::default());
        }
        assert_eq!(stored_points(&params, "red").len(), MAX_CURVE_POINTS);
        assert_eq!(
            set_curve_point(&mut params, "red", -1, 0.4, 0.4, CurveShape::default()),
            None,
            "full"
        );
    }

    /// A curve is drawn as the shader works it out: from black to white
    /// when straight, through its points when bent.
    #[test]
    fn a_curve_is_drawn_as_the_shader_draws_it() {
        let straight = curve_path(&BTreeMap::new(), "luma", CurveShape::default());
        assert!(straight.starts_with("M 0.0000 1.0000 "), "{straight}");
        assert!(straight.ends_with("L 1.0000 0.0000"), "{straight}");
        let mut params = BTreeMap::new();
        set_curve_point(&mut params, "luma", -1, 0.5, 0.75, CurveShape::default());
        assert!(curve_path(&params, "luma", CurveShape::default()).contains("L 0.5000 0.2500"));
        assert_eq!(wheel_keys("lift"), ["lift.x", "lift.y", "lift.m"]);
    }

    /// A curve across hues rests flat in the middle, starts with a point on
    /// each primary and secondary when first touched, and is drawn round
    /// the wheel: a lifted red is lifted at both ends.
    #[test]
    fn a_hue_curve_is_flat_until_a_hue_is_pulled() {
        let shape = CurveShape {
            rest: Some(0.5),
            hue: true,
        };
        let flat = curve_path(&BTreeMap::new(), "hue_sat", shape);
        assert!(flat.starts_with("M 0.0000 0.5000 "), "{flat}");
        assert!(flat.ends_with("L 1.0000 0.5000"), "{flat}");
        let mut params = BTreeMap::new();
        // A point put down on a resting curve: the six first, then it.
        assert_eq!(
            set_curve_point(&mut params, "hue_sat", -1, 0.25, 0.5, shape),
            Some(HUE_POINTS)
        );
        // Red, the first of the six, lifted.
        set_curve_point(&mut params, "hue_sat", 0, 0.0, 0.8, shape);
        let lifted = curve_path(&params, "hue_sat", shape);
        assert!(lifted.starts_with("M 0.0000 0.2000 "), "{lifted}");
        assert!(lifted.ends_with("L 1.0000 0.2000"), "{lifted}");
        // Never right on the right end, which is red again.
        set_curve_point(&mut params, "hue_sat", 1, 1.0, 0.5, shape);
        assert!(stored_points(&params, "hue_sat")[1].1 < 1.0);
    }
}
