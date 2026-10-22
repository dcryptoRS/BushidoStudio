// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Generate with AI (Royalty Studio fork): a clip or a picture made from a
//! prompt by a service - Higgsfield (Seedance 2.5, Soul and the rest),
//! Seedance from ByteDance through BytePlus, Gemini - and a clip from the
//! frame under the playhead as well. The keys and the models are on the
//! Settings page Assistant and AI; the calls are `concat_host::genai`'s.
//!
//! The request runs on a worker and can take minutes. What comes back is
//! written into the project's `generated` folder, shelved in the bin under
//! Generated › With AI, and put on the timeline at the playhead the sheet
//! was opened at. A request, once sent, is paid for; Cancel stops waiting
//! for it, not the service.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_host::genai::{self, ImageRequest, Keys, Provider, VideoRequest};
use concat_host::media::MediaSummary;
use concat_project::Command;
use concat_project::model::MediaOrigin;
use slint::{ModelRc, SharedString, VecModel};

use crate::host::spawn_in_project;
use crate::i18n::{t, tf};
use crate::panes::Msg;
use crate::studio::Studio;
use crate::ui::GenerateSheetData;

/// The shapes a clip or a picture can be asked in, as every service names
/// them.
pub const ASPECTS: [&str; 6] = ["16:9", "9:16", "1:1", "4:3", "3:4", "21:9"];
/// A clip's resolutions.
pub const RESOLUTIONS: [&str; 3] = ["480p", "720p", "1080p"];
/// A picture's sizes; Gemini's.
pub const SIZES: [&str; 3] = ["1K", "2K", "4K"];
/// How long a clip may be asked to run, in seconds.
pub const DURATIONS: (u32, u32) = (4, 30);

/// The services that make what the sheet is set to make, in the order its
/// list shows them.
fn services(picture: bool) -> [Provider; 2] {
    if picture {
        [Provider::Gemini, Provider::Higgsfield]
    } else {
        [Provider::Higgsfield, Provider::Seedance]
    }
}

/// Everything that can happen to the sheet.
#[derive(Clone, Debug)]
pub enum GenerateMsg {
    /// The tray's button, the bin's: open it, or put it away.
    Toggle,
    Close,
    PromptEdited(String),
    /// One of the sheet's choices, by its name, as a number (a switch is 0
    /// or 1): "kind", "service", "frame", "duration", "resolution",
    /// "aspect", "audio", "size".
    Changed(String, i32),
    /// Settings, at the page the keys are on.
    OpenSettings,
    Begin,
    Cancel,
    /// The worker is done, for run `.0`: the file, probed, or why not.
    Finished(u64, Box<Result<MediaSummary, String>>),
}

/// The sheet's state.
pub struct GeneratePane {
    pub open: bool,
    /// A picture rather than a clip.
    picture: bool,
    /// A row of [`services`].
    service: usize,
    prompt: String,
    /// The clip starts from the frame under the playhead.
    from_frame: bool,
    duration: u32,
    resolution: usize,
    /// A row of [`ASPECTS`]; `None` until the sheet first opens on a
    /// project, when it takes the project's own shape.
    aspect: Option<usize>,
    audio: bool,
    size: usize,
    running: bool,
    /// Bumped on every run and every cancel, so a late answer from a run
    /// nobody waits for any more is dropped.
    run: u64,
    cancel: Arc<AtomicBool>,
    /// Where the result lands: the playhead when the run began.
    landing: f64,
    message: String,
}

impl Default for GeneratePane {
    fn default() -> Self {
        GeneratePane {
            open: false,
            picture: false,
            service: 0,
            prompt: String::new(),
            from_frame: false,
            duration: 5,
            resolution: 1,
            aspect: None,
            audio: true,
            size: 1,
            running: false,
            run: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            landing: 0.0,
            message: String::new(),
        }
    }
}

impl GeneratePane {
    pub fn update(&mut self, msg: GenerateMsg, studio: &mut Studio) {
        match msg {
            GenerateMsg::Toggle => {
                if self.open {
                    self.open = false;
                } else if studio.session.is_some() {
                    if self.aspect.is_none() {
                        self.aspect = Some(nearest_aspect(studio.output_size()));
                    }
                    self.open = true;
                }
            }
            GenerateMsg::Close => self.open = false,
            GenerateMsg::PromptEdited(text) => {
                self.prompt = text;
                self.message.clear();
            }
            GenerateMsg::Changed(field, value) => {
                let row = |len: usize| (value.max(0) as usize).min(len - 1);
                match field.as_str() {
                    "kind" => {
                        self.picture = value == 1;
                        self.service = 0;
                    }
                    "service" => self.service = row(2),
                    "frame" => self.from_frame = value != 0,
                    "duration" => {
                        self.duration = (value.max(0) as u32).clamp(DURATIONS.0, DURATIONS.1);
                    }
                    "resolution" => self.resolution = row(RESOLUTIONS.len()),
                    "aspect" => self.aspect = Some(row(ASPECTS.len())),
                    "audio" => self.audio = value != 0,
                    "size" => self.size = row(SIZES.len()),
                    _ => {}
                }
                self.message.clear();
            }
            GenerateMsg::OpenSettings => {
                studio.handle(Msg::Settings(crate::panes::settings::SettingsMsg::Open));
                studio.handle(Msg::Settings(
                    crate::panes::settings::SettingsMsg::PageChanged(
                        crate::panes::settings::ASSISTANT_PAGE,
                    ),
                ));
            }
            GenerateMsg::Begin => self.begin(studio),
            GenerateMsg::Cancel => {
                self.cancel.store(true, Ordering::Relaxed);
                self.run += 1;
                self.running = false;
            }
            GenerateMsg::Finished(run, _) if run != self.run => {}
            GenerateMsg::Finished(_, result) => {
                self.running = false;
                match *result {
                    Ok(summary) => self.land(studio, summary),
                    Err(error) if error == "cancelled" => {}
                    Err(error) => {
                        if !self.open {
                            studio.notify(&tf("generate.failed", &[&error]), true);
                        }
                        self.message = error;
                    }
                }
            }
        }
    }

    /// The service the sheet is set to.
    fn provider(&self) -> Provider {
        services(self.picture)[self.service.min(1)]
    }

    /// Sends the request on a worker.
    fn begin(&mut self, studio: &mut Studio) {
        let prompt = self.prompt.trim().to_owned();
        if prompt.is_empty() {
            self.message = t("generate.describeFirst");
            return;
        }
        let keys = Keys::load(&studio.host.dirs.config);
        let provider = self.provider();
        if !keys.has(provider) {
            self.message = t("generate.noKey");
            return;
        }
        let Some(project) = studio
            .session
            .as_ref()
            .map(|session| PathBuf::from(session.path()))
        else {
            return;
        };
        let folder = project.join("generated");
        let models = studio.prefs.assistant.models.filled();
        let aspect = ASPECTS[self.aspect.unwrap_or(0).min(ASPECTS.len() - 1)].to_owned();
        if studio.playing {
            studio.pause();
        }
        self.landing = f64::from(studio.playhead);
        // The frame under the playhead, drawn on the worker: everything it
        // needs is taken here, on the window's thread.
        let frame = if !self.picture && self.from_frame {
            let Some(grab) = frame_grab(studio, folder.clone()) else {
                self.message = t("generate.noFrame");
                return;
            };
            Some(grab)
        } else {
            None
        };
        let picture = self.picture;
        let video = VideoRequest {
            prompt: prompt.clone(),
            frame: None,
            duration: self.duration,
            resolution: RESOLUTIONS[self.resolution].to_owned(),
            aspect: aspect.clone(),
            audio: self.audio,
        };
        let image = ImageRequest {
            prompt,
            aspect,
            size: SIZES[self.size].to_owned(),
        };
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.cancel);
        self.run += 1;
        let run = self.run;
        self.running = true;
        self.message.clear();
        log::info!(
            "generate: {provider:?}, {}",
            if picture { "picture" } else { "clip" }
        );
        spawn_in_project(
            move || -> Result<MediaSummary, String> {
                let path = if picture {
                    genai::generate_image(provider, &keys, &models, &image, &folder, &cancel)?
                } else {
                    let mut video = video;
                    if let Some(grab) = frame {
                        video.frame = Some(grab()?);
                    }
                    genai::generate_video(provider, &keys, &models, &video, &folder, &cancel)?
                };
                concat_host::media::probe(&path.to_string_lossy())
            },
            move |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Finished(run, Box::new(result))));
            },
        );
    }

    /// The made file into the bin, shelved as generated, and onto the
    /// timeline where the playhead was.
    fn land(&mut self, studio: &mut Studio, summary: MediaSummary) {
        let mut item = summary.to_new_media();
        item.origin = Some(MediaOrigin::Generated);
        let created = studio.apply(Command::AddMedia { item });
        let media_id = created.or_else(|| {
            studio
                .project()
                .media
                .iter()
                .find(|item| item.path == summary.path)
                .map(|item| item.id.clone())
        });
        if let Some(media_id) = media_id {
            studio.apply(Command::AddClipAtFirstFree {
                media_id,
                start: self.landing,
            });
        }
        studio.notify(&t("generate.landed"), false);
    }

    /// The sheet as it is drawn.
    pub fn data(&self, studio: &Studio) -> GenerateSheetData {
        let models = studio.prefs.assistant.models.filled();
        let keys = if self.open {
            Keys::load(&studio.host.dirs.config)
        } else {
            Keys::default()
        };
        let names: Vec<SharedString> = services(self.picture)
            .into_iter()
            .map(|provider| service_name(provider, self.picture, &models).into())
            .collect();
        GenerateSheetData {
            open: self.open,
            picture: self.picture,
            service: self.service as i32,
            services: ModelRc::from(std::rc::Rc::new(VecModel::from(names))),
            prompt: self.prompt.as_str().into(),
            from_frame: self.from_frame,
            frame_offered: !self.picture,
            duration: self.duration as i32,
            resolution: self.resolution as i32,
            aspect: self.aspect.unwrap_or(0) as i32,
            audio: self.audio,
            size: self.size as i32,
            size_offered: self.picture && self.provider() == Provider::Gemini,
            keyed: keys.has(self.provider()),
            running: self.running,
            message: self.message.as_str().into(),
        }
    }
}

/// What the list calls a service: who it is and what it makes there.
fn service_name(provider: Provider, picture: bool, models: &genai::Models) -> String {
    match provider {
        Provider::Higgsfield if picture => format!("Higgsfield · {}", models.higgsfield_image),
        Provider::Higgsfield => format!("Higgsfield · {}", models.higgsfield_video),
        Provider::Seedance => format!("BytePlus · {}", models.seedance),
        Provider::Gemini => format!("Gemini · {}", models.gemini_image),
    }
}

/// The row of [`ASPECTS`] closest to a `width` × `height` frame.
pub fn nearest_aspect((width, height): (u32, u32)) -> usize {
    let ratio = f64::from(width.max(1)) / f64::from(height.max(1));
    ASPECTS
        .iter()
        .map(|aspect| {
            let (w, h) = aspect.split_once(':').unwrap_or(("16", "9"));
            let wanted = w.parse::<f64>().unwrap_or(16.0) / h.parse::<f64>().unwrap_or(9.0);
            (ratio.ln() - wanted.ln()).abs()
        })
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(0, |(row, _)| row)
}

/// A job that draws the frame under the playhead at the output's size, no
/// larger than 1920 on its long side, and writes it as a JPEG into
/// `folder`; taken on the window's thread, run on a worker. None with
/// nothing on the timeline to draw.
#[allow(clippy::type_complexity)]
fn frame_grab(
    studio: &mut Studio,
    folder: PathBuf,
) -> Option<Box<dyn FnOnce() -> Result<PathBuf, String> + Send>> {
    let (clips, settings) = studio.preview_clips()?;
    let (width, height) = studio.output_size();
    let scale = (1920.0 / f64::from(width.max(height).max(1))).min(1.0);
    let even = |side: u32| ((f64::from(side) * scale) as u32 / 2 * 2).max(2);
    let (width, height) = (even(width), even(height));
    let spec = concat_host::preview::FrameSpec {
        time: f64::from(studio.playhead),
        width,
        height,
        moving: false,
        proxy: false,
        color_space: studio.project().active().video.color_space,
    };
    let monitor = studio.host.monitor.clone();
    Some(Box::new(move || {
        let pixels = monitor.frame(clips, &settings, spec)?;
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for pixel in pixels.chunks_exact(4) {
            rgb.extend_from_slice(&pixel[..3]);
        }
        std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis())
            .unwrap_or_default();
        let path = folder.join(format!("frame-{stamp}.jpg"));
        let file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
        let mut writer = std::io::BufWriter::new(file);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 92)
            .encode(&rgb, width, height, image::ColorType::Rgb8.into())
            .map_err(|error| error.to_string())?;
        Ok(path)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_shape_picks_the_nearest_aspect() {
        assert_eq!(ASPECTS[nearest_aspect((1920, 1080))], "16:9");
        assert_eq!(ASPECTS[nearest_aspect((1080, 1920))], "9:16");
        assert_eq!(ASPECTS[nearest_aspect((1080, 1080))], "1:1");
        assert_eq!(ASPECTS[nearest_aspect((1080, 1350))], "3:4");
        assert_eq!(ASPECTS[nearest_aspect((2560, 1080))], "21:9");
    }

    #[test]
    fn a_clip_is_made_by_higgsfield_or_seedance_and_a_picture_by_gemini_first() {
        assert_eq!(services(false), [Provider::Higgsfield, Provider::Seedance]);
        assert_eq!(services(true)[0], Provider::Gemini);
    }
}
