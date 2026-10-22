// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Pauses and words (Royalty Studio fork): which stretches of a recording
//! to keep when its pauses are cut, and how its words are grouped into
//! captions, from the times a transcriber heard each word.
//!
//! The rules and their defaults are those of CutAndCaption, the panel for
//! DaVinci Resolve these tools come from: a pause is cut once it is longer
//! than `min_silence`, each kept stretch keeps a breath of air before its
//! first word and a longer one after its last - the tail of a consonant is
//! what decides whether a cut sounds natural or robotic - and a stretch too
//! short to be a word on its own is dropped. Captions are a few short words
//! at a time, broken at a pause and at the end of a sentence, never shown
//! for less than a blink, and held across a short gap rather than flashing
//! off and on.

/// One word as it was heard: when it started and ended, in seconds from
/// the start of the recording, and what it was.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    /// When it began, in seconds.
    pub start: f64,
    /// When it ended, in seconds.
    pub end: f64,
    /// What it was.
    pub text: String,
}

/// How pauses are cut; see the module.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SilenceRules {
    /// A pause shorter than this stays.
    pub min_silence: f64,
    /// Air kept before a stretch's first word.
    pub pad_in: f64,
    /// Air kept after a stretch's last word.
    pub pad_out: f64,
    /// A kept stretch shorter than this is dropped as a blip.
    pub min_keep: f64,
}

impl Default for SilenceRules {
    fn default() -> Self {
        SilenceRules {
            min_silence: 0.35,
            pad_in: 0.10,
            pad_out: 0.20,
            min_keep: 0.20,
        }
    }
}

/// The stretches of `[0, length)` to keep: the words, joined where the
/// pause between them is shorter than `min_silence`, each run padded and
/// held inside the recording, runs the padding made touch joined, and runs
/// shorter than `min_keep` dropped. In time order, never overlapping. The
/// silence before the first word and after the last goes too.
pub fn kept(words: &[Word], length: f64, rules: &SilenceRules) -> Vec<(f64, f64)> {
    let mut heard: Vec<(f64, f64)> = words
        .iter()
        .filter(|word| word.end > word.start && word.end > 0.0 && word.start < length)
        .map(|word| (word.start.max(0.0), word.end.min(length)))
        .collect();
    heard.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Words close together are one run of speech.
    let mut runs: Vec<(f64, f64)> = Vec::new();
    for (start, end) in heard {
        match runs.last_mut() {
            Some(last) if start - last.1 < rules.min_silence => last.1 = last.1.max(end),
            _ => runs.push((start, end)),
        }
    }
    // Air either side, inside the recording, and runs the air made meet
    // joined again.
    let mut padded: Vec<(f64, f64)> = Vec::new();
    for (start, end) in runs {
        let (start, end) = (
            (start - rules.pad_in).max(0.0),
            (end + rules.pad_out).min(length),
        );
        match padded.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => padded.push((start, end)),
        }
    }
    padded
        .into_iter()
        .filter(|(start, end)| end - start >= rules.min_keep)
        .collect()
}

/// The stretches of `[0, length)` [`kept`] leaves out: what a cut of the
/// pauses takes away, in time order.
pub fn removed(words: &[Word], length: f64, rules: &SilenceRules) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut at = 0.0;
    for (start, end) in kept(words, length, rules) {
        if start > at {
            out.push((at, start));
        }
        at = end;
    }
    if length > at {
        out.push((at, length));
    }
    // A sliver under a frame is a rounding, not a pause.
    out.retain(|(start, end)| end - start > 1e-3);
    out
}

/// How words are grouped into captions; see the module.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptionRules {
    /// The most words on screen at once.
    pub max_words: usize,
    /// The most characters on screen at once, spaces counted.
    pub max_chars: usize,
    /// A pause at least this long starts a new caption.
    pub break_on_pause: f64,
    /// No caption is shown for less than this: shorter reads as a flicker.
    pub min_duration: f64,
    /// A gap to the next caption shorter than this is filled by holding
    /// the one before.
    pub max_fill: f64,
}

impl Default for CaptionRules {
    fn default() -> Self {
        CaptionRules {
            max_words: 3,
            max_chars: 22,
            break_on_pause: 0.30,
            min_duration: 0.16,
            max_fill: 0.40,
        }
    }
}

/// One caption: from when to when, in the recording's seconds, and its words.
#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    /// When it shows, in seconds.
    pub start: f64,
    /// When it goes, in seconds.
    pub end: f64,
    /// Its words, a space between each.
    pub text: String,
}

/// The words as captions, a few at a time: a caption closes when the next
/// word would pass `max_words` or `max_chars`, after a pause of
/// `break_on_pause`, and after a word that ends a sentence. Each lasts at
/// least `min_duration` where the next leaves room, and holds across a gap
/// to the next shorter than `max_fill`.
pub fn captions(words: &[Word], rules: &CaptionRules) -> Vec<Caption> {
    let mut heard: Vec<&Word> = words
        .iter()
        .filter(|word| !word.text.trim().is_empty() && word.end >= word.start)
        .collect();
    heard.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Caption> = Vec::new();
    let mut current: Vec<&Word> = Vec::new();
    let close = |current: &mut Vec<&Word>, out: &mut Vec<Caption>| {
        if let (Some(first), Some(last)) = (current.first(), current.last()) {
            out.push(Caption {
                start: first.start,
                end: last.end,
                text: current
                    .iter()
                    .map(|word| word.text.trim())
                    .collect::<Vec<_>>()
                    .join(" "),
            });
        }
        current.clear();
    };
    for word in heard {
        if let Some(last) = current.last() {
            let chars: usize = current
                .iter()
                .map(|word| word.text.trim().chars().count() + 1)
                .sum::<usize>()
                + word.text.trim().chars().count();
            let ends_sentence = last.text.trim().ends_with(['.', '!', '?', '…']);
            if current.len() >= rules.max_words
                || chars > rules.max_chars
                || word.start - last.end >= rules.break_on_pause
                || ends_sentence
            {
                close(&mut current, &mut out);
            }
        }
        current.push(word);
    }
    close(&mut current, &mut out);
    // Long enough to read, and no flicker off and on between two.
    for index in 0..out.len() {
        let next = out.get(index + 1).map(|caption| caption.start);
        let caption = &mut out[index];
        let room = next.unwrap_or(f64::INFINITY);
        if caption.end - caption.start < rules.min_duration {
            caption.end = (caption.start + rules.min_duration)
                .min(room)
                .max(caption.end);
        }
        if let Some(next) = next
            && next > caption.end
            && next - caption.end < rules.max_fill
        {
            caption.end = next;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start: f64, end: f64, text: &str) -> Word {
        Word {
            start,
            end,
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_long_pause_is_cut_with_air_either_side_and_a_short_one_stays() {
        let words = [
            word(1.0, 1.4, "hola"),
            word(1.6, 2.0, "qué"),
            word(4.0, 4.5, "tal"),
        ];
        let rules = SilenceRules::default();
        let keep = kept(&words, 6.0, &rules);
        assert_eq!(keep.len(), 2, "the 0.2 s pause stays, the 2 s one goes");
        assert!((keep[0].0 - 0.9).abs() < 1e-9 && (keep[0].1 - 2.2).abs() < 1e-9);
        assert!((keep[1].0 - 3.9).abs() < 1e-9 && (keep[1].1 - 4.7).abs() < 1e-9);
        let cut = removed(&words, 6.0, &rules);
        assert_eq!(cut.len(), 3, "the head, the pause and the tail");
        assert!((cut[0].1 - 0.9).abs() < 1e-9);
        assert!((cut[2].0 - 4.7).abs() < 1e-9 && (cut[2].1 - 6.0).abs() < 1e-9);
    }

    #[test]
    fn a_blip_shorter_than_min_keep_is_dropped() {
        let words = [word(1.0, 1.02, "eh"), word(3.0, 4.0, "bueno")];
        let rules = SilenceRules {
            pad_in: 0.0,
            pad_out: 0.0,
            ..SilenceRules::default()
        };
        assert_eq!(kept(&words, 5.0, &rules), vec![(3.0, 4.0)]);
    }

    #[test]
    fn captions_are_a_few_words_broken_at_pauses_and_sentences() {
        let words = [
            word(0.0, 0.3, "Así"),
            word(0.3, 0.7, "editamos"),
            word(0.7, 0.9, "un"),
            word(0.9, 1.3, "reel."),
            word(1.4, 1.7, "Muy"),
            word(2.5, 2.9, "rápido"),
        ];
        let made = captions(&words, &CaptionRules::default());
        let texts: Vec<&str> = made.iter().map(|caption| caption.text.as_str()).collect();
        assert_eq!(texts, ["Así editamos un", "reel.", "Muy", "rápido"]);
        // "reel." holds across the 0.1 s gap to "Muy"; "Muy" does not hold
        // across the 0.8 s one.
        assert_eq!(made[1].end, 1.4);
        assert_eq!(made[2].end, 1.7);
    }

    #[test]
    fn a_caption_is_never_a_flicker_where_there_is_room() {
        let words = [word(0.0, 0.05, "y"), word(2.0, 2.4, "luego")];
        let made = captions(&words, &CaptionRules::default());
        assert!((made[0].end - 0.16).abs() < 1e-9);
    }

    #[test]
    fn long_words_break_on_characters() {
        let words = [
            word(0.0, 0.5, "extraordinariamente"),
            word(0.5, 1.0, "bonito"),
        ];
        let made = captions(&words, &CaptionRules::default());
        assert_eq!(made.len(), 2, "26 characters is past 22");
    }
}
