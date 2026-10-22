// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! Generated media (Royalty Studio fork): a clip or a picture made by a
//! service from a prompt - a clip from a frame of the timeline as well -
//! and written into the project's media folder for the bin.
//!
//! - Higgsfield (api.higgsfield.ai): one endpoint per model, Seedance 2.5
//!   among them; a request is queued and its status asked after until the
//!   media is there. Keys are an id and a secret.
//! - Seedance 2.5 from ByteDance itself, through BytePlus ModelArk: a task
//!   created, then asked after.
//! - Gemini (Google AI Studio): a picture back in the answer itself.
//!
//! Model names change faster than releases, so every one is a setting with
//! the current one as its default. The calls are blocking and meant for a
//! worker thread; `cancel` is looked at between polls.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Who makes the media.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Higgsfield's API, which carries Seedance, Kling, Wan and others.
    Higgsfield,
    /// Seedance from ByteDance, through BytePlus ModelArk.
    Seedance,
    /// Google's Gemini image models.
    Gemini,
}

/// Where the keys are kept: the desktop's keyring, or - with none, or a
/// locked one - a file in the config folder only its owner may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// The Secret Service: GNOME Keyring, KWallet's bridge, KeePassXC.
    Keyring,
    /// `api-keys.json` in the config folder, mode 0600.
    File,
}

/// The keys as last read or kept, and where: read once a run, since the
/// Settings page and the Generate sheet ask on every publish.
static KEPT: Mutex<Option<(Keys, Kept)>> = Mutex::new(None);

/// What the keys' item in the keyring is found by.
#[cfg(target_os = "linux")]
fn keyring_attributes() -> std::collections::HashMap<&'static str, &'static str> {
    keyring_attributes_of(crate::dirs::IDENTIFIER)
}

/// The keys' item as a build under `identifier` kept it: Bushido Studio's
/// first run finds the ones kept before it was renamed.
#[cfg(target_os = "linux")]
fn keyring_attributes_of(
    identifier: &'static str,
) -> std::collections::HashMap<&'static str, &'static str> {
    std::collections::HashMap::from([
        ("application", identifier),
        ("kind", "generating-service-keys"),
    ])
}

/// The keys for each service, as the person typed them in Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Keys {
    /// Higgsfield's key id.
    pub higgsfield_id: String,
    /// Higgsfield's key secret.
    pub higgsfield_secret: String,
    /// The BytePlus ModelArk API key, for Seedance.
    pub seedance: String,
    /// The Gemini API key.
    pub gemini: String,
}

impl Keys {
    /// Whether `provider` has what it needs to be called.
    pub fn has(&self, provider: Provider) -> bool {
        match provider {
            Provider::Higgsfield => {
                !self.higgsfield_id.trim().is_empty() && !self.higgsfield_secret.trim().is_empty()
            }
            Provider::Seedance => !self.seedance.trim().is_empty(),
            Provider::Gemini => !self.gemini.trim().is_empty(),
        }
    }

    /// The keys file in the config folder: readable by its owner alone.
    pub fn path(config: &Path) -> PathBuf {
        config.join("api-keys.json")
    }

    /// The keys kept, or none.
    pub fn load(config: &Path) -> Keys {
        Keys::loaded(config).0
    }

    /// The keys kept, and where they are: from memory after the first read
    /// of a run.
    pub fn loaded(config: &Path) -> (Keys, Kept) {
        if let Some(kept) = KEPT.lock().ok().and_then(|kept| kept.clone()) {
            return kept;
        }
        let kept = Keys::read(config);
        if let Ok(mut cache) = KEPT.lock() {
            *cache = Some(kept.clone());
        }
        kept
    }

    /// The keyring's keys; the file's when the keyring has none yet (they
    /// move in at the next save), or when there is no keyring to ask.
    fn read(config: &Path) -> (Keys, Kept) {
        #[cfg(target_os = "linux")]
        match crate::secrets::load(&keyring_attributes()) {
            Ok(Some(bytes)) => {
                if let Ok(keys) = serde_json::from_slice(&bytes) {
                    return (keys, Kept::Keyring);
                }
            }
            Ok(None) => {
                // Kept before the rename, under Concat's identifier: read,
                // and kept under this one at the next save.
                let legacy =
                    crate::secrets::load(&keyring_attributes_of(crate::dirs::LEGACY_IDENTIFIER));
                if let Ok(Some(bytes)) = legacy
                    && let Ok(keys) = serde_json::from_slice(&bytes)
                {
                    return (keys, Kept::Keyring);
                }
                return (Keys::load_file(config), Kept::Keyring);
            }
            Err(error) => log::info!("keys: no keyring to keep them in ({error}), a file instead"),
        }
        (Keys::load_file(config), Kept::File)
    }

    /// Keeps the keys: in the keyring, or in the file when there is none;
    /// says which.
    pub fn save(&self, config: &Path) -> Result<Kept, String> {
        #[cfg(target_os = "linux")]
        {
            let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
            match crate::secrets::store(
                "Concat (Royalty Studio): generating services' keys",
                &keyring_attributes(),
                &bytes,
            ) {
                Ok(()) => {
                    // In the keyring now: a file left from before goes.
                    let _ = std::fs::remove_file(Keys::path(config));
                    self.remember(Kept::Keyring);
                    return Ok(Kept::Keyring);
                }
                Err(error) => {
                    log::warn!("keys: the keyring would not keep them ({error}), a file instead")
                }
            }
        }
        self.save_file(config)?;
        self.remember(Kept::File);
        Ok(Kept::File)
    }

    fn remember(&self, kept: Kept) {
        if let Ok(mut cache) = KEPT.lock() {
            *cache = Some((self.clone(), kept));
        }
    }

    /// The keys in the config folder's file, or none.
    pub fn load_file(config: &Path) -> Keys {
        std::fs::read(Keys::path(config))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Keeps the keys in a file only its owner may read.
    pub fn save_file(&self, config: &Path) -> Result<(), String> {
        let path = Keys::path(config);
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(config).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .map_err(|error| format!("could not keep the keys: {error}"))?;
            file.write_all(&bytes)
                .map_err(|error| format!("could not keep the keys: {error}"))?;
            // A file that was there before keeps its old mode otherwise.
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            Ok(())
        }
        #[cfg(not(unix))]
        std::fs::write(&path, bytes).map_err(|error| format!("could not keep the keys: {error}"))
    }
}

/// The model each service is asked for, with today's as the default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Models {
    /// Higgsfield's path for a video model: `/text-to-video` is added to it
    /// for a clip from a prompt, `/image-to-video` for one from a frame.
    pub higgsfield_video: String,
    /// Higgsfield's path for a picture.
    pub higgsfield_image: String,
    /// The ModelArk model id for Seedance.
    pub seedance: String,
    /// Where ModelArk is reached.
    pub seedance_base: String,
    /// The Gemini image model.
    pub gemini_image: String,
}

impl Default for Models {
    fn default() -> Self {
        Models {
            higgsfield_video: "bytedance/seedance-2.5".to_owned(),
            higgsfield_image: "higgsfield-ai/soul/v2/standard".to_owned(),
            seedance: "dreamina-seedance-2-5-260628".to_owned(),
            seedance_base: "https://ark.ap-southeast.bytepluses.com/api/v3".to_owned(),
            gemini_image: "gemini-nano-banana-2.1".to_owned(),
        }
    }
}

impl Models {
    /// The models Settings › Assistant and AI chose, as the window keeps
    /// them in its settings file under `assistant.models`, blanks filled:
    /// for a caller that is not the window, the MCP server's tool.
    pub fn from_settings(config: &Path) -> Models {
        std::fs::read(config.join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| {
                serde_json::from_value::<Models>(value.pointer("/assistant/models")?.clone()).ok()
            })
            .unwrap_or_default()
            .filled()
    }

    /// These, with a field left blank in Settings read as today's default.
    pub fn filled(&self) -> Models {
        let defaults = Models::default();
        let pick = |own: &String, default: String| {
            if own.trim().is_empty() {
                default
            } else {
                own.trim().to_owned()
            }
        };
        Models {
            higgsfield_video: pick(&self.higgsfield_video, defaults.higgsfield_video),
            higgsfield_image: pick(&self.higgsfield_image, defaults.higgsfield_image),
            seedance: pick(&self.seedance, defaults.seedance),
            seedance_base: pick(&self.seedance_base, defaults.seedance_base),
            gemini_image: pick(&self.gemini_image, defaults.gemini_image),
        }
    }
}

/// A clip asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoRequest {
    /// The shot, described.
    pub prompt: String,
    /// A frame to start from, a PNG or a JPEG on disk.
    pub frame: Option<PathBuf>,
    /// Seconds, 4 to 30.
    pub duration: u32,
    /// "480p", "720p" or "1080p".
    pub resolution: String,
    /// "16:9", "9:16", "1:1", "4:3", "3:4" or "21:9".
    pub aspect: String,
    /// With sound made for it.
    pub audio: bool,
}

/// A picture asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRequest {
    /// The picture, described.
    pub prompt: String,
    /// As for a clip.
    pub aspect: String,
    /// "1K", "2K" or "4K"; Gemini's sizes.
    pub size: String,
}

/// How long a request may take before it is given up on.
const PATIENCE: Duration = Duration::from_secs(20 * 60);
/// How long between two looks at a queued request.
const POLL: Duration = Duration::from_secs(4);

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .build()
}

/// What a service said back, read whether the call worked or not: a failed
/// call's body names what was wrong, and that is what is worth saying.
fn answer(result: Result<ureq::Response, ureq::Error>, service: &str) -> Result<Value, String> {
    match result {
        // Read whole, up to a size: `into_string` stops at 10 MB, and a
        // picture comes back inside Gemini's answer.
        Ok(response) => {
            use std::io::Read;
            let mut body = Vec::new();
            response
                .into_reader()
                .take(512 * 1024 * 1024)
                .read_to_end(&mut body)
                .map_err(|error| error.to_string())
                .and_then(|_| {
                    serde_json::from_slice::<Value>(&body).map_err(|error| error.to_string())
                })
                .map_err(|error| format!("{service} answered something unreadable: {error}"))
        }
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            let said = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value["error"]["message"]
                        .as_str()
                        .or_else(|| value["error"].as_str())
                        .or_else(|| value["detail"].as_str())
                        .or_else(|| value["message"].as_str())
                        .map(str::to_owned)
                })
                .unwrap_or(body);
            Err(match code {
                401 | 403 => format!("{service} refused the key ({code}): {said}"),
                402 => format!("{service} says the account has no credit left: {said}"),
                429 => format!("{service} is asked too often right now: {said}"),
                _ => format!("{service} refused the request ({code}): {said}"),
            })
        }
        Err(error) => Err(format!("{service} could not be reached: {error}")),
    }
}

/// A picture on disk as a data URI, which every one of the services takes
/// in place of a link.
fn data_uri(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let kind = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_lowercase)
    {
        Some(ext) if ext == "png" => "image/png",
        Some(ext) if ext == "webp" => "image/webp",
        _ => "image/jpeg",
    };
    Ok(format!("data:{kind};base64,{}", encode(&bytes)))
}

/// Makes a clip, and writes it into `folder`; the file's path.
pub fn generate_video(
    provider: Provider,
    keys: &Keys,
    models: &Models,
    request: &VideoRequest,
    folder: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    if !keys.has(provider) {
        return Err("no key for this service: add one in Settings".to_owned());
    }
    let url = match provider {
        Provider::Higgsfield => higgsfield_video(keys, models, request, cancel)?,
        Provider::Seedance => seedance_video(keys, models, request, cancel)?,
        Provider::Gemini => return Err("Gemini makes pictures here, not clips".to_owned()),
    };
    download(&url, folder, "mp4", cancel)
}

/// Makes a picture, and writes it into `folder`; the file's path.
pub fn generate_image(
    provider: Provider,
    keys: &Keys,
    models: &Models,
    request: &ImageRequest,
    folder: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    if !keys.has(provider) {
        return Err("no key for this service: add one in Settings".to_owned());
    }
    match provider {
        Provider::Gemini => {
            let bytes = gemini_image(keys, models, request)?;
            write_new(folder, "png", &bytes)
        }
        Provider::Higgsfield => {
            let url = higgsfield_image(keys, models, request, cancel)?;
            download(&url, folder, "png", cancel)
        }
        Provider::Seedance => Err("Seedance makes clips here, not pictures".to_owned()),
    }
}

fn higgsfield_auth(keys: &Keys) -> String {
    format!(
        "Key {}:{}",
        keys.higgsfield_id.trim(),
        keys.higgsfield_secret.trim()
    )
}

/// The body of a Higgsfield clip request: from a frame, at `image_url`
/// (an `image-to-video` model), or from the prompt alone.
pub fn higgsfield_video_body(request: &VideoRequest, frame: Option<String>) -> Value {
    let mut body = json!({
        "prompt": request.prompt,
        "duration": request.duration.clamp(4, 30),
        "resolution": request.resolution,
        "aspect_ratio": request.aspect,
        "generate_audio": request.audio,
    });
    if let Some(frame) = frame {
        body["image_url"] = json!(frame);
    }
    body
}

/// A model path with its workflow at the end: the one configured when it
/// already names one, else `workflow` added to it.
fn higgsfield_path(model: &str, workflow: &str) -> String {
    let model = model.trim().trim_matches('/');
    if model.ends_with("-to-video") {
        model.to_owned()
    } else {
        format!("{model}/{workflow}")
    }
}

fn higgsfield_video(
    keys: &Keys,
    models: &Models,
    request: &VideoRequest,
    cancel: &AtomicBool,
) -> Result<String, String> {
    // A frame goes up to Higgsfield's storage first: its models read input
    // from a public link, never from the request itself.
    let frame = request
        .frame
        .as_deref()
        .map(|frame| higgsfield_upload(keys, frame))
        .transpose()?;
    let workflow = if frame.is_some() {
        "image-to-video"
    } else {
        "text-to-video"
    };
    let path = higgsfield_path(&models.higgsfield_video, workflow);
    let body = higgsfield_video_body(request, frame);
    let queued = higgsfield_submit(keys, &path, &body)?;
    let done = higgsfield_wait(keys, &queued, cancel)?;
    done["video"]["url"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "Higgsfield finished with no clip".to_owned())
}

fn higgsfield_image(
    keys: &Keys,
    models: &Models,
    request: &ImageRequest,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let body = json!({
        "prompt": request.prompt,
        "aspect_ratio": request.aspect,
    });
    let queued = higgsfield_submit(keys, &models.higgsfield_image, &body)?;
    let done = higgsfield_wait(keys, &queued, cancel)?;
    done["images"][0]["url"]
        .as_str()
        .or_else(|| done["image"]["url"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "Higgsfield finished with no picture".to_owned())
}

/// Puts a picture in Higgsfield's storage, through the one-hour link it
/// hands out for it, and says where it can be read from.
fn higgsfield_upload(keys: &Keys, path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let content_type = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_lowercase)
    {
        Some(ext) if ext == "png" => "image/png",
        Some(ext) if ext == "webp" => "image/webp",
        _ => "image/jpeg",
    };
    let slot = answer(
        agent()
            .post("https://api.higgsfield.ai/files/generate-upload-url")
            .set("Authorization", &higgsfield_auth(keys))
            .set("Content-Type", "application/json")
            .send_string(&json!({ "content_type": content_type }).to_string()),
        "Higgsfield",
    )?;
    let (Some(upload), Some(public)) = (slot["upload_url"].as_str(), slot["public_url"].as_str())
    else {
        return Err("Higgsfield gave nowhere to upload the frame".to_owned());
    };
    // Every header it named, and never the API key: the link is storage's.
    let mut put = agent().put(upload);
    if let Some(headers) = slot["upload_headers"].as_object() {
        for (name, value) in headers {
            if let Some(value) = value.as_str() {
                put = put.set(name, value);
            }
        }
    } else {
        put = put.set("Content-Type", content_type);
    }
    put.send_bytes(&bytes)
        .map_err(|error| format!("the frame could not be uploaded: {error}"))?;
    Ok(public.to_owned())
}

fn higgsfield_submit(keys: &Keys, path: &str, body: &Value) -> Result<Value, String> {
    let url = format!("https://api.higgsfield.ai/{}", path.trim_start_matches('/'));
    answer(
        agent()
            .post(&url)
            .set("Authorization", &higgsfield_auth(keys))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string()),
        "Higgsfield",
    )
}

/// Asks after a queued Higgsfield request until it is done, one way or the
/// other.
fn higgsfield_wait(keys: &Keys, queued: &Value, cancel: &AtomicBool) -> Result<Value, String> {
    let id = queued["request_id"]
        .as_str()
        .ok_or_else(|| "Higgsfield queued nothing".to_owned())?;
    let status_url = queued["status_url"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("https://api.higgsfield.ai/requests/{id}/status"));
    let started = Instant::now();
    let mut current = queued.clone();
    loop {
        match current["status"].as_str().unwrap_or_default() {
            "completed" => return Ok(current),
            "failed" => {
                return Err(format!(
                    "Higgsfield could not make it: {}",
                    current["error"].as_str().unwrap_or("no reason given")
                ));
            }
            "nsfw" => return Err("Higgsfield refused the content".to_owned()),
            "canceled" => return Err("cancelled".to_owned()),
            _ => {}
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = agent()
                .post(&format!("https://api.higgsfield.ai/requests/{id}/cancel"))
                .set("Authorization", &higgsfield_auth(keys))
                .call();
            return Err("cancelled".to_owned());
        }
        if started.elapsed() > PATIENCE {
            return Err("Higgsfield took too long".to_owned());
        }
        sleep_unless(cancel, POLL);
        current = answer(
            agent()
                .get(&status_url)
                .set("Authorization", &higgsfield_auth(keys))
                .call(),
            "Higgsfield",
        )?;
    }
}

/// The body of a ModelArk Seedance task.
pub fn seedance_body(models: &Models, request: &VideoRequest, frame: Option<String>) -> Value {
    let mut content = vec![json!({ "type": "text", "text": request.prompt })];
    if let Some(frame) = frame {
        content.push(json!({
            "type": "image_url",
            "image_url": { "url": frame },
            "role": "first_frame",
        }));
    }
    json!({
        "model": models.seedance,
        "content": content,
        "resolution": request.resolution,
        "ratio": request.aspect,
        "duration": request.duration.clamp(4, 30),
        "generate_audio": request.audio,
        "watermark": false,
    })
}

fn seedance_video(
    keys: &Keys,
    models: &Models,
    request: &VideoRequest,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let frame = request.frame.as_deref().map(data_uri).transpose()?;
    let base = models.seedance_base.trim_end_matches('/');
    let bearer = format!("Bearer {}", keys.seedance.trim());
    let created = answer(
        agent()
            .post(&format!("{base}/contents/generations/tasks"))
            .set("Authorization", &bearer)
            .set("Content-Type", "application/json")
            .send_string(&seedance_body(models, request, frame).to_string()),
        "Seedance",
    )?;
    let id = created["id"]
        .as_str()
        .ok_or_else(|| "Seedance created no task".to_owned())?
        .to_owned();
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = agent()
                .delete(&format!("{base}/contents/generations/tasks/{id}"))
                .set("Authorization", &bearer)
                .call();
            return Err("cancelled".to_owned());
        }
        if started.elapsed() > PATIENCE {
            return Err("Seedance took too long".to_owned());
        }
        sleep_unless(cancel, POLL);
        let task = answer(
            agent()
                .get(&format!("{base}/contents/generations/tasks/{id}"))
                .set("Authorization", &bearer)
                .call(),
            "Seedance",
        )?;
        match task["status"].as_str().unwrap_or_default() {
            "succeeded" => {
                return task["content"]["video_url"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "Seedance finished with no clip".to_owned());
            }
            "failed" | "expired" => {
                return Err(format!(
                    "Seedance could not make it: {}",
                    task["error"]["message"]
                        .as_str()
                        .unwrap_or("no reason given")
                ));
            }
            "cancelled" => return Err("cancelled".to_owned()),
            _ => {}
        }
    }
}

/// The body of a Gemini picture request.
pub fn gemini_body(request: &ImageRequest) -> Value {
    json!({
        "contents": [{ "parts": [{ "text": request.prompt }] }],
        "generationConfig": {
            "responseModalities": ["TEXT", "IMAGE"],
            "imageConfig": { "aspectRatio": request.aspect, "imageSize": request.size },
        },
    })
}

fn gemini_image(keys: &Keys, models: &Models, request: &ImageRequest) -> Result<Vec<u8>, String> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1/models/{}:generateContent",
        models.gemini_image.trim()
    );
    let answered = answer(
        agent()
            .post(&url)
            .set("x-goog-api-key", keys.gemini.trim())
            .set("Content-Type", "application/json")
            .send_string(&gemini_body(request).to_string()),
        "Gemini",
    )?;
    let parts = answered["candidates"][0]["content"]["parts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let data = parts
        .iter()
        .find_map(|part| part["inlineData"]["data"].as_str())
        .ok_or_else(|| {
            let said = parts
                .iter()
                .find_map(|part| part["text"].as_str())
                .unwrap_or("no picture in the answer");
            format!("Gemini made no picture: {said}")
        })?;
    decode(data).ok_or_else(|| "Gemini's picture could not be read".to_owned())
}

/// Waits `length`, a little at a time, so a cancel is felt within a second.
fn sleep_unless(cancel: &AtomicBool, length: Duration) {
    let until = Instant::now() + length;
    while Instant::now() < until && !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Fetches `url` into a new file in `folder`.
fn download(url: &str, folder: &Path, ext: &str, cancel: &AtomicBool) -> Result<PathBuf, String> {
    use std::io::Read;
    let response = agent()
        .get(url)
        .call()
        .map_err(|error| format!("the result could not be fetched: {error}"))?;
    let mut bytes = Vec::new();
    let mut reader = response.into_reader().take(4 * 1024 * 1024 * 1024);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_owned());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("the result could not be fetched: {error}"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    if bytes.is_empty() {
        return Err("the result came back empty".to_owned());
    }
    write_new(folder, ext, &bytes)
}

/// Writes `bytes` into `folder` under a fresh name.
fn write_new(folder: &Path, ext: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default();
    let path = folder.join(format!("generated-{stamp}.{ext}"));
    std::fs::write(&path, bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok(path)
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64, padded.
fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64, padded or not; None for anything else.
fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut bits = 0u32;
    let mut count = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_goes_there_and_back() {
        for text in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(decode(&encode(text)).as_deref(), Some(text));
        }
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode(b"fo"), "Zm8=");
    }

    #[test]
    fn requests_are_shaped_as_each_service_reads_them() {
        let request = VideoRequest {
            prompt: "a slow push-in on a barber's hands".to_owned(),
            frame: None,
            duration: 45,
            resolution: "1080p".to_owned(),
            aspect: "9:16".to_owned(),
            audio: false,
        };
        let body = higgsfield_video_body(&request, Some("data:image/png;base64,AA==".to_owned()));
        assert_eq!(body["duration"], 30, "held inside 4 to 30");
        assert_eq!(body["aspect_ratio"], "9:16");
        assert_eq!(body["image_url"], "data:image/png;base64,AA==");
        assert_eq!(
            higgsfield_path("bytedance/seedance-2.5", "image-to-video"),
            "bytedance/seedance-2.5/image-to-video"
        );
        assert_eq!(
            higgsfield_path("kling-video/v3.0/std/image-to-video", "text-to-video"),
            "kling-video/v3.0/std/image-to-video"
        );
        let models = Models::default();
        let task = seedance_body(&models, &request, None);
        assert_eq!(task["model"], "dreamina-seedance-2-5-260628");
        assert_eq!(task["content"][0]["type"], "text");
        assert_eq!(task["ratio"], "9:16");
        let picture = gemini_body(&ImageRequest {
            prompt: "a poster".to_owned(),
            aspect: "16:9".to_owned(),
            size: "2K".to_owned(),
        });
        assert_eq!(
            picture["generationConfig"]["imageConfig"]["imageSize"],
            "2K"
        );
    }

    #[test]
    fn keys_are_kept_for_their_owner_alone() {
        let folder = std::env::temp_dir().join(format!("concat-keys-{}", std::process::id()));
        let keys = Keys {
            gemini: "secret".to_owned(),
            ..Keys::default()
        };
        // The file alone: a test must not write into the keyring of the
        // machine it runs on.
        keys.save_file(&folder).unwrap();
        assert_eq!(Keys::load_file(&folder), keys);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(Keys::path(&folder))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(keys.has(Provider::Gemini) && !keys.has(Provider::Higgsfield));
        let _ = std::fs::remove_dir_all(&folder);
    }
}
