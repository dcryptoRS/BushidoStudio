// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! Loudness (Royalty Studio fork): a sound's integrated loudness by ITU-R
//! BS.1770-4 and EBU R128, in LUFS, and its sample peak - what a delivery is
//! normalised against: -14 LUFS for the platforms, -23 for broadcast.
//!
//! The measure, as the recommendation has it: each channel K-weighted (a
//! high shelf for the head, a high-pass for the low end), its mean square
//! taken over 400 ms blocks that overlap by three quarters, the channels
//! summed, and the blocks gated twice - below -70 LUFS is silence, and below
//! ten units under the loudness of what is left is too quiet to count -
//! before the rest are averaged into one figure.
//!
//! The true peak, as Annex 2 has it: each channel interpolated to four
//! times its rate by the recommendation's own 48-tap filter, and the
//! highest of those - a peak between two samples, which the samples alone
//! do not show and a decoder or a speaker will.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};
use crate::samples::{AudioDecoder, AudioOptions, SampleFormat};

/// The rate the measure runs at: the K-weighting's coefficients are the
/// recommendation's for it, and the decoder resamples anything else.
pub const RATE: u32 = 48_000;

/// What a sound measures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    /// Integrated loudness, in LUFS; negative infinity for silence.
    pub integrated: f64,
    /// The highest sample, in dBFS; negative infinity for silence.
    pub peak: f64,
    /// The highest point of the waveform between the samples as well, in
    /// dBTP: never under `peak`; negative infinity for silence.
    pub true_peak: f64,
}

/// One second-order section, in transposed direct form II.
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    const fn new(b: [f64; 3], a: [f64; 2]) -> Biquad {
        Biquad { b, a, z: [0.0; 2] }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The K-weighting at 48 kHz: BS.1770's two stages.
fn k_weighting() -> [Biquad; 2] {
    [
        Biquad::new(
            [
                1.535_124_859_586_97,
                -2.691_696_189_406_38,
                1.198_392_810_852_85,
            ],
            [-1.690_659_293_182_41, 0.732_480_774_215_85],
        ),
        Biquad::new(
            [1.0, -2.0, 1.0],
            [-1.990_047_454_833_98, 0.990_072_250_366_21],
        ),
    ]
}

/// BS.1770-4 Annex 2's interpolating filter: four phases of twelve taps,
/// each one the waveform a quarter of a sample further on.
const TRUE_PEAK_PHASES: [[f64; 12]; 4] = [
    [
        0.001_708_984_375,
        0.010_986_328_125,
        -0.019_653_320_312_5,
        0.033_203_125,
        -0.059_448_242_187_5,
        0.137_329_101_562_5,
        0.972_167_968_75,
        -0.102_294_921_875,
        0.047_607_421_875,
        -0.026_611_328_125,
        0.014_892_578_125,
        -0.008_300_781_25,
    ],
    [
        -0.029_174_804_687_5,
        0.029_296_875,
        -0.051_757_812_5,
        0.089_111_328_125,
        -0.166_503_906_25,
        0.465_087_890_625,
        0.779_785_156_25,
        -0.200_317_382_812_5,
        0.101_562_5,
        -0.058_227_539_062_5,
        0.033_081_054_687_5,
        -0.018_920_898_437_5,
    ],
    [
        -0.018_920_898_437_5,
        0.033_081_054_687_5,
        -0.058_227_539_062_5,
        0.101_562_5,
        -0.200_317_382_812_5,
        0.779_785_156_25,
        0.465_087_890_625,
        -0.166_503_906_25,
        0.089_111_328_125,
        -0.051_757_812_5,
        0.029_296_875,
        -0.029_174_804_687_5,
    ],
    [
        -0.008_300_781_25,
        0.014_892_578_125,
        -0.026_611_328_125,
        0.047_607_421_875,
        -0.102_294_921_875,
        0.137_329_101_562_5,
        0.972_167_968_75,
        -0.059_448_242_187_5,
        0.033_203_125,
        -0.019_653_320_312_5,
        0.010_986_328_125,
        0.001_708_984_375,
    ],
];

/// One channel's last twelve samples, newest first, for the interpolator.
#[derive(Clone, Copy, Default)]
struct History {
    samples: [f64; 12],
}

impl History {
    /// `x` taken in; the highest the four interpolated points reach.
    fn push(&mut self, x: f64) -> f64 {
        self.samples.copy_within(0..11, 1);
        self.samples[0] = x;
        TRUE_PEAK_PHASES
            .iter()
            .map(|taps| {
                taps.iter()
                    .zip(&self.samples)
                    .map(|(tap, sample)| tap * sample)
                    .sum::<f64>()
                    .abs()
            })
            .fold(0.0, f64::max)
    }
}

/// A running measure: samples pushed in, the figures taken at the end.
pub struct Meter {
    channels: usize,
    filters: Vec<[Biquad; 2]>,
    histories: Vec<History>,
    /// Summed squares of the 100 ms quarter-block being filled, by channel.
    quarter: Vec<f64>,
    filled: usize,
    /// Each finished quarter-block's mean square, summed over channels.
    quarters: Vec<f64>,
    peak: f64,
    true_peak: f64,
}

/// Samples in a quarter of a block: 100 ms.
const QUARTER: usize = (RATE / 10) as usize;

impl Meter {
    /// A meter for interleaved samples of `channels` channels at [`RATE`].
    pub fn new(channels: usize) -> Meter {
        let channels = channels.max(1);
        Meter {
            channels,
            filters: vec![k_weighting(); channels],
            histories: vec![History::default(); channels],
            quarter: vec![0.0; channels],
            filled: 0,
            quarters: Vec::new(),
            peak: 0.0,
            true_peak: 0.0,
        }
    }

    /// Interleaved samples, in [-1, 1].
    pub fn push(&mut self, samples: &[f32]) {
        for frame in samples.chunks_exact(self.channels) {
            for (channel, sample) in frame.iter().enumerate() {
                let x = f64::from(*sample);
                self.peak = self.peak.max(x.abs());
                self.true_peak = self.true_peak.max(self.histories[channel].push(x));
                let [first, second] = &mut self.filters[channel];
                let y = second.run(first.run(x));
                self.quarter[channel] += y * y;
            }
            self.filled += 1;
            if self.filled == QUARTER {
                let power: f64 = self.quarter.iter().map(|sum| sum / QUARTER as f64).sum();
                self.quarters.push(power);
                self.quarter.iter_mut().for_each(|sum| *sum = 0.0);
                self.filled = 0;
            }
        }
    }

    /// The figures for everything pushed.
    pub fn finish(&self) -> Loudness {
        let loudness = |power: f64| -0.691 + 10.0 * power.log10();
        // Every 400 ms block, one quarter apart.
        let blocks: Vec<f64> = self
            .quarters
            .windows(4)
            .map(|four| four.iter().sum::<f64>() / 4.0)
            .collect();
        let mean = |powers: &[f64]| powers.iter().sum::<f64>() / powers.len().max(1) as f64;
        let audible: Vec<f64> = blocks
            .iter()
            .copied()
            .filter(|power| loudness(*power) > -70.0)
            .collect();
        let integrated = if audible.is_empty() {
            f64::NEG_INFINITY
        } else {
            let relative = loudness(mean(&audible)) - 10.0;
            let gated: Vec<f64> = audible
                .into_iter()
                .filter(|power| loudness(*power) > relative)
                .collect();
            if gated.is_empty() {
                f64::NEG_INFINITY
            } else {
                loudness(mean(&gated))
            }
        };
        Loudness {
            integrated,
            peak: 20.0 * self.peak.log10(),
            true_peak: 20.0 * self.true_peak.max(self.peak).log10(),
        }
    }
}

/// The loudness of a file's sound, its first audio stream decoded at
/// [`RATE`] in stereo. `cancel` is looked at between frames.
pub fn measure(path: &Path, cancel: &AtomicBool) -> Result<Loudness> {
    let options = AudioOptions {
        rate: RATE,
        channels: 2,
        format: SampleFormat::F32,
        ..AudioOptions::default()
    };
    let mut decoder = AudioDecoder::open(path, &options)?;
    let mut meter = Meter::new(2);
    while let Some(samples) = decoder.next_f32()? {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        meter.push(&samples);
    }
    Ok(meter.finish())
}

/// The gain, in dB, that brings `measured` to `target` LUFS; none for
/// silence, which no gain brings anywhere.
pub fn gain_to(measured: &Loudness, target: f64) -> Option<f64> {
    measured
        .integrated
        .is_finite()
        .then_some(target - measured.integrated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `seconds` of a stereo sine at `hz`, peaking at `amplitude`.
    fn sine(hz: f64, amplitude: f64, seconds: f64) -> Vec<f32> {
        let count = (seconds * f64::from(RATE)) as usize;
        (0..count)
            .flat_map(|n| {
                let x = (amplitude
                    * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(RATE)).sin())
                    as f32;
                [x, x]
            })
            .collect()
    }

    fn measured(samples: &[f32]) -> Loudness {
        let mut meter = Meter::new(2);
        meter.push(samples);
        meter.finish()
    }

    #[test]
    fn a_stereo_sine_at_minus_twenty_reads_minus_twenty() {
        // BS.1770's own check: a 997 Hz tone at -20 dBFS in both channels
        // of a stereo programme is -20 LUFS, to a tenth.
        let tone = measured(&sine(997.0, 0.1, 10.0));
        assert!((tone.integrated + 20.0).abs() < 0.1, "{}", tone.integrated);
        assert!((tone.peak + 20.0).abs() < 0.01, "{}", tone.peak);
    }

    #[test]
    fn twice_the_amplitude_is_six_units_louder() {
        let quiet = measured(&sine(1000.0, 0.05, 8.0));
        let loud = measured(&sine(1000.0, 0.1, 8.0));
        let step = loud.integrated - quiet.integrated;
        assert!((step - 6.02).abs() < 0.05, "{step}");
    }

    #[test]
    fn silence_is_gated_out_and_reads_nothing_alone() {
        let tone = sine(1000.0, 0.1, 10.0);
        let mut with_silence = tone.clone();
        with_silence.extend(std::iter::repeat_n(0.0, 10 * RATE as usize * 2));
        let alone = measured(&tone).integrated;
        let padded = measured(&with_silence).integrated;
        assert!((alone - padded).abs() < 0.1, "{alone} against {padded}");
        let silent = measured(&vec![0.0; 4 * RATE as usize * 2]);
        assert_eq!(silent.integrated, f64::NEG_INFINITY);
        assert_eq!(gain_to(&silent, -14.0), None);
    }

    #[test]
    fn a_peak_between_the_samples_is_found() {
        // A quarter of the rate, a quarter-turn off: every sample lands at
        // 0.707 of the crest, which falls halfway between two of them.
        let count = 4 * RATE as usize;
        let samples: Vec<f32> = (0..count)
            .flat_map(|n| {
                let x = (std::f64::consts::FRAC_PI_2 * n as f64 + std::f64::consts::FRAC_PI_4).sin()
                    as f32;
                [x, x]
            })
            .collect();
        let read = measured(&samples);
        assert!((read.peak + 3.01).abs() < 0.05, "{}", read.peak);
        assert!(
            read.true_peak > -0.5 && read.true_peak < 0.5,
            "{}",
            read.true_peak
        );
    }

    #[test]
    fn a_low_tone_peaks_where_its_samples_do() {
        let tone = measured(&sine(997.0, 0.1, 4.0));
        assert!(
            (tone.true_peak - tone.peak).abs() < 0.1,
            "{} against {}",
            tone.true_peak,
            tone.peak
        );
    }

    #[test]
    fn the_gain_to_a_target_is_the_difference() {
        let tone = measured(&sine(997.0, 0.1, 10.0));
        let gain = gain_to(&tone, -14.0).expect("a gain");
        assert!((gain - 6.0).abs() < 0.1, "{gain}");
    }
}
