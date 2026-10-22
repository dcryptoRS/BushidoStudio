// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! Where a picture's subject is as it plays (Royalty Studio fork): what a
//! vertical copy of a horizontal edit follows, through
//! `Command::ReframeTimeline`.
//!
//! Each picture is looked at twice a second of its source by the person
//! model compiled into the app - no download, and on the CPU, where ONNX
//! Runtime runs on Linux - and the subject's place across it is the middle
//! of the mask's weight. What comes out is not that place frame by frame:
//! a camera that chased every lean would never be still. It is what an
//! operator would do with it - hold while the subject stays near the
//! middle, follow smoothly once they leave it, and settle again - drawn
//! with as few keys as keep its shape.

use std::sync::atomic::{AtomicBool, Ordering};

use concat_core::time::{FrameRate, Rational};
use concat_media::{DecodeOptions, Decoder, FrameSource};
use concat_project::Follow;
use concat_vision::{Mask, Segmenter};

/// Looks a second of source.
const LOOKS_PER_SECOND: i64 = 2;

/// How little of a picture a subject may be and still count: under this
/// share of the mask's whole weight the picture has no one in it.
const LEAST_PRESENCE: f64 = 0.01;

/// How far from the camera's middle, as a share of the picture's width, a
/// subject goes before the camera moves.
const DEAD_ZONE: f64 = 0.06;

/// How much of the way past the dead zone the camera goes in one look.
const FOLLOW: f64 = 0.35;

/// The most the camera moves in one look, as a share of the picture's
/// width: a subject who reappears across the frame is panned to, not cut to.
const MAX_STEP: f64 = 0.05;

/// How far, as a share of the picture's width, the drawn path may stray
/// from the camera's before another key is kept.
const TOLERANCE: f64 = 0.006;

/// One picture to follow.
#[derive(Clone, Debug)]
pub struct FollowRequest {
    /// The clip, as the command names it.
    pub clip_id: String,
    /// The media file.
    pub media_path: String,
    /// Its picture size.
    pub media_size: (u32, u32),
    /// A still: one look answers for all of it.
    pub still: bool,
    /// Where in the file the clip starts, in seconds.
    pub source_start: f64,
    /// How many seconds of the file it shows.
    pub source_span: f64,
    /// What its crop takes off each edge, as fractions: left, top, right,
    /// bottom.
    pub crop: [f64; 4],
}

/// Where each picture's subject is, for every request `cancel` leaves the
/// time to: a picture with no one in it is left out, and is centred.
/// `progress` hears how far through, `0..=1`.
pub fn follow(
    requests: &[FollowRequest],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<Vec<Follow>, String> {
    let segmenter = Segmenter::selfie()?;
    let mut paths = Vec::new();
    for (n, request) in requests.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("reframing cancelled".to_owned());
        }
        let looks = looks_at(&segmenter, request, cancel)?;
        let points = camera(&looks);
        if !points.is_empty() {
            paths.push(Follow {
                clip_id: request.clip_id.clone(),
                points,
            });
        }
        progress((n + 1) as f32 / requests.len().max(1) as f32);
    }
    Ok(paths)
}

/// The subject's place across `request`'s picture at each look, as
/// `(at, x)`: `at` a fraction of the clip, `x` of the picture's crop; none
/// where no one is there.
fn looks_at(
    segmenter: &Segmenter,
    request: &FollowRequest,
    cancel: &AtomicBool,
) -> Result<Vec<(f64, Option<f64>)>, String> {
    let (width, height) = request.media_size;
    let (w, h) = segmenter.input_size(width, height);
    let count = if request.still {
        1
    } else {
        ((request.source_span * LOOKS_PER_SECOND as f64).ceil() as u64).max(1)
    };
    let start = Rational::new(
        (request.source_start.max(0.0) * 1000.0).round() as i64,
        1000,
    );
    let mut options = DecodeOptions::default().scaled_to(w, h).limited_to(count);
    if !request.still {
        options = options
            .starting_at(start)
            .at_rate(FrameRate::new(Rational::new(LOOKS_PER_SECOND, 1)));
    }
    let mut decoder =
        Decoder::open(&request.media_path, &options).map_err(|error| error.to_string())?;
    segmenter.begin();
    let mut looks = Vec::new();
    for n in 0..count {
        if cancel.load(Ordering::Relaxed) {
            return Err("reframing cancelled".to_owned());
        }
        let Some(frame) = decoder.next_frame().map_err(|error| error.to_string())? else {
            break;
        };
        let mask = segmenter.mask(&frame)?;
        let at = if request.still || request.source_span <= 0.0 {
            0.0
        } else {
            (n as f64 / LOOKS_PER_SECOND as f64 / request.source_span).min(1.0)
        };
        looks.push((at, subject_x(&mask, request.crop)));
    }
    Ok(looks)
}

/// The middle of the mask's weight across the part of the picture its crop
/// keeps, as a fraction of that part; none when the subject is too little
/// of it to count.
fn subject_x(mask: &Mask, [left, top, right, bottom]: [f64; 4]) -> Option<f64> {
    let (width, height) = (mask.width() as f64, mask.height() as f64);
    let from_x = (left * width).floor().max(0.0) as i64;
    let to_x = ((1.0 - right) * width).ceil().min(width) as i64;
    let from_y = (top * height).floor().max(0.0) as i64;
    let to_y = ((1.0 - bottom) * height).ceil().min(height) as i64;
    if to_x <= from_x || to_y <= from_y {
        return None;
    }
    let (mut weight, mut moment) = (0.0, 0.0);
    for y in from_y..to_y {
        for x in from_x..to_x {
            let value = f64::from(mask.at(x, y)) / 255.0;
            weight += value;
            moment += value * (x as f64 + 0.5);
        }
    }
    let area = ((to_x - from_x) * (to_y - from_y)) as f64;
    if weight / area < LEAST_PRESENCE {
        return None;
    }
    let middle = moment / weight;
    Some(((middle - from_x as f64) / (to_x - from_x) as f64).clamp(0.0, 1.0))
}

/// What an operator's camera does with `looks`: the gaps where no one was
/// held at the nearest place someone was, the flicker of single looks
/// taken out, a hold while the subject stays near the middle and a smooth
/// follow once they leave it, never faster than a pan - drawn with as few
/// `[at, x]` keys as keep its shape. Empty when no one was ever there.
pub fn camera(looks: &[(f64, Option<f64>)]) -> Vec<[f64; 2]> {
    let Some(first) = looks.iter().find_map(|(_, x)| *x) else {
        return Vec::new();
    };
    // Held over the gaps: the last place seen, the first before any.
    let mut seen = first;
    let filled: Vec<(f64, f64)> = looks
        .iter()
        .map(|(at, x)| {
            if let Some(x) = x {
                seen = *x;
            }
            (*at, seen)
        })
        .collect();
    // The median of each look and its neighbours, two either side.
    let steadied: Vec<(f64, f64)> = (0..filled.len())
        .map(|n| {
            let from = n.saturating_sub(2);
            let to = (n + 3).min(filled.len());
            let mut near: Vec<f64> = filled[from..to].iter().map(|(_, x)| *x).collect();
            near.sort_by(f64::total_cmp);
            (filled[n].0, near[near.len() / 2])
        })
        .collect();
    // The operator: still inside the dead zone, after the subject beyond
    // it by a share of the way each look.
    let mut place = steadied[0].1;
    let path: Vec<[f64; 2]> = steadied
        .iter()
        .map(|(at, x)| {
            let off = x - place;
            if off.abs() > DEAD_ZONE {
                place += ((off - DEAD_ZONE * off.signum()) * FOLLOW).clamp(-MAX_STEP, MAX_STEP);
            }
            [*at, place]
        })
        .collect();
    simplified(&path, TOLERANCE)
}

/// `path` with the keys a straight line between its neighbours already
/// draws to within `tolerance` taken out: Ramer, Douglas and Peucker's.
fn simplified(path: &[[f64; 2]], tolerance: f64) -> Vec<[f64; 2]> {
    if path.len() < 3 {
        return path.to_vec();
    }
    let mut keep = vec![false; path.len()];
    keep[0] = true;
    keep[path.len() - 1] = true;
    let mut stack = vec![(0, path.len() - 1)];
    while let Some((from, to)) = stack.pop() {
        let ([a_at, a_x], [b_at, b_x]) = (path[from], path[to]);
        let mut worst = (0.0, from);
        for (n, [at, x]) in path.iter().enumerate().take(to).skip(from + 1) {
            let along = if (b_at - a_at).abs() < 1e-12 {
                a_x
            } else {
                a_x + (b_x - a_x) * (at - a_at) / (b_at - a_at)
            };
            let off = (x - along).abs();
            if off > worst.0 {
                worst = (off, n);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((from, worst.1));
            stack.push((worst.1, to));
        }
    }
    path.iter()
        .zip(keep)
        .filter_map(|(point, kept)| kept.then_some(*point))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looks(xs: &[Option<f64>]) -> Vec<(f64, Option<f64>)> {
        let last = (xs.len() - 1).max(1) as f64;
        xs.iter()
            .enumerate()
            .map(|(n, x)| (n as f64 / last, *x))
            .collect()
    }

    #[test]
    fn a_subject_swaying_by_the_middle_is_held_still() {
        let swaying: Vec<Option<f64>> = (0..40)
            .map(|n| Some(0.5 + if n % 2 == 0 { 0.03 } else { -0.03 }))
            .collect();
        let path = camera(&looks(&swaying));
        assert_eq!(path.len(), 2, "{path:?}");
        assert!((path[0][1] - path[1][1]).abs() < 1e-9);
    }

    #[test]
    fn a_subject_who_walks_across_is_followed_and_settled_on() {
        // In the middle for ten looks, then over to 0.8 and staying.
        let walk: Vec<Option<f64>> = (0..40)
            .map(|n| Some(if n < 10 { 0.5 } else { 0.8 }))
            .collect();
        let path = camera(&looks(&walk));
        let end = path.last().expect("a path")[1];
        // Settled near the subject, short of it by the dead zone.
        assert!((end - (0.8 - DEAD_ZONE)).abs() < 0.01, "{end}");
        // Still at the start, and never past where it was going.
        assert!((path[0][1] - 0.5).abs() < 1e-9);
        assert!(path.iter().all(|[_, x]| *x <= 0.8));
        assert!(path.len() < 40, "simplified: {}", path.len());
    }

    #[test]
    fn nobody_there_is_nothing_to_follow_and_a_gap_is_held() {
        assert!(camera(&looks(&[None, None, None])).is_empty());
        let gap = camera(&looks(&[Some(0.3), None, None, Some(0.3)]));
        assert!(gap.iter().all(|[_, x]| (*x - 0.3).abs() < 1e-9), "{gap:?}");
    }

    #[test]
    fn the_subject_is_found_where_the_mask_is() {
        // A mask with a block of person on its right quarter.
        let (width, height) = (64u32, 32u32);
        let mut data = vec![0u8; (width * height) as usize];
        for y in 0..height {
            for x in 48..56 {
                data[(y * width + x) as usize] = 255;
            }
        }
        let mask = Mask::from_bytes(width, height, data).expect("a mask");
        let x = subject_x(&mask, [0.0; 4]).expect("someone");
        assert!((x - 52.0 / 64.0).abs() < 1e-9, "{x}");
        // Cropped to its right half, the same person is further left in it.
        let cropped = subject_x(&mask, [0.5, 0.0, 0.0, 0.0]).expect("someone");
        assert!((cropped - 20.0 / 32.0).abs() < 1e-9, "{cropped}");
        assert!(subject_x(&Mask::filled(8, 8, 0), [0.0; 4]).is_none());
    }
}
