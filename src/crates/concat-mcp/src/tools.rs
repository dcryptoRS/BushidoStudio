// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The tools: which API method or edit command each one is, and what an
//! agent is told about it.
//!
//! Every tool is a [`concat_api::Request`] in the end. Most are one method
//! or one edit command as it is, their arguments that request's fields and
//! their schema cut from the API's ([`crate::schema`]); a handful compose,
//! as captions are a batch of titles and waiting on an export is asking its
//! status until it ends, and say so. None decides what an edit means.
//!
//! Names are stable: an agent's instructions and a user's allow-list name
//! them. A tool is added, never renamed; a removed one would be a version
//! bump of this server.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use concat_api::{ApiError, ErrorCode, Request};
use serde_json::{Map, Value, json};

use crate::backend::Backend;
use crate::schema::Schemas;
use crate::summary;

/// The longest preview side handed back when the agent names no size: a
/// frame an agent can look at without a 4K PNG in its context.
const PREVIEW_SIDE: f64 = 1280.0;
/// How often a wait asks how an export stands.
const POLL: Duration = Duration::from_millis(250);
/// How long `export_wait` waits when told nothing, and at most.
const WAIT_DEFAULT: f64 = 60.0;
const WAIT_LONGEST: f64 = 600.0;
/// The stages of an export in order, for one progress figure across them.
const STAGES: [&str; 5] = ["cutout", "stabilize", "video", "audio", "mux"];

/// What a tool does with its arguments.
#[derive(Clone, Copy, Debug)]
pub enum Action {
    /// An API method, its arguments the method's params.
    Method {
        /// The method's name, `area.verb`.
        method: &'static str,
        /// What the reply is shown as.
        reply: Reply,
    },
    /// One edit command on a project: `edit.apply` with the command's
    /// fields as arguments, beside the project's `path`.
    Command {
        /// The command's `op`.
        op: &'static str,
    },
    /// `version`, with how this server reaches Concat added.
    Version,
    /// `project.get`, summarised.
    TimelineRead,
    /// Titles at the given times, as one batch.
    Captions,
    /// `export.status` until the job ends or the time is up.
    ExportWait,
    /// `updateClip` with only a mask.
    Mask,
    /// `preview.frame`, as an image.
    PreviewFrame,
    /// A clip or a picture made by a generating service, imported
    /// (Royalty Studio fork).
    Generate,
    /// `preview.frame`, measured as a colourist reads scopes (Royalty
    /// Studio fork).
    Levels,
}

/// How a reply is shown to the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// As the API sent it.
    Raw,
    /// An editor view, as [`summary::view`].
    Summary,
}

/// One tool.
#[derive(Clone, Debug)]
pub struct Tool {
    /// The name an agent calls it by.
    pub name: &'static str,
    /// The name a person is shown.
    pub title: &'static str,
    /// What the agent is told: what it does, its units, its ranges.
    pub description: &'static str,
    /// Changes nothing.
    pub read_only: bool,
    /// May remove or overwrite something.
    pub destructive: bool,
    /// Calling it twice is calling it once.
    pub idempotent: bool,
    /// What it does.
    pub action: Action,
}

const fn method(
    name: &'static str,
    title: &'static str,
    description: &'static str,
    method: &'static str,
    reply: Reply,
) -> Tool {
    Tool {
        name,
        title,
        description,
        read_only: false,
        destructive: false,
        idempotent: false,
        action: Action::Method { method, reply },
    }
}

const fn command(
    name: &'static str,
    title: &'static str,
    description: &'static str,
    op: &'static str,
) -> Tool {
    Tool {
        name,
        title,
        description,
        read_only: false,
        destructive: false,
        idempotent: false,
        action: Action::Command { op },
    }
}

impl Tool {
    const fn reading(mut self) -> Tool {
        self.read_only = true;
        self.idempotent = true;
        self
    }

    const fn destroying(mut self) -> Tool {
        self.destructive = true;
        self
    }

    const fn idempotent(mut self) -> Tool {
        self.idempotent = true;
        self
    }
}

/// Every tool, in the order `tools/list` gives them.
pub fn catalogue() -> Vec<Tool> {
    use Reply::{Raw, Summary};
    vec![
        Tool {
            name: "concat_version",
            title: "Concat version",
            description: "What this Concat speaks: the API version, the build, its capabilities, and how this MCP server reaches it (`mcp.mode`: \"window\" when it goes through the open Concat window's server, \"headless\" when it runs the editor engine itself). Call it first if unsure what is available.",
            read_only: true,
            destructive: false,
            idempotent: true,
            action: Action::Version,
        },
        method(
            "project_list",
            "Recent projects",
            "The projects this machine opened most recently, newest first: each one's folder `path`, name, frame size and rate. A project is a folder; every other project tool names it by that `path`.",
            "project.list",
            Raw,
        )
        .reading(),
        method(
            "project_create",
            "Create a project",
            "Creates a project folder named `name` inside the existing directory `location`, and opens it. `video` sets the frame and rate (1920×1080 at 30 fps when absent). Returns the project summary; its folder is `location/name`, which is the `path` later tools take. Refuses a folder that already holds a project.",
            "project.create",
            Summary,
        ),
        method(
            "project_open",
            "Open a project",
            "Opens a project folder so it can be read and edited. Opening one already open returns it as it stands, unsaved edits included. Refused when the Concat window has that project open. Returns the project summary.",
            "project.open",
            Summary,
        )
        .idempotent(),
        method(
            "project_save",
            "Save a project",
            "Writes an open project's document to its folder; edits are only in memory until this is called. `name` renames the project as it saves.",
            "project.save",
            Raw,
        )
        .idempotent(),
        method(
            "project_close",
            "Close a project",
            "Closes an open project. With `save: true` it is saved first; otherwise unsaved edits are dropped.",
            "project.close",
            Raw,
        )
        .destroying(),
        Tool {
            name: "timeline_read",
            title: "Read the timeline",
            description: "An open project's active timeline as a compact summary: frame size and fps, the lanes (`tracks`, top to bottom, with their ids and - on a timeline with picture and sound lanes - their `kind`, \"video\" or \"audio\"), every clip in time order with its id, kind, track, start, duration and end in seconds and whatever is not at its default (speed, volume, opacity, transform, effects, keyframed properties, text), and the media bin with each item's id. Clips with the same `link` - a picture and its sound - are moved, trimmed, cut, retimed and deleted together; `edit_apply` with `{\"op\": \"unlinkClips\", \"clipIds\": [...]}` takes them apart and `linkClips` links clips. Read this to learn the ids other tools need.",
            read_only: true,
            destructive: false,
            idempotent: true,
            action: Action::TimelineRead,
        },
        method(
            "project_get",
            "Get the whole project",
            "An open project's full state: every timeline, clip field, media item and setting, as the editor holds it. Large; `timeline_read` is usually enough.",
            "project.get",
            Raw,
        )
        .reading(),
        method(
            "project_set_video",
            "Set frame size and rate",
            "Sets the active timeline's frame (width, height in pixels) and rate (rateNum/rateDen frames a second, e.g. 30000/1001 for 29.97) as one undoable edit.",
            "project.setVideo",
            Summary,
        ),
        method(
            "media_probe",
            "Probe a media file",
            "Reports what is inside a video, audio or image file - duration in seconds, streams, frame size and rate, codecs - without changing any project.",
            "media.probe",
            Raw,
        )
        .reading(),
        method(
            "media_import",
            "Import media",
            "Adds a file (an absolute path) to an open project's media bin, as dropping it on the window does. The new item's id is the summary's `createdId`; place it on the timeline with `clip_add`. Importing a path already in the bin changes nothing. For a motion graphic rendered by concat-studio, pass the `motion` its `overlay_render` returned: the item keeps how it was made, so it can be rendered again with other props from the window's Motion tab or with `media_replace_motion`.",
            "media.import",
            Summary,
        )
        .idempotent(),
        method(
            "media_replace_motion",
            "Replace a motion graphic's render",
            "Puts a new render of a motion graphic (a file from concat-studio's `overlay_render`, with the `motion` it returned) in place of the bin item `mediaId`, as one undoable edit. The item keeps its id, so its clips stay where they are: a clip that showed the whole of the old render shows the whole of the new one, longer or shorter, as far as the next clip on its lane; a cut from it keeps its cut. Use it to change a placed title's text, colours or length without placing it again.",
            "media.replaceMotion",
            Summary,
        ),
        method(
            "edit_apply",
            "Apply an edit",
            "Applies any one edit command to an open project, as one undoable step. `command` is an object tagged by `op` - every edit the window can make is one, with the same limits and refusals; the typed tools below are the common ones. `{\"op\": \"batch\", \"commands\": [...]}` applies several commands as one atomic, undoable step: all land or none does, and `createdId` is the last id minted inside. A refusal comes back as the sentence the window would show, with nothing changed. Returns the project summary; a command that creates something puts its id in `createdId`.",
            "edit.apply",
            Summary,
        ),
        method(
            "edit_undo",
            "Undo",
            "Steps an open project's history back one edit (a batch is one edit).",
            "edit.undo",
            Summary,
        ),
        method(
            "edit_redo",
            "Redo",
            "Steps an open project's history forward one edit.",
            "edit.redo",
            Summary,
        ),
        command(
            "clip_add",
            "Place a clip",
            "Places a clip of a media item (`mediaId`, from the bin) on a lane (`trackId`, from `timeline_read`) starting at `start` seconds. Its length is the media's (5 s for a still). On a timeline with picture and sound lanes a clip lands on a lane of its kind, and a video with sound lands as its picture on a video lane and its sound on an audio lane, linked. `ripple: true` pushes later clips on that lane right instead of overlapping them. The new clip's id is `createdId`.",
            "addClip",
        ),
        command(
            "clips_add_sequence",
            "Place clips end to end",
            "Lays several media items (`mediaIds`, from the bin, in order) end to end starting at `start` seconds, each its media's length (5 s for a still), in one undoable step. With no `trackId` they go on the lowest lane free for the whole run; on a named lane, `ripple: true` makes room as a drop does. `createdId` is the last clip's id; `timeline_read` lists the rest.",
            "addClipsInSequence",
        ),
        command(
            "clip_trim",
            "Trim a clip",
            "Moves one edge of a clip by `delta` seconds of timeline: `edge` \"start\" with a positive delta shortens it from the head (the picture does not slide), \"end\" with a positive delta lengthens it. A clip is never shorter than 1/60 s or longer than its media allows. `ripple: true` closes or opens the gap behind it on that lane.",
            "trimClip",
        ),
        command(
            "clips_split",
            "Split clips",
            "Cuts each named clip in two at `time` seconds on the timeline. The first part keeps the id; the second gets a new one. Clips the time does not fall inside are left alone.",
            "splitClips",
        ),
        command(
            "clips_move",
            "Move clips",
            "Moves clips in one undoable step: each to a new `start` (seconds, at least 0) and `trackId`. Unknown ids are skipped.",
            "moveClips",
        ),
        command(
            "clips_remove",
            "Delete clips",
            "Deletes clips from the active timeline. `ripple: true` closes the gaps they leave on their lanes; false leaves the gaps.",
            "removeClips",
        )
        .destroying(),
        command(
            "clip_update",
            "Change a clip",
            "Changes only the fields given in `patch`: volume (gain, 1 is unchanged, may exceed 1), fadeIn/fadeOut (seconds), opacity (0-1), muted, flipH/flipV, blend, crop (fractions off each edge, null to remove), videoEffects and filters (the whole chain: a list of {id, params}; ids from `effects_list`), transitionIn ({id, duration} on the cut into the clip, null to remove), text (a title's style, null to remove), mask (see `clip_mask`), background (the clip's own background, null to follow the timeline's) and audioStream.",
            "updateClip",
        ),
        Tool {
            name: "clip_mask",
            title: "Mask a clip",
            description: "Keeps part of a clip's picture with a shape, or with `mask: null` takes the mask off. `shape` is linear, mirror, circle, rectangle, heart or star; `x`/`y` move its centre as fractions of the picture's width/height (positive right/down); `width`/`height` are fractions of the picture's shorter side; `rotation` in degrees; `roundness` (rectangle corners) and `feather` (edge softness) 0-1; `invert` keeps the outside. Fields left out take the default circle's. Animate it with `keyframe_set` on maskX, maskY, maskWidth, maskHeight, maskRotation, maskRoundness or maskFeather.",
            read_only: false,
            destructive: false,
            idempotent: true,
            action: Action::Mask,
        },
        command(
            "clip_stabilize",
            "Stabilize a clip",
            "Takes the camera's shake out of a video clip, or with `stabilize: null` stops. `smoothness` 0-1 (low keeps pans and removes jitter, high leaves only the slowest moves), `maxZoom` (at least 1) is how far the picture may be enlarged to hide the moving edges, `tripod: true` locks the frame entirely. The shake is measured when the clip is previewed or exported.",
            "setClipStabilize",
        ),
        command(
            "clip_frame",
            "Fit or fill the frame",
            "Sizes a picture to the frame in one step: `framing` \"fit\" shows all of it (bars where the shapes differ), \"fill\" covers the frame and crops the overhang. Where the clip's scale is keyframed, the key goes at `at` (a fraction of the clip, 0-1).",
            "frameClip",
        ),
        command(
            "timeline_background",
            "Set the background",
            "Sets what shows behind pictures that do not cover the frame, on a whole timeline (`timelineId` from `timeline_read`): `{\"kind\": \"none\"}` (black), `{\"kind\": \"blur\", \"amount\": 0-1}` (the clip itself, enlarged and blurred), `{\"kind\": \"color\", \"color\": \"#rrggbb\"}` or `{\"kind\": \"image\", \"mediaId\": ...}`. One clip's own background is `clip_update`'s `background`.",
            "setTimelineBackground",
        ),
        command(
            "clip_transform",
            "Place, scale or rotate a clip",
            "Sets a picture's placement; only the fields given change. scale 0.05-8 (1 fits the frame), offsetX/offsetY as fractions of the frame's width/height from the centre (positive is right/down, within ±3), rotation in degrees, stretchX/stretchY 0.1-10.",
            "setClipTransform",
        ),
        command(
            "clip_speed",
            "Change a clip's speed",
            "Plays a clip faster or slower: `speed` from 0.0625 to 16 (2 is double speed). The same stretch of source is kept, so the clip's length on the timeline changes.",
            "setClipSpeed",
        ),
        command(
            "title_add",
            "Add a title",
            "Places a title (text over the picture) at `start` seconds for `duration` seconds (4 s when absent). `style` sets the words and look - `content`, `fontFamily`, `fontSize` (fraction of frame height, 0.09 by default), `fontWeight` (100-900), `color` (\"#rrggbb\"), `align`, `background`, `strokeWidth`... - and needs only the fields that differ from the default. `offsetY` places it vertically as a fraction of frame height from the centre (positive is down). With no `trackId` and `above: true` it lands on a lane above the picture. The new clip's id is `createdId`.",
            "addTextClip",
        ),
        Tool {
            name: "captions_add",
            title: "Add captions",
            description: "Places a run of captions in one undoable step: one title per entry, from `start` to `end` seconds, each on a lane above the picture. `style` applies to all of them (its `content` is ignored); without one they look like the window's captions - Hanken Grotesk, weight 600, 0.04 of the frame high. `offsetY` is where they sit, as a fraction of frame height from the centre: 0.35 (the default) is near the bottom, 0 the middle, -0.35 the top.",
            read_only: false,
            destructive: false,
            idempotent: false,
            action: Action::Captions,
        },
        command(
            "keyframe_set",
            "Set a keyframe",
            "Animates a clip: puts a key on `property` (scale, offsetX, offsetY, rotation, opacity, volume, or a mask's maskX, maskY, maskWidth, maskHeight, maskRotation, maskRoundness, maskFeather) at `at`, a fraction of the clip's length from 0 (its start) to 1 (its end), holding `value` in the property's own units - the number the inspector shows. A key already within a hair of `at` is replaced. `ease` is how the value is approached: [x1, y1, x2, y2] like a CSS cubic-bezier, linear when absent.",
            "setClipKey",
        ),
        command(
            "keyframe_clear",
            "Remove a keyframe",
            "Removes the key on `property` at `at` (a fraction of the clip's length), if there is one.",
            "clearClipKey",
        )
        .destroying(),
        command(
            "effect_layer_add",
            "Add an effect layer",
            "Places an effect or look as a layer over the timeline: from `start` for `duration` seconds (5 s when absent), it treats everything beneath it. `effectId` is a package id from `effects_list`; the layer's opacity (via `clip_update`) is how strongly it applies. To put an effect on one clip instead, set that clip's `videoEffects` with `clip_update`.",
            "addLayerClip",
        ),
        command(
            "effect_key_set",
            "Animate an effect parameter",
            "Puts a key on one parameter (`key`, from `effects_list`) of one effect in a clip's chain (`entry`, its index in the clip's videoEffects, from 0) at `at` (a fraction of the clip's length, 0-1), holding `value` in the parameter's own units. Keep the value within the range `effects_list` gives.",
            "setEffectKey",
        ),
        command(
            "track_add",
            "Add a lane",
            "Adds a lane to the active timeline: on top, or with `bottom: true` under every other - on a timeline with picture and sound lanes, a video lane on top and an audio lane at the bottom. Its id is `createdId`.",
            "addTrack",
        ),
        method(
            "effects_list",
            "List effects",
            "Every effect package this build has - id, name, kind (\"effect\", \"filter\", \"audio\", \"transition\" or \"generator\"), category and each parameter's key, range, default and unit - so an effect chain, a layer or a transition can be built correctly. `kind` narrows the list.",
            "catalogue.list",
            Raw,
        )
        .reading(),
        method(
            "template_list",
            "List templates",
            "The template library: each template's folder, name and the slots (placeholder media ids) a project made from it must fill.",
            "template.list",
            Raw,
        )
        .reading(),
        method(
            "template_instantiate",
            "Make a project from a template",
            "Makes a project from a template - `template` (its folder), `location` and `name` as for `project_create` - with every slot filled by a file (`fills`: [{mediaId, file}]). Opens it and returns its summary.",
            "template.instantiate",
            Summary,
        ),
        method(
            "export_start",
            "Start an export",
            "Starts rendering an open project's active timeline to `output` (an .mp4 path), as the window's Export does. Returns at once with a `job` id; follow it with `export_wait` or `export_status`. Optional: crf (quality, lower is better, 20 by default), preset (x264 speed, \"medium\"), width/height, rateNum/rateDen, codec (\"h264\", \"hevc\" or \"av1\"), tenBit, colorRange, hdr. `loudness` (LUFS: -14 for Reels, TikTok, Shorts and YouTube, -16 for podcasts, -23 for broadcast) measures the mix by EBU R128 and normalises it. One export runs at a time; another is refused as busy.",
            "export.run",
            Raw,
        ),
        method(
            "export_status",
            "Export status",
            "How an export job stands, at once: `state` (\"running\", \"done\", \"cancelled\" or \"failed\"), `stage` (\"cutout\", \"stabilize\", \"video\", \"audio\", \"mux\"), frames done of total, `fraction` of the stage (0-1), and once done the file and its size, or the error.",
            "export.status",
            Raw,
        )
        .reading(),
        Tool {
            name: "export_wait",
            title: "Wait for an export",
            description: "Waits for an export job to end, up to `timeoutSeconds` (60 by default, 600 at most), reporting progress as it goes, and returns its status as `export_status` does. If it is still running when the time is up, call it again.",
            read_only: true,
            destructive: false,
            idempotent: true,
            action: Action::ExportWait,
        },
        method(
            "export_cancel",
            "Cancel an export",
            "Stops a running export job at its next frame; it then ends as \"cancelled\".",
            "export.cancel",
            Raw,
        )
        .destroying(),
        Tool {
            name: "preview_frame",
            title: "Preview a frame",
            description: "Renders the true composited frame at `time` seconds on an open project's timeline and returns it as an image to look at - titles, effects and transforms included. Without `width` and `height` it fits within 1280 pixels; give both for another size (8192 at most). With `output` (a .png path) it is written there instead.",
            read_only: true,
            destructive: false,
            idempotent: true,
            action: Action::PreviewFrame,
        },
        Tool {
            name: "frame_levels",
            title: "Measure a frame",
            description: "Measures the composited frame at `time` seconds as a colourist reads the scopes, in IRE (0 black, 100 white, display-encoded): luma's low, median and high points and its mean, how much is crushed at black or clipped at white, the colour cast of the neutral areas in the shadows, midtones and highlights (red and blue against green; 0 is neutral), saturation, and how far skin tones sit from the skin-tone line (123° on the vectorscope). Use it before and after grading, with preview_frame to look; `notes` says in words what stands out.",
            read_only: true,
            destructive: false,
            idempotent: true,
            action: Action::Levels,
        },
        Tool {
            name: "media_generate",
            title: "Generate a clip or a picture",
            description: "Makes a clip (Higgsfield's Seedance 2.5, or Seedance through BytePlus) or a picture (Gemini, or Higgsfield's Soul) from `prompt` with the keys and models the person set in Settings › Assistant and AI, writes it into the project's `generated` folder and adds it to the media, marked as generated. Returns its `mediaId`; place it with clip_add. `fromTime` starts a clip from the timeline's frame at that time, so it continues the shot. Describe the shot as a cinematographer would - subject, action, shot size, angle, lens, light, color, movement - and match the footage around it. A request takes one to several minutes and is billed to the person's account with the service: generate only when asked or clearly wanted, and say what it will cost them in time.",
            read_only: false,
            destructive: false,
            idempotent: false,
            action: Action::Generate,
        },
    ]
}

/// What a tool call came to: MCP content, and whether it is an error the
/// agent should read and correct.
pub struct Outcome {
    /// The content blocks.
    pub content: Vec<Value>,
    /// The same result as JSON, for a client that reads it structured.
    pub structured: Option<Value>,
    /// A tool execution error.
    pub is_error: bool,
}

impl Outcome {
    fn json(value: Value) -> Outcome {
        let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
        Outcome {
            content: vec![json!({ "type": "text", "text": text })],
            structured: value.is_object().then_some(value),
            is_error: false,
        }
    }

    fn error(error: &ApiError) -> Outcome {
        let code = serde_json::to_value(error.code)
            .ok()
            .and_then(|code| code.as_str().map(str::to_owned))
            .unwrap_or_default();
        Outcome {
            content: vec![
                json!({ "type": "text", "text": format!("{} (code: {code})", error.message) }),
            ],
            structured: Some(json!({ "error": { "code": code, "message": error.message } })),
            is_error: true,
        }
    }
}

/// What a call may use besides its arguments.
pub struct Context<'a> {
    /// Where requests go.
    pub backend: &'a mut dyn Backend,
    /// Set when the client cancels the call.
    pub cancelled: &'a AtomicBool,
    /// Reports progress, when the client asked for it: done so far, out
    /// of a total, and a sentence.
    pub progress: Option<&'a dyn Fn(f64, f64, String)>,
}

/// The tools with their schemas, made once.
pub struct Toolbox {
    tools: Vec<(Tool, Value)>,
    schemas: Schemas,
}

impl Toolbox {
    /// Every tool in [`catalogue`], with the schema the API gives it.
    /// Panics on a tool whose method or command the API does not have: a
    /// test builds this, so the panic is a failed build, not a user's.
    pub fn new() -> Toolbox {
        let schemas = Schemas::generate();
        let tools = catalogue()
            .into_iter()
            .map(|tool| {
                let schema = input_schema(&schemas, &tool)
                    .unwrap_or_else(|| panic!("the API has nothing for the {} tool", tool.name));
                (tool, schema)
            })
            .collect();
        Toolbox { tools, schemas }
    }

    /// `tools/list`'s list.
    pub fn list(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|(tool, schema)| {
                json!({
                    "name": tool.name,
                    "title": tool.title,
                    "description": tool.description,
                    "inputSchema": schema,
                    "annotations": {
                        "title": tool.title,
                        "readOnlyHint": tool.read_only,
                        "destructiveHint": tool.destructive,
                        "idempotentHint": tool.idempotent,
                        "openWorldHint": false,
                    },
                })
            })
            .collect()
    }

    /// The tool called `name`, with its schema.
    pub fn find(&self, name: &str) -> Option<&(Tool, Value)> {
        self.tools.iter().find(|(tool, _)| tool.name == name)
    }

    /// The schemas tools are cut from.
    pub fn schemas(&self) -> &Schemas {
        &self.schemas
    }

    /// Runs the tool `name`; `None` when there is no such tool.
    pub fn call(&self, name: &str, arguments: Value, context: Context<'_>) -> Option<Outcome> {
        let (tool, _) = self.find(name)?;
        let arguments = match arguments {
            Value::Null => Value::Object(Map::new()),
            other => other,
        };
        Some(match run(tool, arguments, context) {
            Ok(outcome) => outcome,
            Err(error) => Outcome::error(&error),
        })
    }
}

impl Default for Toolbox {
    fn default() -> Self {
        Toolbox::new()
    }
}

/// The schema a tool's arguments follow.
fn input_schema(schemas: &Schemas, tool: &Tool) -> Option<Value> {
    match tool.action {
        Action::Method { method, .. } => schemas.method(method),
        Action::Command { op } => schemas.command(op),
        Action::Version => schemas.method("version"),
        Action::TimelineRead => schemas.method("project.get"),
        Action::ExportWait => {
            let mut schema = schemas.method("export.status")?;
            schema["properties"]["timeoutSeconds"] = json!({
                "type": "number",
                "minimum": 0,
                "maximum": WAIT_LONGEST,
                "default": WAIT_DEFAULT,
                "description": "How long to wait, in seconds.",
            });
            Some(schema)
        }
        Action::PreviewFrame => schemas.method("preview.frame"),
        Action::Levels => {
            let frame = schemas.method("preview.frame")?;
            Some(json!({
                "type": "object",
                "properties": {
                    "path": frame.pointer("/properties/path")?.clone(),
                    "time": frame.pointer("/properties/time")?.clone(),
                },
                "required": ["path", "time"],
            }))
        }
        Action::Generate => {
            let path = schemas
                .method("project.get")?
                .pointer("/properties/path")?
                .clone();
            Some(json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "kind": { "type": "string", "enum": ["clip", "picture"], "description": "A clip or a picture." },
                    "prompt": { "type": "string", "minLength": 1, "description": "The shot or the picture, described." },
                    "service": { "type": "string", "enum": ["higgsfield", "seedance", "gemini"], "description": "Who makes it: higgsfield (clips and pictures), seedance (clips, BytePlus) or gemini (pictures). Without it, the first with a key." },
                    "fromTime": { "type": ["number", "null"], "minimum": 0, "description": "A clip only: start from the timeline's frame at this time, in seconds." },
                    "duration": { "type": "integer", "minimum": 4, "maximum": 30, "default": 5, "description": "A clip's length in seconds." },
                    "resolution": { "type": "string", "enum": ["480p", "720p", "1080p"], "default": "720p", "description": "A clip's resolution." },
                    "aspect": { "type": "string", "enum": ASPECTS, "description": "The shape; the project's own when not given." },
                    "audio": { "type": "boolean", "default": true, "description": "A clip with sound made for it." },
                    "size": { "type": "string", "enum": ["1K", "2K", "4K"], "default": "2K", "description": "A Gemini picture's size." },
                },
                "required": ["path", "kind", "prompt"],
            }))
        }
        Action::Mask => {
            let update = schemas.command("updateClip")?;
            let mut mask = schemas.def("ClipMask")?;
            mask["type"] = json!(["object", "null"]);
            mask["description"] = json!("The shape and its numbers, or null to take the mask off.");
            Some(json!({
                "type": "object",
                "properties": {
                    "path": update.pointer("/properties/path")?.clone(),
                    "clipId": update.pointer("/properties/clipId")?.clone(),
                    "mask": mask,
                },
                "required": ["path", "clipId", "mask"],
            }))
        }
        Action::Captions => {
            let path = schemas
                .method("project.get")?
                .pointer("/properties/path")?
                .clone();
            let mut style = schemas.def("TextStyle")?;
            style["description"] = json!(
                "The look every caption shares, as a title's style; only the fields that differ from the default are needed. Its content is replaced by each caption's text."
            );
            Some(json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "captions": {
                        "type": "array",
                        "minItems": 1,
                        "description": "The captions, each shown from start to end.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "start": { "type": "number", "minimum": 0, "description": "When it appears, in seconds on the timeline." },
                                "end": { "type": "number", "minimum": 0, "description": "When it goes, in seconds on the timeline; after start." },
                                "text": { "type": "string", "description": "The words. A newline breaks the line." },
                            },
                            "required": ["start", "end", "text"],
                        },
                    },
                    "style": style,
                    "offsetY": {
                        "type": "number",
                        "default": 0.35,
                        "minimum": -3,
                        "maximum": 3,
                        "description": "Vertical placement as a fraction of frame height from the centre, positive down.",
                    },
                },
                "required": ["path", "captions"],
            }))
        }
    }
}

/// A request from a method's arguments.
fn request(method: &str, mut arguments: Value) -> Result<Request, ApiError> {
    let Some(fields) = arguments.as_object_mut() else {
        return Err(ApiError::invalid("the arguments are an object"));
    };
    fields.insert("method".to_owned(), json!(method));
    serde_json::from_value(arguments)
        .map_err(|error| ApiError::invalid(format!("invalid arguments: {error}")))
}

/// `edit.apply` of one command, from the command's arguments and the
/// project's `path` beside them.
fn edit(op: &str, mut arguments: Value) -> Result<Request, ApiError> {
    let Some(fields) = arguments.as_object_mut() else {
        return Err(ApiError::invalid("the arguments are an object"));
    };
    let path = fields
        .remove("path")
        .ok_or_else(|| ApiError::invalid("invalid arguments: missing field `path`"))?;
    fields.insert("op".to_owned(), json!(op));
    request("edit.apply", json!({ "path": path, "command": arguments }))
}

fn run(tool: &Tool, arguments: Value, context: Context<'_>) -> Result<Outcome, ApiError> {
    let backend = context.backend;
    match tool.action {
        Action::Method { method, reply } => {
            let result = backend.call(request(method, arguments)?)?;
            Ok(Outcome::json(match reply {
                Reply::Raw => result,
                Reply::Summary => summary::view(&result),
            }))
        }
        Action::Command { op } => {
            let result = backend.call(edit(op, arguments)?)?;
            Ok(Outcome::json(summary::view(&result)))
        }
        Action::Version => {
            let mut result = backend.call(request("version", arguments)?)?;
            result["mcp"] = backend.describe();
            Ok(Outcome::json(result))
        }
        Action::TimelineRead => {
            let result = backend.call(request("project.get", arguments)?)?;
            Ok(Outcome::json(summary::view(&result)))
        }
        Action::Captions => {
            let result = backend.call(captions(arguments)?)?;
            Ok(Outcome::json(summary::view(&result)))
        }
        Action::ExportWait => export_wait(arguments, backend, context.cancelled, context.progress),
        Action::PreviewFrame => preview(arguments, backend),
        Action::Mask => {
            let result = backend.call(mask(arguments)?)?;
            Ok(Outcome::json(summary::view(&result)))
        }
        Action::Generate => generate(arguments, backend, context.cancelled, context.progress),
        Action::Levels => levels(arguments, backend),
    }
}

/// The longest side of the frame `frame_levels` measures: enough for the
/// numbers, little to draw.
const LEVELS_SIDE: f64 = 640.0;

/// `frame_levels`: the frame drawn small, read back, and measured.
fn levels(arguments: Value, backend: &mut dyn Backend) -> Result<Outcome, ApiError> {
    use base64::Engine as _;
    let path = arguments.get("path").cloned().unwrap_or(Value::Null);
    let time = arguments.get("time").cloned().unwrap_or(Value::Null);
    let view = backend.call(request("project.get", json!({ "path": path }))?)?;
    let side = |key: &str, default: f64| {
        view.pointer(&format!("/settings/{key}"))
            .and_then(Value::as_f64)
            .unwrap_or(default)
    };
    let (width, height) = (side("width", 1920.0), side("height", 1080.0));
    let scale = (LEVELS_SIDE / width.max(height)).min(1.0);
    let result = backend.call(request(
        "preview.frame",
        json!({
            "path": path,
            "time": time,
            "width": ((width * scale).round() as u32).max(2),
            "height": ((height * scale).round() as u32).max(2),
        }),
    )?)?;
    let png = result
        .get("png")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::failed("the frame came back without its picture"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png)
        .map_err(|error| ApiError::failed(format!("the frame would not decode: {error}")))?;
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|error| ApiError::failed(format!("the frame would not decode: {error}")))?;
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|error| ApiError::failed(format!("the frame would not decode: {error}")))?;
    let channels = info.color_type.samples();
    let mut measured = measure(&pixels[..info.buffer_size()], channels);
    measured["time"] = time;
    Ok(Outcome::json(measured))
}

/// A frame of 8-bit pixels, `channels` bytes each (RGB first), measured
/// as the scopes would read it: see the `frame_levels` tool.
pub(crate) fn measure(pixels: &[u8], channels: usize) -> Value {
    let ire = |value: f64| (value * 1000.0).round() / 10.0;
    let mut luma = [0u64; 256];
    let (mut crushed, mut clipped, mut total) = (0u64, 0u64, 0u64);
    let mut luma_sum = 0.0;
    let mut sat_sum = 0.0;
    let mut sats = [0u64; 101];
    // Neutral areas by band: shadows, midtones, highlights, as summed
    // red, green, blue and a count.
    let mut neutral = [[0.0f64; 4]; 3];
    let (mut skin_cb, mut skin_cr, mut skin) = (0.0f64, 0.0f64, 0u64);
    // Pure black over a tenth of the frame is bars around the picture - a
    // vertical clip in a wide frame - or an empty frame, not shadows the
    // grade crushed: said on its own and left out of the rest.
    let stride = channels.max(3);
    let all = pixels.len() / stride;
    let pure_black = pixels
        .chunks_exact(stride)
        .filter(|pixel| pixel[..3] == [0, 0, 0])
        .count();
    let bars = all > 0 && pure_black * 10 >= all;
    for pixel in pixels.chunks_exact(stride) {
        if bars && pixel[..3] == [0, 0, 0] {
            continue;
        }
        let [r, g, b] = [pixel[0], pixel[1], pixel[2]].map(|c| f64::from(c) / 255.0);
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let (high, low) = (r.max(g).max(b), r.min(g).min(b));
        let sat = if high > 1e-4 {
            (high - low) / high
        } else {
            0.0
        };
        total += 1;
        luma[((y * 255.0).round() as usize).min(255)] += 1;
        luma_sum += y;
        sat_sum += sat;
        sats[((sat * 100.0).round() as usize).min(100)] += 1;
        if y <= 2.0 / 255.0 {
            crushed += 1;
        }
        if high >= 254.0 / 255.0 {
            clipped += 1;
        }
        if sat < 0.12 && y > 0.06 && y < 0.94 {
            let band = if y < 1.0 / 3.0 {
                0
            } else if y < 2.0 / 3.0 {
                1
            } else {
                2
            };
            for (slot, value) in neutral[band].iter_mut().zip([r, g, b, 1.0]) {
                *slot += value;
            }
        }
        let hue = hue_of(r, g, b);
        if (5.0..=45.0).contains(&hue) && (0.12..=0.65).contains(&sat) && y > 0.2 && y < 0.9 {
            skin_cb += (b - y) / 1.8556;
            skin_cr += (r - y) / 1.5748;
            skin += 1;
        }
    }
    if total == 0 {
        return json!({
            "pureBlackPercent": 100.0,
            "notes": ["the frame is pure black: nothing on the timeline here, or a fade at its darkest"],
        });
    }
    let share = |count: u64| (count as f64 / total as f64 * 1000.0).round() / 10.0;
    let percentile = |counts: &[u64], fraction: f64, scale: f64| {
        let wanted = (total as f64 * fraction).ceil() as u64;
        let mut seen = 0;
        for (index, count) in counts.iter().enumerate() {
            seen += count;
            if seen >= wanted.max(1) {
                return index as f64 / scale;
            }
        }
        (counts.len() - 1) as f64 / scale
    };
    let mut notes: Vec<String> = Vec::new();
    let bars_share = (pure_black as f64 / all.max(1) as f64 * 1000.0).round() / 10.0;
    if bars {
        notes.push(format!(
            "{bars_share}% of the frame is pure black - bars around the picture, or an empty frame - and is left out of the other numbers"
        ));
    }
    let (clip_share, crush_share) = (share(clipped), share(crushed));
    if clip_share >= 1.0 {
        notes.push(format!("{clip_share}% of the pixels are clipped at white"));
    }
    if crush_share >= 1.0 {
        notes.push(format!("{crush_share}% of the pixels are crushed at black"));
    }
    let low = ire(percentile(&luma, 0.005, 255.0));
    let high = ire(percentile(&luma, 0.995, 255.0));
    if high < 85.0 {
        notes.push(format!(
            "the brightest point is only {high} IRE: room to lift the highlights"
        ));
    }
    if low > 10.0 {
        notes.push(format!(
            "the darkest point is {low} IRE: the blacks are lifted"
        ));
    }
    let names = ["shadows", "midtones", "highlights"];
    let mut cast = Map::new();
    for (band, sums) in neutral.iter().enumerate() {
        if sums[3] < (total as f64 * 0.002).max(20.0) {
            continue;
        }
        let [r, g, b] = [sums[0] / sums[3], sums[1] / sums[3], sums[2] / sums[3]];
        let (red, blue) = (ire(r - g), ire(b - g));
        cast.insert(names[band].to_owned(), json!({ "red": red, "blue": blue }));
        let lean =
            |amount: f64, warm: &str, cold: &str| if amount > 0.0 { warm } else { cold }.to_owned();
        if red.abs() >= 2.0 || blue.abs() >= 2.0 {
            let (axis, amount) = if red.abs() >= blue.abs() {
                (lean(red, "red", "cyan"), red.abs())
            } else {
                (lean(blue, "blue", "yellow"), blue.abs())
            };
            notes.push(format!(
                "the neutrals in the {} lean {axis} by {amount} IRE",
                names[band]
            ));
        }
    }
    let skin_line = if skin as f64 >= total as f64 * 0.01 {
        let angle = (skin_cr / skin as f64)
            .atan2(skin_cb / skin as f64)
            .to_degrees();
        let off = ((angle - 123.0) * 10.0).round() / 10.0;
        if off.abs() >= 5.0 {
            notes.push(format!(
                "skin tones sit {}° {} of the skin-tone line",
                off.abs(),
                if off > 0.0 {
                    "towards yellow-green"
                } else {
                    "towards red-magenta"
                }
            ));
        }
        json!({ "share": share(skin), "angle": (angle * 10.0).round() / 10.0, "offLine": off })
    } else {
        Value::Null
    };
    json!({
        "luma": {
            "low": low,
            "median": ire(percentile(&luma, 0.5, 255.0)),
            "high": high,
            "mean": ire(luma_sum / total as f64),
            "crushedPercent": crush_share,
            "clippedPercent": clip_share,
        },
        "pureBlackPercent": if bars { bars_share } else { 0.0 },
        "neutralCast": cast,
        "saturation": {
            "mean": (sat_sum / total as f64 * 100.0).round(),
            "p95": (percentile(&sats, 0.95, 1.0)).round(),
        },
        "skin": skin_line,
        "notes": notes,
    })
}

/// A colour's hue in degrees, 0 for a grey.
fn hue_of(r: f64, g: f64, b: f64) -> f64 {
    let (high, low) = (r.max(g).max(b), r.min(g).min(b));
    let d = high - low;
    if d < 1e-4 {
        return 0.0;
    }
    let h = if high == r {
        (g - b) / d
    } else if high == g {
        2.0 + (b - r) / d
    } else {
        4.0 + (r - g) / d
    };
    (h / 6.0).rem_euclid(1.0) * 360.0
}

/// The shapes a generated clip or picture can be asked in.
const ASPECTS: [&str; 6] = ["16:9", "9:16", "1:1", "4:3", "3:4", "21:9"];

/// The one of [`ASPECTS`] nearest a `width` × `height` frame.
fn nearest_aspect(width: f64, height: f64) -> &'static str {
    let ratio = (width.max(1.0) / height.max(1.0)).ln();
    ASPECTS
        .iter()
        .min_by(|a, b| {
            let off = |aspect: &str| {
                let (w, h) = aspect.split_once(':').unwrap_or(("16", "9"));
                let wanted = w.parse::<f64>().unwrap_or(16.0) / h.parse::<f64>().unwrap_or(9.0);
                (ratio - wanted.ln()).abs()
            };
            off(a).total_cmp(&off(b))
        })
        .copied()
        .unwrap_or("16:9")
}

/// `media_generate`: the frame to start from when asked, the service's
/// request and its wait, and the file added to the project's media.
fn generate(
    arguments: Value,
    backend: &mut dyn Backend,
    cancelled: &AtomicBool,
    progress: Option<&dyn Fn(f64, f64, String)>,
) -> Result<Outcome, ApiError> {
    use concat_host::genai::{self, ImageRequest, Keys, Models, Provider, VideoRequest};

    let invalid = |why: &str| ApiError::invalid(format!("invalid arguments: {why}"));
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let path = text("path").ok_or_else(|| invalid("missing field `path`"))?;
    let picture = match text("kind").as_deref() {
        Some("clip") => false,
        Some("picture") => true,
        _ => return Err(invalid("`kind` is \"clip\" or \"picture\"")),
    };
    let prompt = text("prompt")
        .map(|prompt| prompt.trim().to_owned())
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| invalid("`prompt` says what to make"))?;
    let dirs = concat_host::AppDirs::locate().map_err(ApiError::failed)?;
    let keys = Keys::load(&dirs.config);
    let models = Models::from_settings(&dirs.config);
    let offered = if picture {
        [Provider::Gemini, Provider::Higgsfield]
    } else {
        [Provider::Higgsfield, Provider::Seedance]
    };
    let provider = match text("service").as_deref() {
        Some("higgsfield") => Provider::Higgsfield,
        Some("seedance") => Provider::Seedance,
        Some("gemini") => Provider::Gemini,
        Some(other) => return Err(invalid(&format!("no service called `{other}`"))),
        None => offered
            .into_iter()
            .find(|provider| keys.has(*provider))
            .unwrap_or(offered[0]),
    };
    if !offered.contains(&provider) {
        return Err(invalid(if picture {
            "pictures are made by gemini or higgsfield"
        } else {
            "clips are made by higgsfield or seedance"
        }));
    }
    if !keys.has(provider) {
        return Err(ApiError::failed(
            "no key for this service: the person adds one in Settings › Assistant and AI",
        ));
    }
    let view = backend.call(request("project.get", json!({ "path": path }))?)?;
    let (width, height) = (
        view.pointer("/settings/width")
            .and_then(Value::as_f64)
            .unwrap_or(1920.0),
        view.pointer("/settings/height")
            .and_then(Value::as_f64)
            .unwrap_or(1080.0),
    );
    let aspect = match text("aspect") {
        Some(aspect) if ASPECTS.contains(&aspect.as_str()) => aspect,
        Some(_) => {
            return Err(invalid(
                "`aspect` is one of 16:9, 9:16, 1:1, 4:3, 3:4, 21:9",
            ));
        }
        None => nearest_aspect(width, height).to_owned(),
    };
    let folder = std::path::Path::new(&path).join("generated");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default();
    // The frame to start from, drawn by the API as preview_frame draws one,
    // no larger than 1920 on its long side.
    let frame = match arguments.get("fromTime").and_then(Value::as_f64) {
        Some(time) if !picture => {
            let scale = (1920.0 / width.max(height)).min(1.0);
            let even = |side: f64| (((side * scale) as u32) / 2 * 2).max(2);
            let output = folder.join(format!("frame-{stamp}.png"));
            backend.call(request(
                "preview.frame",
                json!({
                    "path": path,
                    "time": time,
                    "output": output.to_string_lossy(),
                    "width": even(width),
                    "height": even(height),
                }),
            )?)?;
            Some(output)
        }
        _ => None,
    };
    if let Some(report) = progress {
        report(
            0.0,
            1.0,
            format!("asked {provider:?}; a request takes one to several minutes"),
        );
    }
    let made = if picture {
        let request = ImageRequest {
            prompt,
            aspect,
            size: text("size").unwrap_or_else(|| "2K".to_owned()),
        };
        genai::generate_image(provider, &keys, &models, &request, &folder, cancelled)
    } else {
        let request = VideoRequest {
            prompt,
            frame,
            duration: arguments
                .get("duration")
                .and_then(Value::as_u64)
                .map_or(5, |seconds| seconds.clamp(4, 30) as u32),
            resolution: text("resolution").unwrap_or_else(|| "720p".to_owned()),
            aspect,
            audio: arguments
                .get("audio")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        };
        genai::generate_video(provider, &keys, &models, &request, &folder, cancelled)
    }
    .map_err(ApiError::failed)?;
    let file = made.to_string_lossy().into_owned();
    let mut item = concat_host::media::probe(&file)
        .map_err(ApiError::failed)?
        .to_new_media();
    item.origin = Some(concat_project::model::MediaOrigin::Generated);
    let result = backend.call(Request::EditApply {
        path,
        command: Box::new(concat_project::Command::AddMedia { item }),
    })?;
    let media_id = result
        .pointer("/project/media")
        .and_then(Value::as_array)
        .and_then(|media| {
            media
                .iter()
                .find(|item| item["path"].as_str() == Some(file.as_str()))
        })
        .and_then(|item| item["id"].as_str())
        .map(str::to_owned);
    Ok(Outcome::json(json!({
        "file": file,
        "mediaId": media_id,
        "service": format!("{provider:?}").to_lowercase(),
        "next": "Place it with clip_add, then grade and cut it to match the footage around it.",
    })))
}

/// `clip_mask`: a clip patch with only its mask.
fn mask(mut arguments: Value) -> Result<Request, ApiError> {
    let invalid = |why: &str| ApiError::invalid(format!("invalid arguments: {why}"));
    let fields = arguments
        .as_object_mut()
        .ok_or_else(|| invalid("the arguments are an object"))?;
    let path = fields
        .remove("path")
        .ok_or_else(|| invalid("missing field `path`"))?;
    let clip = fields
        .remove("clipId")
        .ok_or_else(|| invalid("missing field `clipId`"))?;
    let mask = fields
        .remove("mask")
        .ok_or_else(|| invalid("missing field `mask`"))?;
    if let Some(unknown) = fields.keys().next() {
        return Err(invalid(&format!("unknown field `{unknown}`")));
    }
    edit(
        "updateClip",
        json!({ "path": path, "clipId": clip, "patch": { "mask": mask } }),
    )
}

/// `captions_add`: one batch of titles.
fn captions(mut arguments: Value) -> Result<Request, ApiError> {
    let invalid = |why: &str| ApiError::invalid(format!("invalid arguments: {why}"));
    let fields = arguments
        .as_object_mut()
        .ok_or_else(|| invalid("the arguments are an object"))?;
    let path = fields
        .remove("path")
        .ok_or_else(|| invalid("missing field `path`"))?;
    let captions = match fields.remove("captions") {
        Some(Value::Array(captions)) if !captions.is_empty() => captions,
        _ => return Err(invalid("`captions` is a list of {start, end, text}")),
    };
    let offset = match fields.remove("offsetY") {
        None | Some(Value::Null) => 0.35,
        Some(value) => value
            .as_f64()
            .ok_or_else(|| invalid("`offsetY` is a number"))?,
    };
    let style = match fields.remove("style") {
        None | Some(Value::Null) => json!({
            "fontFamily": "Hanken Grotesk",
            "fontSize": 0.04,
            "fontWeight": 600,
        }),
        Some(Value::Object(style)) => Value::Object(style),
        Some(_) => return Err(invalid("`style` is an object")),
    };
    if let Some(unknown) = fields.keys().next() {
        return Err(invalid(&format!("unknown field `{unknown}`")));
    }
    let mut commands = Vec::with_capacity(captions.len());
    for (index, caption) in captions.iter().enumerate() {
        let number = |key: &str| caption.get(key).and_then(Value::as_f64);
        let (Some(start), Some(end), Some(text)) = (
            number("start"),
            number("end"),
            caption.get("text").and_then(Value::as_str),
        ) else {
            return Err(invalid(&format!(
                "caption {index} needs a start, an end and a text"
            )));
        };
        if !(start >= 0.0 && end > start) {
            return Err(invalid(&format!(
                "caption {index} ends at {end} s, not after its start at {start} s"
            )));
        }
        let mut style = style.clone();
        style["content"] = json!(text);
        commands.push(json!({
            "op": "addTextClip",
            "trackId": null,
            "above": true,
            "start": start,
            "duration": end - start,
            "style": style,
            "offsetY": offset,
        }));
    }
    request(
        "edit.apply",
        json!({ "path": path, "command": { "op": "batch", "commands": commands } }),
    )
}

/// `export_wait`: the job's status once it has ended, or when the time is
/// up, with progress reported on the way.
fn export_wait(
    mut arguments: Value,
    backend: &mut dyn Backend,
    cancelled: &AtomicBool,
    progress: Option<&dyn Fn(f64, f64, String)>,
) -> Result<Outcome, ApiError> {
    let timeout = match arguments
        .as_object_mut()
        .and_then(|fields| fields.remove("timeoutSeconds"))
    {
        None | Some(Value::Null) => WAIT_DEFAULT,
        Some(value) => value
            .as_f64()
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .ok_or_else(|| {
                ApiError::invalid("invalid arguments: `timeoutSeconds` is a number of seconds")
            })?
            .min(WAIT_LONGEST),
    };
    let status = request("export.status", arguments)?;
    let until = Instant::now() + Duration::from_secs_f64(timeout);
    let mut reported = -1.0_f64;
    loop {
        let mut current = backend.call(status.clone())?;
        let running = current["state"] == "running";
        if let Some(progress) = progress {
            let stage = current["stage"].as_str().unwrap_or_default();
            let place = STAGES.iter().position(|known| *known == stage).unwrap_or(0) as f64;
            let fraction = current["fraction"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
            let overall = if running {
                ((place + fraction) / STAGES.len() as f64 * 100.0).min(99.9)
            } else {
                100.0
            };
            // A progress value must rise; a stage that starts over at 0
            // says nothing new until it passes where the last one was.
            if overall > reported {
                reported = overall;
                let frames = match (current["frame"].as_i64(), current["total"].as_i64()) {
                    (Some(frame), Some(total)) if total > 0 => format!(" {frame}/{total}"),
                    _ => String::new(),
                };
                let state = current["state"].as_str().unwrap_or_default();
                progress(
                    (overall * 10.0).round() / 10.0,
                    100.0,
                    format!("{state} {stage}{frames}").trim().to_owned(),
                );
            }
        }
        if !running {
            return Ok(Outcome::json(current));
        }
        if cancelled.load(Ordering::Relaxed) {
            return Err(ApiError::new(
                ErrorCode::Cancelled,
                "stopped waiting; the export goes on",
            ));
        }
        if Instant::now() >= until {
            current["note"] = json!("still running: call export_wait again, or export_status");
            return Ok(Outcome::json(current));
        }
        std::thread::sleep(POLL);
    }
}

/// `preview_frame`: the picture as an image, or the file written.
fn preview(mut arguments: Value, backend: &mut dyn Backend) -> Result<Outcome, ApiError> {
    let wants_file = arguments
        .get("output")
        .is_some_and(|output| !output.is_null());
    let sized = ["width", "height"]
        .iter()
        .all(|key| arguments.get(*key).is_some_and(|value| !value.is_null()));
    if !wants_file && !sized {
        // The timeline's own frame, fitted within PREVIEW_SIDE.
        let path = arguments.get("path").cloned().unwrap_or(Value::Null);
        let view = backend.call(request("project.get", json!({ "path": path }))?)?;
        let width = view
            .pointer("/settings/width")
            .and_then(Value::as_f64)
            .unwrap_or(1920.0);
        let height = view
            .pointer("/settings/height")
            .and_then(Value::as_f64)
            .unwrap_or(1080.0);
        let scale = (PREVIEW_SIDE / width.max(height)).min(1.0);
        if let Some(fields) = arguments.as_object_mut() {
            fields.insert(
                "width".to_owned(),
                json!(((width * scale).round() as u32).max(1)),
            );
            fields.insert(
                "height".to_owned(),
                json!(((height * scale).round() as u32).max(1)),
            );
        }
    }
    let time = arguments.get("time").cloned().unwrap_or(Value::Null);
    let result = backend.call(request("preview.frame", arguments)?)?;
    let Some(png) = result.get("png").and_then(Value::as_str) else {
        return Ok(Outcome::json(result));
    };
    Ok(Outcome {
        content: vec![
            json!({ "type": "image", "data": png, "mimeType": "image/png" }),
            json!({
                "type": "text",
                "text": format!("The frame at {time} s, {}×{}.", result["width"], result["height"]),
            }),
        ],
        structured: Some(
            json!({ "width": result["width"], "height": result["height"], "time": time }),
        ),
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::check;

    /// One call each tool may take, as an agent would write it.
    fn samples() -> Vec<(&'static str, Value)> {
        vec![
            ("concat_version", json!({})),
            ("project_list", json!({})),
            (
                "project_create",
                json!({ "location": "/tmp", "name": "Cut", "video": { "width": 1080, "height": 1920, "rateNum": 30, "rateDen": 1 } }),
            ),
            ("project_open", json!({ "path": "/tmp/Cut" })),
            ("project_save", json!({ "path": "/tmp/Cut" })),
            ("project_close", json!({ "path": "/tmp/Cut", "save": true })),
            ("timeline_read", json!({ "path": "/tmp/Cut" })),
            ("project_get", json!({ "path": "/tmp/Cut" })),
            (
                "project_set_video",
                json!({ "path": "/tmp/Cut", "video": { "width": 1920, "height": 1080, "rateNum": 30000, "rateDen": 1001 } }),
            ),
            ("media_probe", json!({ "path": "/tmp/a.mp4" })),
            (
                "media_import",
                json!({ "path": "/tmp/Cut", "file": "/tmp/a.mp4" }),
            ),
            (
                "media_replace_motion",
                json!({ "path": "/tmp/Cut", "mediaId": "m3", "file": "/tmp/Cut/overlays/badge-2.mov", "motion": { "template": "badge", "props": { "text": "Nuevo", "seconds": 3 } } }),
            ),
            (
                "edit_apply",
                json!({ "path": "/tmp/Cut", "command": { "op": "addTrack" } }),
            ),
            ("edit_undo", json!({ "path": "/tmp/Cut" })),
            ("edit_redo", json!({ "path": "/tmp/Cut" })),
            (
                "clip_add",
                json!({ "path": "/tmp/Cut", "mediaId": "m1", "trackId": "t1", "start": 2.5 }),
            ),
            (
                "clip_trim",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "edge": "end", "delta": -1.0, "ripple": true }),
            ),
            (
                "clips_split",
                json!({ "path": "/tmp/Cut", "clipIds": ["c1"], "time": 3.0 }),
            ),
            (
                "clips_move",
                json!({ "path": "/tmp/Cut", "moves": [{ "clipId": "c1", "start": 0.0, "trackId": "t2" }] }),
            ),
            (
                "clips_remove",
                json!({ "path": "/tmp/Cut", "clipIds": ["c1"], "ripple": false }),
            ),
            (
                "clip_update",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "patch": { "volume": 0.5, "crop": null, "transitionIn": { "id": "cross-fade", "duration": 0.5 }, "videoEffects": [{ "id": "concat.warm", "params": { "amount": 0.4 } }] } }),
            ),
            (
                "clip_transform",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "scale": 1.2, "rotation": 15.0 }),
            ),
            (
                "clip_speed",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "speed": 0.5 }),
            ),
            (
                "title_add",
                json!({ "path": "/tmp/Cut", "start": 1.0, "duration": 3.0, "above": true, "style": { "content": "Hello", "fontSize": 0.08, "align": "left" }, "offsetY": -0.3 }),
            ),
            (
                "captions_add",
                json!({ "path": "/tmp/Cut", "captions": [{ "start": 0.0, "end": 1.5, "text": "Hi" }], "style": { "color": "#ffff00" }, "offsetY": 0.3 }),
            ),
            (
                "keyframe_set",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "property": "scale", "at": 0.0, "value": 1.0, "ease": [0.42, 0.0, 0.58, 1.0] }),
            ),
            (
                "keyframe_clear",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "property": "opacity", "at": 1.0 }),
            ),
            (
                "effect_layer_add",
                json!({ "path": "/tmp/Cut", "start": 0.0, "duration": 2.0, "effectId": "concat.warm", "name": "Warm", "trackId": null }),
            ),
            (
                "effect_key_set",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "entry": 0, "key": "amount", "at": 0.5, "value": 0.8 }),
            ),
            ("track_add", json!({ "path": "/tmp/Cut", "bottom": false })),
            (
                "clips_add_sequence",
                json!({ "path": "/tmp/Cut", "mediaIds": ["m1", "m2"], "start": 0.0 }),
            ),
            (
                "clip_mask",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "mask": { "shape": "heart", "width": 0.5, "feather": 0.1, "invert": true } }),
            ),
            (
                "clip_stabilize",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "stabilize": { "smoothness": 0.7, "maxZoom": 1.2, "tripod": false } }),
            ),
            (
                "clip_frame",
                json!({ "path": "/tmp/Cut", "clipId": "c1", "framing": "fill" }),
            ),
            (
                "timeline_background",
                json!({ "path": "/tmp/Cut", "timelineId": "tl1", "background": { "kind": "blur", "amount": 0.5 } }),
            ),
            ("effects_list", json!({ "kind": "transition" })),
            ("template_list", json!({})),
            (
                "template_instantiate",
                json!({ "template": "/t", "location": "/tmp", "name": "From", "fills": [{ "mediaId": "m1", "file": "/tmp/a.mp4" }] }),
            ),
            (
                "export_start",
                json!({ "path": "/tmp/Cut", "output": "/tmp/cut.mp4", "crf": 23, "codec": "h264" }),
            ),
            ("export_status", json!({ "job": "j1" })),
            ("export_wait", json!({ "job": "j1", "timeoutSeconds": 5 })),
            ("export_cancel", json!({ "job": "j1" })),
            (
                "preview_frame",
                json!({ "path": "/tmp/Cut", "time": 1.5, "width": 320, "height": 180 }),
            ),
            ("frame_levels", json!({ "path": "/tmp/Cut", "time": 1.5 })),
            (
                "media_generate",
                json!({ "path": "/tmp/Cut", "kind": "clip", "prompt": "A slow push-in on a barber's hands", "fromTime": 2.0, "duration": 6, "aspect": "9:16" }),
            ),
        ]
    }

    #[test]
    fn every_tool_has_a_schema_and_a_sample_and_names_are_unique() {
        let toolbox = Toolbox::new();
        let listed = toolbox.list();
        let samples = samples();
        assert_eq!(listed.len(), samples.len(), "a sample for every tool");
        let mut names: Vec<&str> = listed
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), listed.len(), "unique names");
        for tool in &listed {
            let name = tool["name"].as_str().expect("named");
            assert!(
                name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{name}"
            );
            assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
            assert!(
                tool["description"]
                    .as_str()
                    .is_some_and(|text| text.len() > 40),
                "{name}"
            );
        }
    }

    /// The round trip: what each tool's schema accepts is what its Rust
    /// type reads, and what the Rust type writes the schema accepts.
    #[test]
    fn every_sample_fits_its_schema_and_reads_as_its_request() {
        let toolbox = Toolbox::new();
        for (name, sample) in samples() {
            let (tool, schema) = toolbox.find(name).unwrap_or_else(|| panic!("no {name}"));
            check(schema, &sample, schema, name).unwrap_or_else(|why| panic!("{why}"));
            let built = match tool.action {
                Action::Method { method, .. } => request(method, sample.clone()),
                Action::Command { op } => edit(op, sample.clone()),
                Action::Version => request("version", sample.clone()),
                Action::TimelineRead => request("project.get", sample.clone()),
                Action::Captions => captions(sample.clone()),
                Action::ExportWait => {
                    let mut sample = sample.clone();
                    sample
                        .as_object_mut()
                        .map(|fields| fields.remove("timeoutSeconds"));
                    request("export.status", sample)
                }
                Action::PreviewFrame => request("preview.frame", sample.clone()),
                Action::Mask => mask(sample.clone()),
                // Composed: its first request is the project's settings.
                Action::Generate | Action::Levels => {
                    request("project.get", json!({ "path": sample["path"] }))
                }
            };
            let built = built.unwrap_or_else(|error| panic!("{name}: {error}"));
            // Back again: the request as the API writes it fits the API's
            // own schema for requests.
            let written = serde_json::to_value(&built).expect("writes");
            let root = concat_api::request_schema();
            check(&root, &written, &root, name).unwrap_or_else(|why| panic!("{why}"));
        }
    }

    /// A frame of one colour, `pixels` of them, as RGBA bytes.
    fn flat(rgb: [u8; 3], pixels: usize) -> Vec<u8> {
        (0..pixels)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect()
    }

    #[test]
    fn a_grey_frame_measures_neutral_and_a_blue_one_leans_blue() {
        let grey = measure(&flat([128, 128, 128], 1000), 4);
        assert_eq!(grey["luma"]["median"], 50.2);
        assert_eq!(grey["neutralCast"]["midtones"]["red"], 0.0);
        assert_eq!(grey["luma"]["clippedPercent"], 0.0);
        assert!(grey["notes"].as_array().is_some_and(|notes| {
            notes
                .iter()
                .all(|note| !note.as_str().unwrap_or_default().contains("lean"))
        }));
        let cold = measure(&flat([124, 128, 136], 1000), 4);
        let blue = cold["neutralCast"]["midtones"]["blue"]
            .as_f64()
            .expect("a midtone cast");
        assert!(blue > 2.5, "{blue}");
        assert!(
            cold["notes"].to_string().contains("lean blue"),
            "{}",
            cold["notes"]
        );
    }

    #[test]
    fn bars_of_pure_black_are_said_apart_and_left_out() {
        let mut frame = flat([0, 0, 0], 600);
        frame.extend(flat([128, 128, 128], 400));
        let measured = measure(&frame, 4);
        assert_eq!(measured["pureBlackPercent"], 60.0);
        assert_eq!(measured["luma"]["crushedPercent"], 0.0);
        assert_eq!(measured["luma"]["median"], 50.2);
        // A little pure black is part of the picture, and counts as crushed.
        let mut frame = flat([0, 0, 0], 50);
        frame.extend(flat([128, 128, 128], 950));
        let measured = measure(&frame, 4);
        assert_eq!(measured["pureBlackPercent"], 0.0);
        assert_eq!(measured["luma"]["crushedPercent"], 5.0);
    }

    #[test]
    fn white_is_clipped_and_a_face_lands_near_the_skin_line() {
        let white = measure(&flat([255, 255, 255], 500), 4);
        assert_eq!(white["luma"]["clippedPercent"], 100.0);
        assert!(white["notes"].to_string().contains("clipped"));
        // A face's tone: R 0.8, G 0.6, B 0.5 sits about 2° off the line.
        let face = measure(&flat([204, 153, 128], 500), 4);
        let off = face["skin"]["offLine"].as_f64().expect("skin found");
        assert!(off.abs() < 5.0, "{off}");
    }

    #[test]
    fn a_wrong_argument_is_refused_by_the_schema_and_by_the_type_alike() {
        let toolbox = Toolbox::new();
        let (_, schema) = toolbox.find("clip_speed").expect("a tool");
        let wrong = json!({ "path": "/p", "clipId": "c1", "speed": "fast" });
        assert!(check(schema, &wrong, schema, "clip_speed").is_err());
        let error = edit("setClipSpeed", wrong).expect_err("refused");
        assert_eq!(error.code, ErrorCode::Invalid);
        let missing = request("project.open", json!({}));
        assert!(missing.expect_err("refused").message.contains("path"));
        assert!(
            captions(json!({ "path": "/p", "captions": [{ "start": 2, "end": 1, "text": "x" }] }))
                .is_err()
        );
    }
}
