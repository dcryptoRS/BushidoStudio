// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

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

use serde_json::{Value, json};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::host::Shell;
use crate::panes::Msg;
use crate::panes::settings::SettingsMsg;
use crate::studio::Studio;
use crate::ui::{App, ChatEntry, Claude};

const YOU: i32 = 0;
const CLAUDE: i32 = 1;
const TOOL: i32 = 2;
const NOTE: i32 = 3;

/// What Claude is told about where it is, after its own system prompt.
const PROMPT: &str = "\
Estás en el panel «Claude» dentro de Concat, el editor de vídeo de Dany (Royalty Studio), \
mientras Dany edita. Responde en español, breve y directo: di en una o dos frases qué hiciste.\n\
Cada mensaje empieza con una línea [Concat] que dice qué proyecto está abierto en la ventana, \
dónde está el cabezal y qué hay seleccionado. Ese proyecto se edita EN VIVO con el MCP `concat` \
(modo ventana, live mode): cada herramienta que lo cambia se ve en pantalla al momento y queda \
en el historial de deshacer de Dany, una edición por llamada. Usa esa carpeta como `path`. No \
abras otros proyectos ni cierres este salvo que Dany lo pida, y no guardes (project_save) salvo \
que lo pida: la ventana guarda sola.\n\
Para overlays de marca, subtítulos palabra a palabra, LUTs y perfil de color usa el MCP \
`concat-studio` y sigue la skill concat-editor. Si ninguna plantilla sirve, escribe tú la animación \
en código Remotion con overlay_compose, como en un proyecto de Remotion (lee antes src/kit.tsx y \
src/templates/index.tsx), y luego renderízala con overlay_render y colócala como cualquier overlay. \
Antes de decir que algo quedó bien, míralo con preview_frame.";

/// The tools Claude may use from the pane: the two MCP servers, which can
/// only do what Concat and concat-studio do, and reading files. Nothing
/// that writes a file or runs a command is allowed here - in print mode a
/// tool not on this list is refused, not asked about - so new templates
/// and anything else of that kind are made from a Claude Code session in a
/// terminal, where each step is approved.
const ALLOWED: &[&str] = &["mcp__concat", "mcp__concat-studio", "Read", "Glob", "Grep"];

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
                format!("{name} · en vivo")
            } else {
                format!("{name} · se conectará al escribir")
            }
        }
        None => "Abre un proyecto para que Claude lo edite".to_owned(),
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
                "nada".to_owned()
            } else {
                studio.selection.join(", ")
            };
            format!(
                "[Concat] Proyecto abierto: {} · cabezal en {:.2} s · seleccionado: {selection}",
                session.path(),
                studio.playhead
            )
        }
        None => "[Concat] No hay ningún proyecto abierto en la ventana.".to_owned(),
    }
}

fn send(text: String) {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return;
    }
    Shell::with(|shell, app| {
        let (line, logs) = {
            let mut studio = shell.studio.borrow_mut();
            if studio.session.is_some() {
                ensure_server(&mut studio);
            }
            (context(&studio), studio.host.dirs.data.join("logs"))
        };
        shell.studio.borrow().publish(&app, &shell.models);
        CHAT.with(|chat| {
            let mut chat = chat.borrow_mut();
            push(&chat, YOU, text.clone());
            if chat.process.is_none() {
                chat.epoch += 1;
                match start(chat.session.as_deref(), chat.epoch, &logs) {
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
                chat.status = "Pensando…".to_owned();
            } else {
                chat.process = None;
                push(
                    &chat,
                    NOTE,
                    "No pude hablar con Claude Code; vuelve a enviarlo.",
                );
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
                push(
                    &chat,
                    NOTE,
                    "Detenido. Lo que ya hizo se queda; deshazlo con Ctrl+Z si quieres.",
                );
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
        .ok_or_else(|| {
            "No encuentro Claude Code (`claude`). Instálalo, o di dónde está con CONCAT_CLAUDE."
                .to_owned()
        })
}

/// concat-studio's MCP server, if it is installed.
fn studio_server() -> Option<PathBuf> {
    std::env::var_os("CONCAT_STUDIO_MCP")
        .map(PathBuf::from)
        .or_else(|| Some(home().join("dev/concat-studio/bin/concat-studio-mcp")))
        .filter(|path| path.is_file())
}

fn start(session: Option<&str>, epoch: u64, logs: &std::path::Path) -> Result<Running, String> {
    let claude = claude_binary()?;
    let me = std::env::current_exe()
        .map_err(|error| format!("Concat no se encuentra a sí mismo: {error}"))?;
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
    if let Some(studio) = studio_server() {
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
        "--append-system-prompt",
        PROMPT,
    ]);
    command.arg("--mcp-config").arg(&config);
    command.arg("--allowedTools").args(ALLOWED);
    if let Some(session) = session {
        command.arg("--resume").arg(session);
    }
    let workdir = home.join("dev/concat-studio");
    command.current_dir(if workdir.is_dir() { workdir } else { home });
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
    let mut child = command.spawn().map_err(|error| {
        format!(
            "No pude arrancar Claude Code ({}): {error}",
            claude.display()
        )
    })?;
    let stdin = child.stdin.take().ok_or("Claude Code sin entrada")?;
    let stdout = child.stdout.take().ok_or("Claude Code sin salida")?;
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
                    Some("thinking") => vec![Event::Status("Pensando…".to_owned())],
                    Some("tool_use") => {
                        let name = event["content_block"]["name"]
                            .as_str()
                            .unwrap_or("una herramienta");
                        vec![Event::Status(format!("Usando {}…", short(name)))]
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
                Event::Text(text) => {
                    match chat.streaming.take() {
                        Some(row) => chat.entries.set_row_data(
                            row,
                            ChatEntry {
                                role: CLAUDE,
                                text: text.into(),
                            },
                        ),
                        None => push(&chat, CLAUDE, text),
                    }
                }
                Event::Tool(line) => {
                    chat.streaming = None;
                    push(&chat, TOOL, line);
                }
                Event::ToolError(text) => push(&chat, NOTE, format!("Falló una herramienta: {text}")),
                Event::Done(error) => {
                    chat.busy = false;
                    chat.streaming = None;
                    chat.status.clear();
                    if let Some(error) = error {
                        push(&chat, NOTE, format!("No terminó bien: {error}"));
                    }
                }
                Event::Exited => {
                    chat.process = None;
                    if chat.busy {
                        chat.busy = false;
                        chat.status.clear();
                        push(
                            &chat,
                            NOTE,
                            "Claude Code se cerró. Mira logs/claude.log en la carpeta de datos de Concat.",
                        );
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
