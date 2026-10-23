// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! The Motion tab (Royalty Studio fork): a motion graphic's recipe as
//! fields, and a new render of it put in place of the old one.
//!
//! - concat-studio renders motion graphics from Remotion templates, and a
//!   bin item it rendered keeps its recipe ([`MediaItem::motion`]): the
//!   template and the props. Selecting a clip of one fills the tab from the
//!   recipe, laid out by the template's props schema (`overlay_templates`).
//! - Apply renders the recipe with the tab's edits, at the timeline's frame
//!   size and rate, with the same `overlay_render` an agent calls, and puts
//!   the render in place of the old one ([`Command::ReplaceMotion`]): one
//!   step in the undo history, and the clips stay where they are.
//! - concat-studio's MCP server runs as a child of the window from the first
//!   request on, fed by a thread of its own, so its Remotion bundle is built
//!   once; requests take turns on it.
//!
//! [`MediaItem::motion`]: concat_project::model::MediaItem::motion

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command as Process, Stdio};
use std::sync::mpsc;

use concat_project::Command;
use concat_project::commands::NewMedia;
use concat_project::model::{MediaOrigin, Motion as Recipe};
use serde_json::{Map, Value, json};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::host::Shell;
use crate::i18n::{t, tf};
use crate::studio::Studio;
use crate::ui::{App, Motion, MotionField};

const TEXT: i32 = 0;
const NUMBER: i32 = 1;
const SWITCH: i32 = 2;
const CHOICE: i32 = 3;

/// Props the tab never shows: the frame is the timeline's, set on every
/// render.
const FRAME: &[&str] = &["width", "height", "fps"];

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Text,
    Number {
        minimum: f64,
        maximum: f64,
        integer: bool,
    },
    Switch,
    Choice(Vec<String>),
}

/// One prop as the tab edits it.
#[derive(Clone, Debug, PartialEq)]
struct Spec {
    key: String,
    kind: Kind,
    hint: String,
}

/// A template as the tab lays it out.
#[derive(Debug, PartialEq)]
struct Layout {
    title: String,
    description: String,
    specs: Vec<Spec>,
    /// Props of the template the tab cannot edit (a list of words).
    hidden: Vec<String>,
}

/// What the tab is showing: the item, its recipe as it is in the bin, and
/// the fields laid out for it.
struct Shown {
    media_id: String,
    recipe: Recipe,
    specs: Vec<Spec>,
}

#[derive(Default)]
struct State {
    jobs: Option<mpsc::Sender<Job>>,
    /// `overlay_templates`, once concat-studio has answered.
    catalogue: Option<Value>,
    asking: bool,
    /// Templates the catalogue was asked for again and still lacked.
    missing: HashSet<String>,
    shown: Option<Shown>,
    /// The recipe's props with the tab's edits.
    draft: Map<String, Value>,
    busy: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

enum Job {
    Templates,
    Render {
        epoch: u64,
        media_id: String,
        template: String,
        props: Map<String, Value>,
        output: PathBuf,
    },
}

/// Wires the tab's callbacks.
pub fn install(app: &App) {
    let global = app.global::<Motion>();
    global.on_text_edited(|key, text| edit(&key, |_| Value::String(text.to_string())));
    global.on_number_changed(|key, value| {
        edit(&key, |kind| match kind {
            Some(Kind::Number { integer: true, .. }) => json!(value.round() as i64),
            // Through text, so 0.2 is stored as 0.2 and not as the f32 nearest.
            _ => serde_json::from_str(&format!("{value:.4}")).unwrap_or_else(|_| json!(value)),
        })
    });
    global.on_toggled(|key, on| edit(&key, |_| Value::Bool(on)));
    global.on_picked(|key, index| {
        edit(&key, |kind| match kind {
            Some(Kind::Choice(options)) => options
                .get(index.max(0) as usize)
                .map(|option| Value::String(option.clone()))
                .unwrap_or(Value::Null),
            _ => Value::Null,
        })
    });
    global.on_apply(|| {
        Shell::with(|shell, app| {
            apply(&shell.studio.borrow(), &app);
        })
    });
    global.on_revert(|| {
        STATE.with(|state| state.borrow_mut().shown = None);
        Shell::with(|shell, app| shell.studio.borrow().publish(&app, &shell.models));
    });
}

/// Writes one field's new value into the draft. `value` is told the field's
/// kind, and a null from it changes nothing.
fn edit(key: &str, value: impl FnOnce(Option<&Kind>) -> Value) {
    let changed = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let kind = state
            .shown
            .as_ref()
            .and_then(|shown| shown.specs.iter().find(|spec| spec.key == key))
            .map(|spec| spec.kind.clone());
        let value = value(kind.as_ref());
        if value.is_null() {
            return false;
        }
        state.draft.insert(key.to_owned(), value);
        true
    });
    if changed {
        Shell::with(|_, app| app.global::<Motion>().set_dirty(true));
    }
}

/// The tab, for the clip `clip_id` the inspector shows. Rebuilt only when
/// the item or its recipe is not the one already shown, so an edit in
/// progress survives every publish in between.
pub fn publish(studio: &Studio, app: &App, clip_id: &str) {
    let project = studio.project();
    let item = project
        .active()
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .and_then(|clip| project.media.iter().find(|item| item.id == clip.media_id))
        .filter(|item| item.motion.is_some());
    let Some(item) = item else {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.shown = None;
            state.draft.clear();
        });
        return;
    };
    let recipe = item.motion.clone().expect("filtered");
    let global = app.global::<Motion>();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state
            .shown
            .as_ref()
            .is_some_and(|shown| shown.media_id == item.id && shown.recipe == recipe)
        {
            return;
        }
        let Some(catalogue) = state.catalogue.as_ref() else {
            global.set_title(label_of_template(&recipe.template).into());
            global.set_description("".into());
            global.set_fields(ModelRc::default());
            global.set_dirty(false);
            global.set_note(t("motion.readingTemplates").into());
            if !state.asking {
                ask_templates(&mut state, studio);
            }
            return;
        };
        let Some(layout) = layout(catalogue, &recipe.template) else {
            // An animation Claude wrote after the catalogue was read: read
            // it again, once.
            if state.missing.insert(recipe.template.clone()) && !state.asking {
                state.catalogue = None;
                ask_templates(&mut state, studio);
                global.set_note(t("motion.readingTemplates").into());
            } else {
                global.set_note(tf("motion.unknownTemplate", &[&recipe.template]).into());
            }
            global.set_title(label_of_template(&recipe.template).into());
            global.set_description("".into());
            global.set_fields(ModelRc::default());
            global.set_dirty(false);
            return;
        };

        let fields: Vec<MotionField> = layout
            .specs
            .iter()
            .map(|spec| field(spec, recipe.props.get(&spec.key)))
            .collect();
        let video = project.active().video;
        let fps = video.rate_num as f64 / video.rate_den.max(1) as f64;
        let elsewhere = frame_differs(&recipe.props, video.width, video.height, fps);
        let mut notes = Vec::new();
        if let Some(rendered) = &elsewhere {
            notes.push(tf(
                "motion.otherFrame",
                &[
                    rendered,
                    &format!("{}×{}", video.width, video.height),
                    &fps_text(fps),
                ],
            ));
        }
        if !layout.hidden.is_empty() {
            notes.push(tf("motion.hiddenFields", &[&layout.hidden.join(", ")]));
        }
        global.set_title(layout.title.into());
        global.set_description(layout.description.into());
        global.set_fields(ModelRc::new(VecModel::from(fields)));
        global.set_dirty(elsewhere.is_some());
        global.set_note(notes.join("\n").into());
        state.draft = recipe.props.clone();
        state.shown = Some(Shown {
            media_id: item.id.clone(),
            recipe,
            specs: layout.specs,
        });
    });
}

/// Renders the draft at the timeline's frame, and puts the render in place
/// of the old one when it is done.
fn apply(studio: &Studio, app: &App) {
    let Some(folder) = studio
        .session
        .as_ref()
        .map(|session| PathBuf::from(session.path()))
    else {
        return;
    };
    let video = studio.project().active().video;
    let fps = video.rate_num as f64 / video.rate_den.max(1) as f64;
    let global = app.global::<Motion>();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.busy {
            return;
        }
        let Some(shown) = state.shown.as_ref() else {
            return;
        };
        let mut props = state.draft.clone();
        props.insert("width".to_owned(), json!(video.width));
        props.insert("height".to_owned(), json!(video.height));
        props.insert("fps".to_owned(), json!((fps * 1000.0).round() / 1000.0));
        let template = shown.recipe.template.clone();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis())
            .unwrap_or_default();
        let output = folder
            .join("overlays")
            .join(format!("{template}-{stamp}.mov"));
        let job = Job::Render {
            epoch: crate::host::project_epoch(),
            media_id: shown.media_id.clone(),
            template,
            props,
            output,
        };
        if send(&mut state, studio, job) {
            state.busy = true;
            global.set_busy(true);
            global.set_progress(0.0);
            global.set_note("".into());
        } else {
            global.set_note(t("motion.couldNotStart").into());
        }
    });
}

fn ask_templates(state: &mut State, studio: &Studio) {
    state.asking = send(state, studio, Job::Templates);
}

/// Hands `job` to the worker, starting it if it is not running.
fn send(state: &mut State, studio: &Studio, job: Job) -> bool {
    // A worker that has gone hands the job back, for a new one.
    let job = match &state.jobs {
        Some(jobs) => match jobs.send(job) {
            Ok(()) => return true,
            Err(mpsc::SendError(job)) => job,
        },
        None => job,
    };
    let (jobs, inbox) = mpsc::channel();
    let logs = studio.host.dirs.data.join("logs");
    let started = std::thread::Builder::new()
        .name("motion".to_owned())
        .spawn(move || work(inbox, &logs))
        .is_ok();
    if !started || jobs.send(job).is_err() {
        return false;
    }
    state.jobs = Some(jobs);
    true
}

// ---------------------------------------------------------------- results

/// `overlay_templates`' answer, on the event-loop thread.
fn catalogued(answer: Result<Value, String>) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.asking = false;
        // Laid out afresh on the publish after this.
        state.shown = None;
        match answer {
            Ok(catalogue) => state.catalogue = Some(catalogue),
            Err(error) => {
                log::warn!("motion: overlay_templates failed: {error}");
                // An empty catalogue rather than none, so a failure is said
                // once instead of asked again on every publish.
                state.catalogue = Some(json!({ "templates": [], "custom": [] }));
            }
        }
    });
}

/// A render's outcome, on the event-loop thread: put in place, or said;
/// None for a render whose project has closed since, which only frees the
/// tab.
fn finished(
    studio: &mut Studio,
    app: &App,
    media_id: String,
    result: Option<Result<NewMedia, String>>,
) {
    let global = app.global::<Motion>();
    STATE.with(|state| state.borrow_mut().busy = false);
    global.set_busy(false);
    global.set_progress(0.0);
    let Some(result) = result else {
        return;
    };
    match result {
        Ok(item) => {
            studio.apply(Command::ReplaceMotion { media_id, item });
            global.set_dirty(false);
            global.set_note("".into());
        }
        Err(error) => {
            log::warn!("motion: render failed: {error}");
            global.set_note(error.into());
        }
    }
}

// ----------------------------------------------------------------- layout

/// The template `id` as the tab lays it out: its own props in their order,
/// then the brand and the length. None when the catalogue has no such
/// template or animation.
fn layout(catalogue: &Value, id: &str) -> Option<Layout> {
    let listed = |shelf: &str| {
        catalogue[shelf]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["id"] == id))
    };
    let order = |entry: &Value| -> Vec<String> {
        entry["order"]
            .as_array()
            .map(|keys| {
                keys.iter()
                    .filter_map(|key| key.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut specs = Vec::new();
    let mut hidden = Vec::new();
    let description;
    if let Some(entry) = listed("templates") {
        description = entry["description"].as_str().unwrap_or_default().to_owned();
        let properties = &entry["props"]["properties"];
        for key in order(entry) {
            match spec_of_schema(&key, &properties[&key]) {
                Some(spec) => specs.push(spec),
                None => hidden.push(label_of(&key)),
            }
        }
    } else if let Some(entry) = listed("custom") {
        description = entry["description"].as_str().unwrap_or_default().to_owned();
        let defaults = &entry["defaults"];
        for key in order(entry) {
            if FRAME.contains(&key.as_str()) || key == "brand" || key == "seconds" {
                continue;
            }
            match spec_of_value(&key, &defaults[&key]) {
                Some(spec) => specs.push(spec),
                None => hidden.push(label_of(&key)),
            }
        }
    } else {
        return None;
    }
    let common = &catalogue["commonProps"]["properties"];
    specs.push(spec_of_schema("brand", &common["brand"]).unwrap_or(Spec {
        key: "brand".to_owned(),
        kind: Kind::Text,
        hint: String::new(),
    }));
    specs.push(Spec {
        key: "seconds".to_owned(),
        kind: Kind::Number {
            minimum: common["seconds"]["minimum"].as_f64().unwrap_or(0.3),
            maximum: common["seconds"]["maximum"].as_f64().unwrap_or(300.0),
            integer: false,
        },
        hint: common["seconds"]["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    });
    Some(Layout {
        title: label_of_template(id),
        description,
        specs,
        hidden,
    })
}

/// A prop from its JSON Schema; None for a list or an object.
fn spec_of_schema(key: &str, schema: &Value) -> Option<Spec> {
    let hint = schema["description"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let kind = if let Some(options) = schema["enum"].as_array() {
        Kind::Choice(
            options
                .iter()
                .filter_map(|option| option.as_str().map(str::to_owned))
                .collect(),
        )
    } else {
        match schema["type"].as_str() {
            Some("string") => Kind::Text,
            Some(kind @ ("number" | "integer")) => Kind::Number {
                minimum: schema["minimum"].as_f64().unwrap_or(-1.0e9),
                maximum: schema["maximum"].as_f64().unwrap_or(1.0e9),
                integer: kind == "integer",
            },
            Some("boolean") => Kind::Switch,
            _ => return None,
        }
    };
    Some(Spec {
        key: key.to_owned(),
        kind,
        hint,
    })
}

/// A prop of an animation of concat-studio's own, which has no schema: from
/// the value it was made with.
fn spec_of_value(key: &str, value: &Value) -> Option<Spec> {
    let kind = match value {
        Value::String(_) => Kind::Text,
        Value::Number(number) => Kind::Number {
            minimum: -1.0e9,
            maximum: 1.0e9,
            integer: number.is_i64() || number.is_u64(),
        },
        Value::Bool(_) => Kind::Switch,
        _ => return None,
    };
    Some(Spec {
        key: key.to_owned(),
        kind,
        hint: String::new(),
    })
}

/// The row the tab shows for `spec`, holding `value`.
fn field(spec: &Spec, value: Option<&Value>) -> MotionField {
    let mut field = MotionField {
        key: spec.key.as_str().into(),
        label: label_of(&spec.key).into(),
        hint: spec.hint.as_str().into(),
        ..Default::default()
    };
    match &spec.kind {
        Kind::Text => {
            field.kind = TEXT;
            field.text = value.and_then(Value::as_str).unwrap_or_default().into();
        }
        Kind::Number {
            minimum,
            maximum,
            integer,
        } => {
            field.kind = NUMBER;
            field.number = value.and_then(Value::as_f64).unwrap_or_default() as f32;
            field.minimum = *minimum as f32;
            field.maximum = *maximum as f32;
            // A fraction of the frame moves by hundredths; a count by ones.
            let (step, precision) = if *integer {
                (1.0, 0)
            } else if *maximum <= 1.0 {
                (0.01, 2)
            } else {
                (0.1, 1)
            };
            field.step = step;
            field.precision = precision;
        }
        Kind::Switch => {
            field.kind = SWITCH;
            field.on = value.and_then(Value::as_bool).unwrap_or_default();
        }
        Kind::Choice(options) => {
            field.kind = CHOICE;
            let current = value.and_then(Value::as_str).unwrap_or_default();
            field.current = options
                .iter()
                .position(|option| option == current)
                .map_or(-1, |index| index as i32);
            let shown: Vec<SharedString> = options.iter().map(|option| option.into()).collect();
            field.options = ModelRc::new(VecModel::from(shown));
        }
    }
    field
}

/// Where the recipe was rendered, said, when it is not the timeline's frame.
fn frame_differs(props: &Map<String, Value>, width: u32, height: u32, fps: f64) -> Option<String> {
    let w = props.get("width").and_then(Value::as_u64)?;
    let h = props.get("height").and_then(Value::as_u64)?;
    let f = props.get("fps").and_then(Value::as_f64)?;
    let same = w == u64::from(width) && h == u64::from(height) && (f - fps).abs() < 0.01;
    (!same).then(|| tf("motion.frameAt", &[&format!("{w}×{h}"), &fps_text(f)]))
}

fn fps_text(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.01 {
        format!("{} fps", fps.round())
    } else {
        format!("{fps:.2} fps")
    }
}

/// A prop's name as the tab shows it.
fn label_of(key: &str) -> String {
    let known = match key {
        "text" => t("motion.fieldText"),
        "title" => t("motion.fieldTitle"),
        "subtitle" => t("motion.fieldSubtitle"),
        "accent" => t("motion.fieldAccent"),
        "size" => t("motion.fieldSize"),
        "x" => t("motion.fieldX"),
        "y" => t("motion.fieldY"),
        "align" => t("motion.fieldAlign"),
        "icon" => t("motion.fieldIcon"),
        "value" => t("motion.fieldValue"),
        "decimals" => t("motion.fieldDecimals"),
        "prefix" => t("motion.fieldPrefix"),
        "suffix" => t("motion.fieldSuffix"),
        "label" => t("motion.fieldLabel"),
        "cta" => t("motion.fieldCta"),
        "handle" => t("motion.fieldHandle"),
        "background" => t("motion.fieldBackground"),
        "showLogo" => t("motion.fieldShowLogo"),
        "scale" => t("motion.fieldScale"),
        "maxWords" => t("motion.fieldMaxWords"),
        "uppercase" => t("motion.fieldUppercase"),
        "edge" => t("motion.fieldEdge"),
        "thickness" => t("motion.fieldThickness"),
        "words" => t("motion.fieldWords"),
        "brand" => t("motion.fieldBrand"),
        "seconds" => t("motion.fieldSeconds"),
        "count" => t("motion.fieldCount"),
        "color" => t("motion.fieldColor"),
        _ => String::new(),
    };
    if !known.is_empty() {
        return known;
    }
    // camelCase or kebab-case, as a sentence.
    let mut out = String::new();
    for (index, c) in key.chars().enumerate() {
        if c == '-' || c == '_' {
            out.push(' ');
        } else if c.is_uppercase() && index > 0 {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else if index == 0 {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// A template's name as the tab's heading shows it.
fn label_of_template(id: &str) -> String {
    match id {
        "lower-third" => t("motion.templateLowerThird"),
        "hook-title" => t("motion.templateHookTitle"),
        "badge" => t("motion.templateBadge"),
        "stat" => t("motion.templateStat"),
        "end-card" => t("motion.templateEndCard"),
        "logo-sting" => t("motion.templateLogoSting"),
        "word-captions" => t("motion.templateWordCaptions"),
        "progress-bar" => t("motion.templateProgressBar"),
        other => label_of(other),
    }
}

// ----------------------------------------------------------------- worker

/// The worker: one request at a time to concat-studio's MCP server, which
/// it starts on the first and starts again if it dies.
fn work(jobs: mpsc::Receiver<Job>, logs: &Path) {
    let mut server: Option<Server> = None;
    for job in jobs {
        match job {
            // The catalogue is the machine's, not the project's: kept
            // whichever project is open when it lands. Dropped with the
            // project it was asked in, `asking` stayed set and the tab read
            // "reading templates" for the rest of the session.
            Job::Templates => {
                let answer = call(&mut server, logs, "overlay_templates", json!({}), |_| {})
                    .and_then(|text| {
                        serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())
                    });
                crate::host::on_ui(move |_, _, _| catalogued(answer));
            }
            Job::Render {
                epoch,
                media_id,
                template,
                props,
                output,
            } => {
                let arguments = json!({
                    "template": template,
                    "props": props,
                    "output": output,
                });
                let result = call(&mut server, logs, "overlay_render", arguments, |fraction| {
                    let _ = slint::invoke_from_event_loop(move || {
                        Shell::with(|_, app| app.global::<Motion>().set_progress(fraction));
                    });
                })
                .and_then(|text| probed(&text));
                // Always delivered, so the tab is not left busy; laid in
                // only in the project it was rendered for.
                crate::host::on_ui(move |studio, app, _| {
                    let result = if epoch == crate::host::project_epoch() {
                        Some(result)
                    } else {
                        None
                    };
                    finished(studio, app, media_id, result)
                });
            }
        }
    }
}

/// `overlay_render`'s answer as the bin takes it: the file probed, marked as
/// a motion graphic, with the recipe concat-studio rendered it from.
fn probed(answer: &str) -> Result<NewMedia, String> {
    let answer: Value = serde_json::from_str(answer).map_err(|error| error.to_string())?;
    let path = answer["path"].as_str().ok_or_else(|| t("motion.noPath"))?;
    let recipe: Recipe =
        serde_json::from_value(answer["motion"].clone()).map_err(|error| error.to_string())?;
    let mut item = concat_host::media::probe(path)?.to_new_media();
    item.origin = Some(MediaOrigin::Motion);
    item.motion = Some(recipe);
    Ok(item)
}

/// `tool` with `arguments`, its text back; the server started if it is not
/// running, and dropped if the call found it dead.
fn call(
    server: &mut Option<Server>,
    logs: &Path,
    tool: &str,
    arguments: Value,
    progress: impl Fn(f32),
) -> Result<String, String> {
    if server.is_none() {
        *server = Some(Server::start(logs)?);
    }
    let running = server.as_mut().expect("started");
    let result = running.call(tool, arguments, &progress);
    if result.is_err() && !running.alive() {
        *server = None;
    }
    result
}

/// concat-studio's MCP server, spoken to over its standard input and output.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Server {
    fn start(logs: &Path) -> Result<Server, String> {
        let path = crate::claude::studio_server().ok_or_else(|| t("motion.notFound"))?;
        let _ = std::fs::create_dir_all(logs);
        let stderr = std::fs::File::create(logs.join("concat-studio.log"))
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let mut child = Process::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .map_err(|error| tf("motion.startFailed", &[&error]))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(t("motion.noPipes"));
        };
        let mut server = Server {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next: 0,
        };
        server.request(
            "initialize",
            |_| {
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "concat-motion", "version": env!("CARGO_PKG_VERSION") },
                })
            },
            &|_| {},
        )?;
        server.write(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(server)
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn call(
        &mut self,
        tool: &str,
        arguments: Value,
        progress: &dyn Fn(f32),
    ) -> Result<String, String> {
        let result = self.request(
            "tools/call",
            |id| {
                json!({
                    "name": tool,
                    "arguments": arguments,
                    "_meta": { "progressToken": id },
                })
            },
            progress,
        )?;
        let text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if result["isError"].as_bool() == Some(true) {
            return Err(text.trim_start_matches("Error: ").to_owned());
        }
        Ok(text)
    }

    /// One request, its result back; progress notifications for it, as
    /// fractions, to `progress` on the way.
    fn request(
        &mut self,
        method: &str,
        params: impl FnOnce(u64) -> Value,
        progress: &dyn Fn(f32),
    ) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params(id) }))?;
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .stdout
                .read_line(&mut line)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                return Err(t("motion.exited"));
            }
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message["id"].as_u64() == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(error["message"].as_str().unwrap_or("error").to_owned());
                }
                return Ok(message["result"].clone());
            }
            if message["method"] == "notifications/progress"
                && message["params"]["progressToken"].as_u64() == Some(id)
            {
                let done = message["params"]["progress"].as_f64().unwrap_or_default();
                let total = message["params"]["total"]
                    .as_f64()
                    .unwrap_or(100.0)
                    .max(1.0);
                progress((done / total) as f32);
            }
        }
    }

    fn write(&mut self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|error| tf("motion.notResponding", &[&error]))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `overlay_templates` answers in, cut down to two entries.
    fn catalogue() -> Value {
        json!({
            "templates": [{
                "id": "badge",
                "description": "A small label with an icon",
                "props": { "type": "object", "properties": {
                    "text": { "type": "string", "description": "The text" },
                    "icon": { "type": "string" },
                    "x": { "type": "number", "minimum": 0, "maximum": 1 },
                    "align": { "type": "string", "enum": ["left", "center", "right"] },
                    "words": { "type": "array" },
                }},
                "order": ["text", "icon", "x", "align", "words"],
            }],
            "custom": [{
                "id": "golden-flash",
                "description": "A sweep of light",
                "defaults": { "width": 1080, "label": "Hello", "count": 14, "glow": true, "fps": 30 },
                "order": ["width", "label", "count", "glow", "fps"],
            }],
            "commonProps": { "properties": {
                "brand": { "type": "string", "enum": ["royalty", "sensei"] },
                "seconds": { "type": "number", "minimum": 0.3, "maximum": 300 },
            }},
        })
    }

    #[test]
    fn a_template_is_laid_out_in_its_order_then_brand_and_length() {
        let layout = layout(&catalogue(), "badge").expect("listed");
        let keys: Vec<&str> = layout.specs.iter().map(|spec| spec.key.as_str()).collect();
        assert_eq!(keys, ["text", "icon", "x", "align", "brand", "seconds"]);
        assert_eq!(layout.title, t("motion.templateBadge"));
        assert_eq!(
            layout.hidden,
            [t("motion.fieldWords")],
            "a list is said, not shown"
        );
        assert_eq!(
            layout.specs[2].kind,
            Kind::Number {
                minimum: 0.0,
                maximum: 1.0,
                integer: false
            }
        );
        assert_eq!(
            layout.specs[3].kind,
            Kind::Choice(vec!["left".into(), "center".into(), "right".into()])
        );
    }

    #[test]
    fn an_animation_of_its_own_is_laid_out_from_what_it_was_made_with() {
        let layout = layout(&catalogue(), "golden-flash").expect("listed");
        let keys: Vec<&str> = layout.specs.iter().map(|spec| spec.key.as_str()).collect();
        assert_eq!(
            keys,
            ["label", "count", "glow", "brand", "seconds"],
            "the frame is the timeline's"
        );
        assert_eq!(
            layout.title, "Golden flash",
            "an animation of its own reads as words"
        );
        assert_eq!(
            layout.specs[1].kind,
            Kind::Number {
                minimum: -1.0e9,
                maximum: 1.0e9,
                integer: true
            }
        );
        assert_eq!(layout.specs[2].kind, Kind::Switch);
    }

    #[test]
    fn an_unknown_template_has_no_layout() {
        assert!(layout(&catalogue(), "nope").is_none());
    }

    #[test]
    fn a_frame_that_is_not_the_timelines_is_said() {
        let mut props = Map::new();
        props.insert("width".into(), json!(1080));
        props.insert("height".into(), json!(1920));
        props.insert("fps".into(), json!(30));
        assert_eq!(frame_differs(&props, 1080, 1920, 30.0), None);
        assert_eq!(
            frame_differs(&props, 1920, 1080, 25.0).as_deref(),
            Some(tf("motion.frameAt", &[&"1080×1920", &"30 fps"]).as_str())
        );
    }

    #[test]
    fn a_prop_without_a_name_of_its_own_reads_as_words() {
        assert_eq!(label_of("glowColor"), "Glow color");
        assert_eq!(label_of("max-size"), "Max size");
        assert_eq!(label_of("cta"), t("motion.fieldCta"));
    }
}
