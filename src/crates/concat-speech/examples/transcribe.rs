// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Transcribes a file with an installed model and says how long it took
//! (Royalty Studio fork): a check that whisper runs where it should - on
//! the GPU in a `cuda` build. `transcribe FILE [MODEL] [words]`.

use concat_speech::transcribe::{TranscribeRequest, Transcriber};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a file to transcribe");
    let model = args.next().unwrap_or_else(|| "base".to_owned());
    let words = args.next().is_some_and(|arg| arg == "words");
    let dirs = concat_host::AppDirs::locate().expect("the app's folders");
    let duration = concat_host::media::probe(&path)
        .ok()
        .and_then(|summary| summary.duration)
        .unwrap_or(30.0);
    let request = TranscribeRequest {
        path,
        audio_stream: None,
        source_start: 0.0,
        window: duration,
        model_id: model,
        words,
    };
    let started = std::time::Instant::now();
    let segments = Transcriber::new()
        .transcribe(&dirs, &request, |_| {})
        .expect("transcribes");
    let took = started.elapsed().as_secs_f64();
    for segment in segments.iter().take(12) {
        println!(
            "{:7.2} {:7.2}  {}",
            segment.start, segment.end, segment.text
        );
    }
    println!(
        "{} segments from {duration:.1} s of sound in {took:.1} s ({:.1}x real time)",
        segments.len(),
        duration / took.max(1e-3)
    );
}
