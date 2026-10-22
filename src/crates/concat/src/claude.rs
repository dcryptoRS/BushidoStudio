// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! The Claude pane (Royalty Studio fork): a conversation with Claude Code,
//! run as a child process in stream-json mode for the life of the
//! conversation, which edits the project on screen.
//!
//! - Claude reaches the project through this window's server in live mode
//!   (`crate::live`): the MCP server it is given is this same binary as
//!   `--mcp window`, so its edits land on screen and in the window's undo.
//!   The first message turns the server and live mode on if they are off.
//! - It makes motion graphics, transcribes and grades through concat-studio
//!   (`~/dev/concat-studio`), when that is installed.
//! - Every message carries a line of context - the project, the playhead,
//!   the selection - so "this clip" and "here" mean something.
//!
//! The process's lines are read on a thread of their own and handed to the
//! event loop one event at a time; the conversation and its model live on
//! the event-loop thread, in a thread-local, like the rest of the window's
//! state. Stopping kills the process and the next message resumes the same
//! session; starting over forgets it.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::rc::Rc;
use std::sync::Mutex;

use serde_json::{Value, json};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::host::Shell;
use crate::i18n::{t, tf};
use crate::panes::Msg;
use crate::panes::settings::SettingsMsg;
use crate::prefs::AssistantPrefs;
use crate::studio::Studio;
use crate::ui::{App, ChatEntry, Claude};

const YOU: i32 = 0;
const CLAUDE: i32 = 1;
const TOOL: i32 = 2;
const NOTE: i32 = 3;

/// What Claude is told about where it is and whom it works for, after its
/// own system prompt: the person's name, where their material is and what
/// is out of bounds, from Settings > Assistant and AI. English, since it is
/// read by the model; it answers in the language it is written to.
fn prompt(assistant: &AssistantPrefs, studio: bool) -> String {
    let name = assistant.name.trim();
    let who = if name.is_empty() {
        "the person editing"
    } else {
        name
    };
    let mut text = format!(
        "You are in the Claude panel inside the video editor, working with {who} while \
they edit. Answer in the language they write in, short and direct: say in a sentence or two \
what you did.\n\
Every message starts with a [Concat] line saying which project is open in the window, where \
the playhead is and what is selected. That project is edited LIVE through the `concat` MCP \
server (window mode): every tool that changes it shows on screen at once and lands in \
{who}'s undo history, one edit per call. Use that folder as `path`. Do not open other \
projects or close this one unless asked, and do not save (project_save) unless asked: the \
window saves by itself.\n\
Before saying something looks right, look at it with preview_frame.\n"
    );
    if studio {
        text.push_str(
            "For brand overlays, word-by-word captions, LUTs and color profiles use the \
`concat-studio` MCP server and follow the concat-editor skill. When no template fits, write the \
animation yourself in Remotion code with overlay_compose (read src/kit.tsx and \
src/templates/index.tsx first), render it with overlay_render and place it like any overlay. \
When importing an overlay, pass media_import the `motion` overlay_render returned, so it can be \
retouched in the Motion tab; to change one already placed, render it again and use \
media_replace_motion with its mediaId instead of importing it again.\n",
        );
    }
    let footage: Vec<&str> = assistant.footage.iter().map(String::as_str).collect();
    text.push_str(&format!(
        "You have every permission on this computer: commands (Bash), reading and writing \
files, the home folder and the mounted drives.{} Use them with care: the project is edited \
through the `concat` MCP server, never by editing its concat.json by hand (the window would not \
see it), and animations with overlay_compose, which builds and checks them, not by writing the \
.tsx yourself. A guard blocks what cannot be undone (sudo, deleting whole folders or more than \
300 files / 3 GB at once, Docker clean-ups, discarding git work, shutting the machine down",
        if footage.is_empty() {
            String::new()
        } else {
            format!(" Their material is in: {}.", footage.join(", "))
        }
    ));
    if !assistant.protected.is_empty() {
        text.push_str(&format!(", anything of {}", assistant.protected.join(", ")));
    }
    text.push_str(&format!(
        "): if something like that is needed, explain it to {who} instead of working around it."
    ));
    text
}

/// The Claude pane runs with every permission (the fork's choice,
/// 2026-10-20): commands, files, the home folder and the drives, without
/// asking. The one limit is this guard, a PreToolUse hook that refuses what
/// cannot be taken back - sudo, deleting whole folders or more than 300
/// files / 3 GB at once, Docker clean-ups, discarding git work, shutting the
/// machine down, and anything of what Settings protects. Written next to
/// the logs on each start, with that list beside it, so the binary carries
/// it.
const GUARD: &str = include_str!("claude-guard.py");

/// How a professional editor and colorist works - cutting, continuity, shot
/// language, color, sound, short-form - as the assistant is told it with
/// its system prompt (Royalty Studio fork). Written for the program, so it
/// ships with it, whoever runs it.
const CRAFT: &str = include_str!("craft.md");

struct Running {
    child: Child,
    stdin: ChildStdin,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Chat {
    entries: Rc<VecModel<ChatEntry>>,
    process: Option<Running>,
    /// Claude Code's session, to resume after a stop.
    session: Option<String>,
    /// Bumped whenever a process is replaced, so a dead one's late lines
    /// are dropped rather than written into the conversation.
    epoch: u64,
    /// The row Claude's answer is streaming into.
    streaming: Option<usize>,
    busy: bool,
    status: String,
}

thread_local! {
    static CHAT: RefCell<Chat> = RefCell::new(Chat::default());
}

/// One thing the process said, as far as the pane cares.
enum Event {
    Session(String),
    Status(String),
    Delta(String),
    Text(String),
    Tool(String),
    ToolError(String),
    Done(Option<String>),
    Exited,
}

/// Hands the conversation's model to the pane and wires its callbacks.
pub fn install(app: &App) {
    let global = app.global::<Claude>();
    CHAT.with(|chat| global.set_entries(ModelRc::from(chat.borrow().entries.clone())));
    global.on_send(|text| send(text.to_string()));
    global.on_stop(stop);
    global.on_reset(reset);
    Shell::with(|shell, app| refresh(&shell.studio.borrow(), &app));
}

fn push(chat: &Chat, role: i32, text: impl Into<SharedString>) {
    chat.entries.push(ChatEntry {
        role,
        text: text.into(),
    });
}

/// The pane's note, for the window's publish: the project it would edit
/// changes when one is opened or closed, not only when Claude speaks.
pub fn publish(studio: &Studio, app: &App) {
    refresh(studio, app);
}

/// The pane's status line and flags, from the conversation and the window.
fn refresh(studio: &Studio, app: &App) {
    let global = app.global::<Claude>();
    let note = match studio.session.as_ref() {
        Some(session) => {
            let name = std::path::Path::new(session.path())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if studio.prefs.server.enabled && studio.prefs.server.live {
                tf("claude.noteLive", &[&name])
            } else {
                tf("claude.noteOnSend", &[&name])
            }
        }
        None => t("claude.openAProject"),
    };
    global.set_note(note.into());
    CHAT.with(|chat| {
        let chat = chat.borrow();
        global.set_busy(chat.busy);
        global.set_status(chat.status.clone().into());
    });
}

/// The server on, with a token, in live mode: what `--mcp window` needs.
fn ensure_server(studio: &mut Studio) {
    if concat_server::check_token(&studio.prefs.server.token).is_err() {
        studio.handle(Msg::Settings(SettingsMsg::ServerTokenGenerated));
    }
    if !studio.prefs.server.live {
        studio.handle(Msg::Settings(SettingsMsg::ServerLiveChanged(true)));
    }
    if !studio.prefs.server.enabled || studio.host.server.is_none() {
        studio.handle(Msg::Settings(SettingsMsg::ServerEnabledChanged(true)));
    }
}

fn context(studio: &Studio) -> String {
    match studio.session.as_ref() {
        Some(session) => {
            let selection = if studio.selection.is_empty() {
                "nothing".to_owned()
            } else {
                studio.selection.join(", ")
            };
            format!(
                "[Concat] Open project: {} · playhead at {:.2} s · selected: {selection}",
                session.path(),
                studio.playhead
            )
        }
        None => "[Concat] No project is open in the window.".to_owned(),
    }
}

fn send(text: String) {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return;
    }
    Shell::with(|shell, app| {
        let (line, logs, assistant) = {
            let mut studio = shell.studio.borrow_mut();
            if studio.session.is_some() {
                ensure_server(&mut studio);
            }
            (
                context(&studio),
                studio.host.dirs.data.join("logs"),
                studio.prefs.assistant.clone(),
            )
        };
        shell.studio.borrow().publish(&app, &shell.models);
        CHAT.with(|chat| {
            let mut chat = chat.borrow_mut();
            push(&chat, YOU, text.clone());
            if chat.process.is_none() {
                chat.epoch += 1;
                match start(chat.session.as_deref(), chat.epoch, &logs, &assistant) {
                    Ok(running) => chat.process = Some(running),
                    Err(error) => {
                        push(&chat, NOTE, error);
                        return;
                    }
                }
            }
            let message = json!({
                "type": "user",
                "message": { "role": "user", "content": format!("{line}\n\n{text}") },
            });
            let written = chat.process.as_mut().is_some_and(|running| {
                writeln!(running.stdin, "{message}").is_ok() && running.stdin.flush().is_ok()
            });
            if written {
                chat.busy = true;
                chat.streaming = None;
                chat.status = t("claude.thinking");
            } else {
                chat.process = None;
                push(&chat, NOTE, t("claude.couldNotSend"));
            }
        });
        refresh(&shell.studio.borrow(), &app);
    });
}

fn stop() {
    CHAT.with(|chat| {
        let mut chat = chat.borrow_mut();
        if chat.process.take().is_some() {
            chat.epoch += 1;
            if chat.busy {
                push(&chat, NOTE, t("claude.stopped"));
            }
        }
        chat.busy = false;
        chat.streaming = None;
        chat.status.clear();
    });
    Shell::with(|shell, app| refresh(&shell.studio.borrow(), &app));
}

fn reset() {
    CHAT.with(|chat| {
        let mut chat = chat.borrow_mut();
        chat.process = None;
        chat.epoch += 1;
        chat.session = None;
        chat.busy = false;
        chat.streaming = None;
        chat.status.clear();
        chat.entries.set_vec(Vec::new());
    });
    Shell::with(|shell, app| refresh(&shell.studio.borrow(), &app));
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Claude Code: `CONCAT_CLAUDE`, or `claude` on the PATH or where its
/// installer puts it. A window started from the desktop menu may not have
/// `~/.local/bin` on its PATH, hence the list.
fn claude_binary() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("CONCAT_CLAUDE") {
        return Ok(PathBuf::from(path));
    }
    let home = home();
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("claude"))
                .collect()
        })
        .unwrap_or_default();
    candidates.extend([
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
        PathBuf::from("/usr/local/bin/claude"),
        PathBuf::from("/usr/bin/claude"),
    ]);
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| t("claude.notFound"))
}

/// Where Settings > Assistant and AI says concat-studio's server is, when
/// it says; set as the preferences are read and as the field is edited.
static STUDIO: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Remembers where concat-studio's server is, from the preferences.
pub(crate) fn set_studio_server(path: Option<&str>) {
    let path = path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(expand);
    if let Ok(mut studio) = STUDIO.lock() {
        *studio = path;
    }
}

/// `~/...` read as the home folder's.
pub(crate) fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}

/// concat-studio's MCP server, if it is installed: `CONCAT_STUDIO_MCP`,
/// the one Settings names, or where its installer puts it.
pub(crate) fn studio_server() -> Option<PathBuf> {
    std::env::var_os("CONCAT_STUDIO_MCP")
        .map(PathBuf::from)
        .or_else(|| STUDIO.lock().ok().and_then(|studio| studio.clone()))
        .or_else(|| Some(home().join("dev/concat-studio/bin/concat-studio-mcp")))
        .filter(|path| path.is_file())
}

fn start(
    session: Option<&str>,
    epoch: u64,
    logs: &std::path::Path,
    assistant: &AssistantPrefs,
) -> Result<Running, String> {
    let claude = claude_binary()?;
    let me = std::env::current_exe().map_err(|error| tf("claude.noSelf", &[&error]))?;
    let home = home();
    // Where the agent may write: home, and the drives mounted for this user.
    let mut roots = vec![home.clone()];
    if let Some(user) = home.file_name() {
        roots.push(PathBuf::from("/run/media").join(user));
    }
    let roots = std::env::join_paths(&roots)
        .map(|joined| joined.to_string_lossy().into_owned())
        .unwrap_or_else(|_| home.to_string_lossy().into_owned());
    let mut servers = json!({
        "concat": {
            "command": me,
            "args": ["--mcp", "window"],
            "env": { "CONCAT_MCP_ROOTS": roots },
        },
    });
    let studio = studio_server();
    if let Some(studio) = &studio {
        servers["concat-studio"] = json!({ "command": studio, "args": [] });
    }
    let config = json!({ "mcpServers": servers }).to_string();

    let mut command = Command::new(&claude);
    command.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--strict-mcp-config",
    ]);
    // Where it is and what it may do, then how a professional cuts and grades.
    command.arg("--append-system-prompt").arg(format!(
        "{}\n\n{CRAFT}",
        prompt(assistant, studio.is_some())
    ));
    command.arg("--mcp-config").arg(&config);
    command.args(["--permission-mode", "bypassPermissions"]);
    // Read, Glob and Grep keep to the working directory and these; without
    // them Claude could not see the footage and the music it is asked for.
    command.arg("--add-dir").arg(&home);
    if let Some(user) = home.file_name() {
        command.arg(PathBuf::from("/run/media").join(user));
    }
    for folder in &assistant.footage {
        let folder = expand(folder);
        if folder.is_dir() && !folder.starts_with(&home) {
            command.arg(folder);
        }
    }
    // The guard, and beside it what this person wants kept out of reach.
    let guard = logs.parent().unwrap_or(logs).join("claude-guard.py");
    std::fs::write(&guard, GUARD).map_err(|error| tf("claude.noGuard", &[&error]))?;
    let fences = json!({
        "protected": assistant.protected,
        "keep": [home.join("dev/Concat"), home.join("dev/concat-studio")],
    });
    std::fs::write(guard.with_extension("json"), fences.to_string())
        .map_err(|error| tf("claude.noGuard", &[&error]))?;
    let hooks = json!({
        "hooks": { "PreToolUse": [{
            "matcher": "Bash|Write|Edit|MultiEdit|NotebookEdit",
            "hooks": [{ "type": "command", "command": format!("python3 '{}'", guard.display()) }],
        }]},
    });
    command.arg("--settings").arg(hooks.to_string());
    if let Some(session) = session {
        command.arg("--resume").arg(session);
    }
    // concat-studio's checkout, where its skill and kit are read from:
    // the server lives in its `bin`.
    let workdir = studio
        .as_deref()
        .and_then(|server| server.parent()?.parent())
        .filter(|dir| dir.join("src").is_dir())
        .map(PathBuf::from);
    command.current_dir(workdir.unwrap_or(home));
    let _ = std::fs::create_dir_all(logs);
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("claude.log"))
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr);
    let mut child = command
        .spawn()
        .map_err(|error| tf("claude.couldNotStart", &[&claude.display(), &error]))?;
    let stdin = child.stdin.take().ok_or_else(|| t("claude.noPipes"))?;
    let stdout = child.stdout.take().ok_or_else(|| t("claude.noPipes"))?;
    log::info!("claude: started {} (session {session:?})", claude.display());
    std::thread::Builder::new()
        .name("claude".to_owned())
        .spawn(move || read(stdout, epoch))
        .map_err(|error| error.to_string())?;
    Ok(Running { child, stdin })
}

/// The process's lines, as events for the event loop.
fn read(stdout: ChildStdout, epoch: u64) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let events = interpret(&value);
        if !events.is_empty() {
            let _ = slint::invoke_from_event_loop(move || deliver(epoch, events));
        }
    }
    let _ = slint::invoke_from_event_loop(move || deliver(epoch, vec![Event::Exited]));
}

/// "mcp__concat-studio__overlay_render" → "overlay_render".
fn short(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

/// A tool call as one line: its name and the arguments that say the most.
fn tool_line(name: &str, input: &Value) -> String {
    let mut line = format!("▸ {}", short(name));
    for key in [
        "template", "text", "title", "cta", "time", "start", "file", "input", "effectId", "op",
    ] {
        if let Some(value) = input.get(key) {
            let shown = match value {
                Value::String(text) => {
                    let text = std::path::Path::new(text)
                        .file_name()
                        .filter(|_| key == "file" || key == "input")
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| text.clone());
                    if text.chars().count() > 40 {
                        format!("{}…", text.chars().take(40).collect::<String>())
                    } else {
                        text
                    }
                }
                other => other.to_string(),
            };
            line.push_str(&format!(" · {shown}"));
            break;
        }
    }
    line
}

fn interpret(value: &Value) -> Vec<Event> {
    let kind = value["type"].as_str().unwrap_or_default();
    match kind {
        "system" if value["subtype"] == "init" => value["session_id"]
            .as_str()
            .map(|id| vec![Event::Session(id.to_owned())])
            .unwrap_or_default(),
        "stream_event" => {
            let event = &value["event"];
            match event["type"].as_str().unwrap_or_default() {
                "content_block_start" => match event["content_block"]["type"].as_str() {
                    Some("thinking") => vec![Event::Status(t("claude.thinking"))],
                    Some("tool_use") => {
                        let name = event["content_block"]["name"]
                            .as_str()
                            .map(|name| short(name).to_owned())
                            .unwrap_or_else(|| t("claude.aTool"));
                        vec![Event::Status(tf("claude.using", &[&name]))]
                    }
                    _ => Vec::new(),
                },
                "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                    event["delta"]["text"]
                        .as_str()
                        .map(|text| vec![Event::Delta(text.to_owned())])
                        .unwrap_or_default()
                }
                _ => Vec::new(),
            }
        }
        "assistant" => value["message"]["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| match block["type"].as_str() {
                        Some("text") => block["text"]
                            .as_str()
                            .filter(|text| !text.trim().is_empty())
                            .map(|text| Event::Text(text.to_owned())),
                        Some("tool_use") => Some(Event::Tool(tool_line(
                            block["name"].as_str().unwrap_or_default(),
                            &block["input"],
                        ))),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "user" => value["message"]["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| block["type"] == "tool_result" && block["is_error"] == true)
                    .map(|block| {
                        let text = match &block["content"] {
                            Value::String(text) => text.clone(),
                            Value::Array(parts) => parts
                                .iter()
                                .filter_map(|part| part["text"].as_str())
                                .collect::<Vec<_>>()
                                .join(" "),
                            _ => String::new(),
                        };
                        Event::ToolError(text.chars().take(300).collect())
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "result" => {
            let failed = value["is_error"] == true || value["subtype"] != "success";
            vec![Event::Done(failed.then(|| {
                value["result"]
                    .as_str()
                    .or_else(|| value["subtype"].as_str())
                    .unwrap_or("error")
                    .to_owned()
            }))]
        }
        _ => Vec::new(),
    }
}

fn deliver(epoch: u64, events: Vec<Event>) {
    let current = CHAT.with(|chat| {
        let mut chat = chat.borrow_mut();
        if chat.epoch != epoch {
            return false;
        }
        for event in events {
            match event {
                Event::Session(id) => chat.session = Some(id),
                Event::Status(status) => chat.status = status,
                Event::Delta(text) => match chat.streaming {
                    Some(row) => {
                        if let Some(mut entry) = chat.entries.row_data(row) {
                            entry.text = format!("{}{text}", entry.text).into();
                            chat.entries.set_row_data(row, entry);
                        }
                    }
                    None => {
                        push(&chat, CLAUDE, text);
                        chat.streaming = Some(chat.entries.row_count() - 1);
                        chat.status.clear();
                    }
                },
                Event::Text(text) => match chat.streaming.take() {
                    Some(row) => chat.entries.set_row_data(
                        row,
                        ChatEntry {
                            role: CLAUDE,
                            text: text.into(),
                        },
                    ),
                    None => push(&chat, CLAUDE, text),
                },
                Event::Tool(line) => {
                    chat.streaming = None;
                    push(&chat, TOOL, line);
                }
                Event::ToolError(text) => push(&chat, NOTE, tf("claude.toolFailed", &[&text])),
                Event::Done(error) => {
                    chat.busy = false;
                    chat.streaming = None;
                    chat.status.clear();
                    if let Some(error) = error {
                        push(&chat, NOTE, tf("claude.endedBadly", &[&error]));
                    }
                }
                Event::Exited => {
                    chat.process = None;
                    if chat.busy {
                        chat.busy = false;
                        chat.status.clear();
                        push(&chat, NOTE, t("claude.exited"));
                    }
                }
            }
        }
        true
    });
    if current {
        Shell::with(|shell, app| refresh(&shell.studio.borrow(), &app));
    }
}
