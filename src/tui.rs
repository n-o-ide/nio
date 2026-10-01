use super::*;
use crossterm::cursor::{Hide, Show};
use crossterm::style::SetBackgroundColor;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use std::cell::RefCell;

static ACTIVE: AtomicBool = AtomicBool::new(false);
pub fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

thread_local! {
    static OUTPUT: RefCell<Option<mpsc::Sender<Value>>> = const { RefCell::new(None) };
    static APPROVAL: RefCell<Option<mpsc::Receiver<bool>>> = const { RefCell::new(None) };
    static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}
pub fn configure_worker(
    sender: mpsc::Sender<Value>,
    approval: mpsc::Receiver<bool>,
    cancel: Arc<AtomicBool>,
) {
    OUTPUT.with(|value| *value.borrow_mut() = Some(sender));
    APPROVAL.with(|value| *value.borrow_mut() = Some(approval));
    CANCEL.with(|value| *value.borrow_mut() = Some(cancel));
}
pub fn send_event(value: &Value) -> bool {
    OUTPUT.with(|output| {
        if let Some(sender) = output.borrow().as_ref() {
            let _ = sender.send(value.clone());
            true
        } else {
            false
        }
    })
}
pub fn cancelled() -> Option<Arc<AtomicBool>> {
    CANCEL.with(|value| value.borrow().clone())
}
pub fn approve(action: &str, preview: Option<(&str, &str)>) -> Option<Result<bool, String>> {
    APPROVAL.with(|receiver| receiver.borrow().as_ref().map(|receiver| {
        send_event(&json!({"type":"approval", "action": action, "preview":preview.map(|(_, details)| details)}));
        receiver.recv().map_err(|_| "approval input closed".to_string())
    }))
}

// The TUI owns wrapping once. Markdown supplies styling and block formatting;
// this wrapper keeps prose words and list continuation indentation together.
fn chat_rows(entry: &Entry, width: usize) -> Vec<String> {
    let prefix = match entry.role.as_str() {
        "Nio" => "🤖 nio: ".to_string(),
        "You" => "🤖 nio> ".to_string(),
        "Progress" => "🔹 ".to_string(),
        _ => format!("{}: ", entry.role),
    };
    let indent = " ".repeat(terminal_text_width(&prefix));
    let content_width = width
        .saturating_sub(1 + terminal_text_width(&prefix))
        .max(1);
    let text = if entry.role == "Nio" {
        let mut formatter = MarkdownFormatter::new(true);
        formatter.wrap_width = content_width + RESPONSE_INDENT_WIDTH;
        formatter.wrap_prose = false;
        let mut text = formatter.push(&entry.text);
        text.push_str(&formatter.finish());
        text
    } else {
        entry.text.clone()
    };
    let mut output = Vec::new();
    let mut style = String::new();
    for logical in text.replace("\r\n", "\n").replace('\t', "    ").split('\n') {
        let visible = strip_terminal_ansi(logical);
        if visible.trim().is_empty() {
            output.push(String::new());
            continue;
        }
        let leading = visible.len() - visible.trim_start().len();
        let leading_width = terminal_text_width(&visible[..leading]);
        let rest = visible.trim_start();
        let list_width =
            if rest.starts_with("• ") || rest.starts_with("- ") || rest.starts_with("* ") {
                2
            } else {
                let digits = rest.chars().take_while(char::is_ascii_digit).count();
                if digits > 0
                    && rest
                        .get(digits..)
                        .is_some_and(|suffix| suffix.starts_with(". ") || suffix.starts_with(") "))
                {
                    digits + 2
                } else {
                    0
                }
            };
        let hanging = (leading_width + list_width).min(content_width.saturating_sub(1));
        let mut line = String::new();
        let mut cells = 0;
        for word in logical.split_inclusive(' ') {
            let word_cells = terminal_text_width(strip_terminal_ansi(word).trim_end());
            if cells > hanging && cells + word_cells > content_width {
                output.push(line.trim_end().to_string());
                line = format!("{}{style}", " ".repeat(hanging));
                cells = hanging;
            }
            let mut chars = word.chars();
            while let Some(character) = chars.next() {
                if character == '\x1b' {
                    if chars.next() == Some('[') {
                        let mut sequence = String::from("\x1b[");
                        for character in chars.by_ref() {
                            sequence.push(character);
                            if ('@'..='~').contains(&character) {
                                break;
                            }
                        }
                        if sequence.ends_with('m') {
                            if sequence == "\x1b[m"
                                || sequence[2..sequence.len() - 1]
                                    .split(';')
                                    .any(|value| value == "0")
                            {
                                style.clear();
                            }
                            style.push_str(&sequence);
                            line.push_str(&sequence);
                        }
                    }
                    continue;
                }
                if character.is_control() {
                    continue;
                }
                let character_width = terminal_char_width(character);
                if cells + character_width > content_width {
                    if character.is_whitespace() {
                        continue;
                    }
                    output.push(line.trim_end().to_string());
                    line = format!("{}{style}", " ".repeat(hanging));
                    cells = hanging;
                }
                line.push(character);
                cells += character_width;
            }
        }
        output.push(line.trim_end().to_string());
    }
    let leading_empty = output.iter().take_while(|line| line.is_empty()).count();
    output.drain(..leading_empty);
    while output.last().is_some_and(String::is_empty) {
        output.pop();
    }
    output
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("{prefix}{line}")
            } else if line.is_empty() {
                line
            } else {
                format!("{indent}{line}")
            }
        })
        .collect()
}

fn selected_rows(rows: &[String], selected: usize, accent: u8) -> Vec<String> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            if index == selected {
                format!("\x1b[1;38;5;{accent}m› ✓ {row}\x1b[0m")
            } else {
                format!("    {row}")
            }
        })
        .collect()
}

struct ScreenGuard;
impl ScreenGuard {
    fn enter() -> Result<Self, String> {
        terminal::enable_raw_mode().map_err(|e| e.to_string())?;
        if let Err(error) = execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            Hide
        ) {
            let _ = terminal::disable_raw_mode();
            return Err(error.to_string());
        }
        ACTIVE.store(true, Ordering::SeqCst);
        Ok(Self)
    }
}
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::SeqCst);
        let _ = execute!(
            io::stdout(),
            ResetColor,
            DisableMouseCapture,
            DisableBracketedPaste,
            Show,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}
struct Entry {
    role: String,
    text: String,
}
struct ModelSearch {
    query: String,
    items: Vec<(String, String)>,
}
struct Panel {
    title: String,
    rows: Vec<String>,
    selected: usize,
    sessions: Option<Vec<String>>,
    commands: Option<Vec<String>>,
    search: Option<ModelSearch>,
}
impl Panel {
    fn filter_models(&mut self) {
        let Some(search) = &self.search else {
            return;
        };
        let query = search.query.to_lowercase();
        let words = query.split_whitespace().collect::<Vec<_>>();
        let matches = search
            .items
            .iter()
            .filter(|(label, _)| {
                let label = label.to_lowercase();
                words.iter().all(|word| label.contains(word))
            })
            .collect::<Vec<_>>();
        self.rows = matches.iter().map(|(label, _)| label.clone()).collect();
        self.commands = Some(matches.iter().map(|(_, command)| command.clone()).collect());
        self.selected = 0;
    }
}
struct State {
    entries: Vec<Entry>,
    input: String,
    cursor: usize,
    pastes: PastedBlocks,
    queue_draft: Option<(String, usize, PastedBlocks)>,
    scroll: usize,
    busy: bool,
    notice: String,
    panel: Option<Panel>,
    approval: Option<String>,
    approval_preview: Option<String>,
    approval_sender: Option<mpsc::Sender<bool>>,
    cancel: Option<Arc<AtomicBool>>,
    worker: Option<JoinHandle<()>>,
    receiver: Option<mpsc::Receiver<Value>>,
    jobs: Option<mpsc::Receiver<Value>>,
    answer_index: Option<usize>,
    last_diff: Option<String>,
    turn_started: Option<Instant>,
    progress_active: bool,
    history: Vec<Value>,
    session: String,
    model: String,
    root: PathBuf,
    config: UserConfig,
    composer_row: u16,
    composer_positions: Vec<(u16, u16)>,
    palette_selected: usize,
    frame_rows: Vec<String>,
    frame_size: (usize, usize),
    frame_background: Option<Color>,
    frame_cursor: Option<(u16, u16)>,
}
impl State {
    fn add(&mut self, role: &str, text: impl Into<String>) {
        self.entries.push(Entry {
            role: role.into(),
            text: text.into(),
        });
    }
    fn restore(&mut self) {
        self.entries.clear();
        for message in self.history.clone() {
            match message["role"].as_str() {
                Some("user") => {
                    if let Some(text) = message["content"].as_str() {
                        self.add("You", text);
                    }
                }
                Some("assistant") => {
                    if let Some(text) = message["content"]
                        .as_str()
                        .filter(|text| !text.trim().is_empty())
                    {
                        self.add("Nio", text);
                    }
                }
                Some("tool") => {
                    if let Some(text) = message["content"].as_str().filter(|text| {
                        text.starts_with("Edited ") || text.starts_with("Tool error:")
                    }) {
                        if text.starts_with("Edited ") {
                            self.last_diff = Some(text.to_string());
                        }
                        self.add("Tool", text.lines().next().unwrap_or_default());
                    }
                }
                _ => {}
            }
        }
        self.scroll = 0;
    }
    fn start(&mut self, options: &Options, prompt: String) -> Result<(), String> {
        if prompt.len() > 24 * 1024 {
            return Err("prompt exceeds the 24 KiB limit".into());
        }
        self.add("You", &prompt);
        self.scroll = 0;
        self.answer_index = None;
        let mut options = options.clone();
        options.command = "tui-worker".into();
        options.json_output = true;
        options.auto_approve |= self.config.auto_approve_actions.unwrap_or(false);
        let history = self.history.clone();
        options.mode = self.config.agent_mode.clone();
        options.reasoning = self.config.reasoning_effort.clone();
        let model = self.model.clone();
        let session = self.session.clone();
        let root = self.root.clone();
        let (sender, receiver) = mpsc::channel();
        let (approval_sender, approval_receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.approval_sender = Some(approval_sender);
        self.receiver = Some(receiver);
        self.busy = true;
        self.turn_started = Some(Instant::now());
        self.progress_active = true;
        self.notice = "Working · type a message and Enter to queue".into();
        self.worker = Some(thread::spawn(move || {
            OUTPUT.with(|output| *output.borrow_mut() = Some(sender.clone()));
            APPROVAL.with(|value| *value.borrow_mut() = Some(approval_receiver));
            CANCEL.with(|value| *value.borrow_mut() = Some(cancel));
            let mut history = history;
            let result = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime
                    .block_on(run_agent_turn(&options, &model, &prompt, &mut history))
                    .map(|_| ()),
                Err(error) => Err(error.to_string()),
            };
            let saved =
                save_session_history(Some(&session), &root, &history, options.project_trusted);
            let error = result.err().or_else(|| saved.err());
            let _ = sender.send(json!({"type":"complete", "history":history, "error":error}));
        }));
        Ok(())
    }
    fn events(&mut self) -> bool {
        let mut events = self
            .receiver
            .as_ref()
            .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        if let Some(jobs) = &self.jobs {
            events.extend(jobs.try_iter());
        }
        let changed = !events.is_empty();
        for event in events {
            match event["type"].as_str() {
                Some("skills_result") => {
                    self.jobs = None;
                    self.add(
                        "Skills",
                        event["text"].as_str().unwrap_or("Skill operation finished"),
                    );
                    self.config = load_user_config().unwrap_or_default();
                }
                Some("models_result") => {
                    self.jobs = None;
                    if let Some(items) = event["items"].as_array() {
                        self.choices(
                            "Models",
                            items
                                .iter()
                                .filter_map(|item| {
                                    Some((
                                        format!(
                                            "{} · {}",
                                            item["id"].as_str()?,
                                            item["label"].as_str()?
                                        ),
                                        format!(":model {}", item["id"].as_str()?),
                                    ))
                                })
                                .collect(),
                        );
                    } else {
                        self.notice = event["error"]
                            .as_str()
                            .unwrap_or("Model catalog unavailable")
                            .into();
                    }
                }
                Some("text") => {
                    let text = event["part"]["text"].as_str().unwrap_or_default();
                    let index = if let Some(index) = self.answer_index {
                        index
                    } else {
                        self.add("Nio", "");
                        let index = self.entries.len() - 1;
                        self.answer_index = Some(index);
                        index
                    };
                    self.entries[index].text.push_str(text);
                }
                Some("reasoning") => {
                    let text = event["part"]["text"]
                        .as_str()
                        .unwrap_or("Working…")
                        .to_string();
                    self.notice = text.clone();
                    self.progress_active = true;
                    if !self
                        .entries
                        .last()
                        .is_some_and(|entry| entry.role == "Progress" && entry.text == text)
                    {
                        self.add("Progress", text);
                    }
                }
                Some("status") => {
                    let message = event["message"].as_str().unwrap_or("Working…").to_string();
                    self.notice = message.clone();
                    self.progress_active = true;
                    if !self
                        .entries
                        .last()
                        .is_some_and(|entry| entry.role == "Progress" && entry.text == message)
                    {
                        self.add("Progress", message);
                    }
                }
                Some("tool_use") => {
                    let part = &event["part"];
                    let status = part["state"]["status"].as_str().unwrap_or_default();
                    let title = part["state"]["title"].as_str().unwrap_or("tool");
                    if status == "running" {
                        self.progress_active = true;
                        self.add("Progress", format!("{title} …"));
                    } else if matches!(status, "completed" | "error") {
                        self.progress_active = false;
                        let duration = part["state"]["duration"].as_f64().unwrap_or(0.0);
                        if let Some(entry) = self.entries.iter_mut().rev().find(|entry| {
                            entry.role == "Progress" && entry.text == format!("{title} …")
                        }) {
                            entry.text = format!(
                                "{} {title} ({duration:.1}s)",
                                if status == "error" { "✖" } else { "✔" }
                            );
                        }
                        let output = part["state"]["output"].as_str().unwrap_or_default();
                        if output.starts_with("Edited ") && output.contains("diff --git") {
                            self.last_diff = Some(output.to_string());
                            self.add("Edit", strip_terminal_ansi(&compact_edit_view(output, 120)));
                        } else {
                            self.add(
                                "Tool",
                                if status == "error" {
                                    format!(
                                        "✖ {title}: {}",
                                        output.lines().next().unwrap_or_default()
                                    )
                                } else if output.starts_with("No changes")
                                    || output.starts_with("Updated ")
                                    || output.starts_with("Created empty")
                                {
                                    output.to_string()
                                } else {
                                    format!("✓ {title}")
                                },
                            );
                        }
                        self.answer_index = None;
                    }
                }
                Some("approval") => {
                    self.progress_active = false;
                    self.approval = Some(
                        event["action"]
                            .as_str()
                            .unwrap_or("Approve action?")
                            .to_string(),
                    );
                    self.approval_preview = event["preview"].as_str().map(str::to_string);
                    self.notice = "Approval required · Y approve · N deny · D details".into();
                }
                Some("complete") => {
                    self.history = event["history"].as_array().cloned().unwrap_or_default();
                    self.busy = false;
                    self.progress_active = false;
                    self.receiver = None;
                    self.cancel = None;
                    self.approval_sender = None;
                    self.approval = None;
                    self.answer_index = None;
                    if let Some(worker) = self.worker.take() {
                        let _ = worker.join();
                    }
                    if let Some(error) = event["error"].as_str() {
                        self.add("Error", error);
                        self.notice = "Queue paused · :queue resume to continue".into();
                        QUEUE_PAUSED.store(true, Ordering::SeqCst);
                    } else {
                        let elapsed = self
                            .turn_started
                            .take()
                            .map(|started| started.elapsed())
                            .unwrap_or_default();
                        if let Some(entry) = self
                            .entries
                            .iter_mut()
                            .rev()
                            .find(|entry| entry.role == "Progress")
                        {
                            entry.text = format!("✔  Finished ({:.1}s)", elapsed.as_secs_f32());
                        } else {
                            self.add(
                                "Progress",
                                format!("✔  Finished ({:.1}s)", elapsed.as_secs_f32()),
                            );
                        }
                        self.notice = "Ready".into();
                    }
                }
                Some("step_finish") => {
                    self.notice = "Saving conversation…".into();
                }
                _ => {}
            }
        }
        changed
    }
    fn details(&mut self) {
        let report = if self.approval.is_some() {
            self.approval_preview.as_ref()
        } else {
            self.last_diff.as_ref()
        };
        if let Some(report) = report {
            self.panel = Some(Panel {
                title: "Edit details · Esc closes".into(),
                rows: numbered_edit_rows(report, false)
                    .into_iter()
                    .map(|(_, row)| row)
                    .collect(),
                selected: 0,
                sessions: None,
                commands: None,
                search: None,
            });
        } else {
            self.notice = "No edit details yet".into();
        }
    }
    fn sessions(&mut self, options: &Options) -> Result<(), String> {
        if self.busy || MESSAGE_QUEUE.lock().map_err(|_| "queue unavailable")?.len() > 0 {
            return Err("stop the response and clear the queue before switching sessions".into());
        }
        let mut items = Vec::new();
        if let Ok(entries) = std::fs::read_dir(sessions_dir()?) {
            for entry in entries.flatten() {
                let Some(id) = decode_session_id(&entry.file_name().to_string_lossy()) else {
                    continue;
                };
                let Ok(bytes) = read_bounded(&entry.path(), RESPONSE_LIMIT * 4) else {
                    continue;
                };
                let Ok(stored) = serde_json::from_slice::<SessionHistory>(&bytes) else {
                    continue;
                };
                if stored.version != 1
                    || stored.messages.is_empty()
                    || stored.project_root != self.root
                    || stored.project_access != options.project_trusted
                {
                    continue;
                }
                let preview = stored
                    .first_user_message
                    .as_deref()
                    .or_else(|| first_user_message(&stored.messages))
                    .unwrap_or("Saved conversation")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                items.push((
                    entry
                        .metadata()
                        .ok()
                        .and_then(|metadata| metadata.modified().ok()),
                    id,
                    preview,
                ));
            }
        }
        items.sort_by_key(|(modified, _, _)| std::cmp::Reverse(*modified));
        let ids = items.iter().map(|(_, id, _)| id.clone()).collect();
        self.panel = Some(Panel {
            title: "Sessions · Enter switches · Esc closes".into(),
            rows: items
                .into_iter()
                .map(|(_, id, preview)| format!("{id} · {}", truncate(&preview, 120)))
                .collect(),
            selected: 0,
            sessions: Some(ids),
            commands: None,
            search: None,
        });
        Ok(())
    }
    fn choices(&mut self, title: &str, items: Vec<(String, String)>) {
        self.panel = Some(Panel {
            title: format!("{title} · Enter selects · Esc closes"),
            rows: items.iter().map(|(label, _)| label.clone()).collect(),
            search: (title == "Models").then(|| ModelSearch {
                query: String::new(),
                items: items.clone(),
            }),
            commands: Some(items.into_iter().map(|(_, command)| command).collect()),
            selected: 0,
            sessions: None,
        });
    }
    fn job(&mut self, command: &str, args: Vec<String>) -> Result<(), String> {
        if self.jobs.is_some() {
            return Err("another skill or model operation is running".into());
        }
        let executable = env::current_exe().map_err(|e| e.to_string())?;
        let (sender, receiver) = mpsc::channel();
        self.jobs = Some(receiver);
        self.notice = format!("Loading {command}…");
        let command = command.to_string();
        thread::spawn(move || {
            let output = std::process::Command::new(executable)
                .arg(&command)
                .args(args)
                .output();
            let event = match output {
                Ok(output) if command == "models" && output.status.success() => {
                    match serde_json::from_slice::<Value>(&output.stdout) {
                        Ok(items) => json!({"type":"models_result","items":items}),
                        Err(error) => json!({"type":"models_result","error":error.to_string()}),
                    }
                }
                Ok(output) => {
                    json!({"type":if command=="models"{"models_result"}else{"skills_result"},"text":format!("{}{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr)),"error":String::from_utf8_lossy(&output.stderr)})
                }
                Err(error) => {
                    json!({"type":if command=="models"{"models_result"}else{"skills_result"},"text":error.to_string(),"error":error.to_string()})
                }
            };
            let _ = sender.send(event);
        });
        Ok(())
    }
    fn command(&mut self, input: &str, options: &Options) -> Result<bool, String> {
        let normalized = input
            .strip_prefix('/')
            .map(|input| format!(":{input}"))
            .unwrap_or_else(|| input.to_string());
        let (command, argument) = normalized.split_once(' ').unwrap_or((&normalized, ""));
        match command {
            ":mode" if argument.is_empty()=>self.choices("Agent Mode",["ask","plan","build"].into_iter().map(|value|(value.to_string(),format!(":mode {value}"))).collect()),
            ":theme" if argument.is_empty()=>{
                self.choices("Themes",THEMES.iter().map(|theme|(theme.name.to_string(),format!(":theme {}",theme.id))).collect());
                if let Some(panel)=&mut self.panel {
                    panel.selected=THEMES.iter().position(|theme|theme.id==configured_theme(&self.config).id).unwrap_or(0);
                    panel.title="Themes · Live preview · Enter saves · Esc cancels".into();
                }
            },
            ":reasoning" if argument.is_empty()=>self.choices("Reasoning",["default","low","medium","high"].into_iter().map(|value|(value.to_string(),format!(":reasoning {value}"))).collect()),
            ":model" | ":models" if argument.is_empty()=>self.job("models",vec!["--format".into(),"json".into()])?,
            ":setting" | ":settings"=>self.choices("Settings",vec![(format!("Agent Mode · {}",configured_agent_mode(&self.config)),":mode".into()),("Model".into(),":model".into()),(format!("Automatic approval · {}",self.config.auto_approve_actions.unwrap_or(false)),":approval".into()),("Reasoning".into(),":reasoning".into()),("Theme".into(),":theme".into()),("Skills".into(),":skills".into()),("Proxy".into(),":proxy".into())]),
            ":proxy" if argument.is_empty()=>{ self.input=":proxy ".into();self.cursor=self.input.chars().count();self.notice="Enter a proxy URL, or :proxy off".into(); },
            ":proxy"=>{self.config.proxy_url=if argument=="off"{None}else{let _=reqwest::Proxy::all(argument).map_err(|e|format!("invalid proxy: {e}"))?;Some(argument.into())};save_user_config(&self.config)?;self.notice="Proxy updated for subsequent requests".into();},
            ":provider"=>{self.choices("Saved providers",self.config.providers.iter().map(|provider|(format!("{} · {}",provider.name,safe_proxy_label(&provider.base_url)),format!(":provider-info {}",provider.id))).collect());},
            ":provider-info"=>{self.notice="Add/update providers with nio provider in the inline CLI".into();},
            ":diff"=>{let output=std::process::Command::new("git").arg("diff").current_dir(&self.root).output().map_err(|e|e.to_string())?;let text=String::from_utf8_lossy(&output.stdout).to_string();self.last_diff=Some(text);self.details();},
            ":undo"=>{if self.busy{return Err("stop the response before undoing a file edit".into());}let restored=undo_last_change(&self.root)?;self.add("Undo",restored);},
            ":quit" | ":q" | ":exit" => return Ok(true),
            ":stop" => { if let Some(cancel) = &self.cancel { cancel.store(true, Ordering::SeqCst); if let Some(sender) = &self.approval_sender { let _ = sender.send(false); } } QUEUE_PAUSED.store(true, Ordering::SeqCst); self.notice = "Stopping; pending messages preserved".into(); },
            ":queue" => {
                if argument.is_empty() || argument=="list" {
                    let items=MESSAGE_QUEUE.lock().map_err(|_|"queue unavailable")?.iter().enumerate().map(|(index,message)|(format!("{}. {}",index+1,message.split_whitespace().collect::<Vec<_>>().join(" ")),format!(":queue-edit {}",index+1))).collect();
                    self.choices(&format!("Queue · {} · Enter/e edit · Delete/d remove · p pause/resume · Esc",if QUEUE_PAUSED.load(Ordering::SeqCst){"paused"}else{"running"}),items);
                    if let Some(panel)=&mut self.panel {if panel.rows.is_empty(){panel.rows.push("No pending messages. Enter queues while working.".into());}}

                } else {queue_command(&normalized)?; self.notice = format!("Queue {}", if QUEUE_PAUSED.load(Ordering::SeqCst) { "paused" } else { "running" });}
            },
            ":queue-edit" => {
                let index=argument.parse::<usize>().map_err(|_|"invalid queue index")?.checked_sub(1).ok_or("invalid queue index")?;
                let message=MESSAGE_QUEUE.lock().map_err(|_|"queue unavailable")?.get(index).cloned().ok_or("message is no longer queued")?;
                if self.queue_draft.is_none() {
                    self.queue_draft=Some((std::mem::take(&mut self.input),self.cursor,std::mem::take(&mut self.pastes)));
                }
                self.input=format!(":queue edit {} {}",index+1,message);
                self.cursor=self.input.chars().count();
                self.pastes=PastedBlocks::default();
                self.notice="Editing queued message · Enter saves · Esc cancels".into();
            },
            ":details" => self.details(),
            ":help" => { self.panel = Some(Panel { title:"Commands · Esc closes".into(), rows:COMMANDS.iter().map(|(name,description)|format!("{name}  {description}")).collect(), selected:0, sessions:None, commands:None,search:None }); },
            ":sessions" | ":history" | ":histoy" => self.sessions(options)?,
            ":clear" => { if self.busy { return Err("stop the response before clearing history".into()); } self.history.clear(); self.entries.clear(); self.last_diff = None; save_session_history(Some(&self.session), &self.root, &self.history, options.project_trusted)?; },
            ":model" if !argument.is_empty() => { split_model_selector(argument)?; self.model = argument.into(); self.config.default_model = Some(argument.into()); save_user_config(&self.config)?; self.notice="Model updated for subsequent requests".into(); },
            ":mode" if matches!(argument,"ask"|"plan"|"build") => { self.config.agent_mode=Some(argument.into()); save_user_config(&self.config)?; self.notice="Mode updated for subsequent requests".into(); },
            ":theme" if THEMES.iter().any(|theme|theme.id==argument) => {
                let mut config=load_user_config()?;
                config.theme=Some(argument.into());
                save_user_config(&config)?;
                self.config=config;
                self.notice=format!("Theme saved: {argument}");
            },
            ":approval" => { self.config.auto_approve_actions = Some(!self.config.auto_approve_actions.unwrap_or(false)); save_user_config(&self.config)?; },
            ":reasoning" if matches!(argument,"default"|"low"|"medium"|"high") => { self.config.reasoning_effort=Some(argument.into()); save_user_config(&self.config)?; },
            ":path" => { self.add("Path", self.root.display().to_string()); },
            ":skills" => {
                if argument.is_empty() || argument == "list" {
                    let rows=skills::list(&skills_base()?)?.into_iter().map(|skill|format!("{} · {} · {}", skill.name, if skill.enabled {"enabled"} else {"disabled"}, skill.description)).collect();
                    self.panel=Some(Panel {title:"Skills · :skills add/remove/enable/disable · Esc closes".into(),rows,selected:0,sessions:None,commands:None,search:None});
                } else {
                    self.job("skills",argument.split_whitespace().map(str::to_string).collect())?;
                }
            }
            ":continue" => { let prompt="Continue the unfinished task using the saved history and current files.".to_string(); if self.busy {enqueue_message(prompt)?;} else {self.start(options,prompt)?;} }
            _ => return Err("Use :help. TUI settings accept values: :mode build, :model SELECTOR, :theme ocean, :reasoning high.".into()),
        }
        Ok(false)
    }
    fn draw(&mut self) -> Result<(), String> {
        let (width, height) = terminal::size().map_err(|e| e.to_string())?;
        let width = width as usize;
        let height = height as usize;
        if width < 20 || height < 8 {
            self.frame_size = (0, 0);
            execute!(io::stdout(), MoveTo(0, 0), Clear(ClearType::All))
                .map_err(|e| e.to_string())?;
            print!("Resize terminal to at least 20×8");
            return Ok(());
        }
        let theme = self
            .panel
            .as_ref()
            .and_then(|panel| {
                panel
                    .commands
                    .as_ref()
                    .and_then(|commands| commands.get(panel.selected))
            })
            .and_then(|command| command.strip_prefix(":theme "))
            .and_then(|id| THEMES.iter().find(|theme| theme.id == id))
            .copied()
            .unwrap_or_else(|| configured_theme(&self.config));
        let soft_light = matches!(theme.id, "light" | "paper" | "cloud");
        let foreground = if soft_light { 24 } else { 252 };
        let background = match theme.id {
            "light" => Color::Rgb {
                r: 226,
                g: 230,
                b: 236,
            },
            "paper" => Color::Rgb {
                r: 232,
                g: 224,
                b: 209,
            },
            "cloud" => Color::Rgb {
                r: 231,
                g: 236,
                b: 241,
            },
            "tokyo" => Color::Rgb {
                r: 26,
                g: 27,
                b: 38,
            },
            "ocean" => Color::Rgb {
                r: 10,
                g: 22,
                b: 38,
            },
            "forest" => Color::Rgb {
                r: 12,
                g: 25,
                b: 18,
            },
            "sunset" => Color::Rgb {
                r: 35,
                g: 16,
                b: 26,
            },
            "dracula" => Color::Rgb {
                r: 40,
                g: 42,
                b: 54,
            },
            "nord" => Color::Rgb {
                r: 46,
                g: 52,
                b: 64,
            },
            "solarized" => Color::Rgb { r: 0, g: 43, b: 54 },
            "monokai" => Color::Rgb {
                r: 39,
                g: 40,
                b: 34,
            },
            _ => Color::Rgb {
                r: 18,
                g: 18,
                b: 22,
            },
        };
        let mut frame_rows = vec![String::new(); height];
        let mut put = |row: usize, text: &str| -> Result<(), String> {
            if let Some(line) = frame_rows.get_mut(row) {
                *line = clip_terminal_text(text, width.saturating_sub(1)).replace(
                    "\x1b[0m",
                    &match background {
                        Color::Rgb { r, g, b } => {
                            format!("\x1b[0m\x1b[48;2;{r};{g};{b}m\x1b[38;5;{foreground}m")
                        }
                        _ => "\x1b[0m".into(),
                    },
                );
            }
            Ok(())
        };
        let header = format!(
            "{} · {}",
            render_status_bar(&self.config, &self.root, &self.history, &self.model),
            self.session
        );
        put(0, &format!("\x1b[38;5;{}m{header}\x1b[0m", theme.accent))?;
        put(
            1,
            &format!(
                "\x1b[38;5;{}m{}\x1b[0m",
                theme.muted,
                "─".repeat(width.saturating_sub(1))
            ),
        )?;
        let queue = MESSAGE_QUEUE
            .lock()
            .map_err(|_| "queue unavailable")?
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let queue_rows = queue.len().min(3);
        let input_height = 3usize.min(height / 3);
        let input_row = height - 2 - input_height;
        let body_end = input_row.saturating_sub(1);
        let content_end = body_end.saturating_sub(queue_rows + usize::from(!queue.is_empty()));
        let content_start = 3usize;
        let content_height = content_end.saturating_sub(content_start);
        let mut rows = Vec::new();
        let active_progress = self
            .entries
            .iter()
            .rposition(|entry| entry.role == "Progress");
        for (entry_index, entry) in self.entries.iter().enumerate() {
            let mut entry = Entry {
                role: entry.role.clone(),
                text: entry.text.clone(),
            };
            if self.busy && self.progress_active && Some(entry_index) == active_progress {
                const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let elapsed = self
                    .turn_started
                    .map(|started| started.elapsed())
                    .unwrap_or_default();
                let frame = FRAMES[(elapsed.as_millis() / 80) as usize % FRAMES.len()];
                entry.text = format!("{frame} {} ({}s)", entry.text, elapsed.as_secs());
            }
            rows.extend(chat_rows(&entry, width));
            rows.push(String::new());
        }
        let max_scroll = rows.len().saturating_sub(content_height);
        self.scroll = self.scroll.min(max_scroll);
        let start = rows.len().saturating_sub(content_height + self.scroll);
        for (index, row) in rows.iter().skip(start).take(content_height).enumerate() {
            put(index + content_start, row)?;
        }
        if !queue.is_empty() {
            put(
                content_end,
                &format!(
                    "Queued · {}{}",
                    queue.len(),
                    if QUEUE_PAUSED.load(Ordering::SeqCst) {
                        " · paused"
                    } else {
                        ""
                    }
                ),
            )?;
            for (index, text) in queue.iter().take(queue_rows).enumerate() {
                put(
                    content_end + 1 + index,
                    &format!(
                        " {}. {}",
                        index + 1,
                        text.split_whitespace().collect::<Vec<_>>().join(" ")
                    ),
                )?;
            }
        }
        put(
            input_row.saturating_sub(1),
            &"─".repeat(width.saturating_sub(1)),
        )?;
        let (rendered, _, positions) = render_input_text("🤖 nio> ", &self.input, width);
        let lines = rendered.split("\r\n").collect::<Vec<_>>();
        let cursor_pos = positions.get(self.cursor).copied().unwrap_or((0, 0));
        let first = usize::from(cursor_pos.0).saturating_sub(input_height - 1);
        for (index, row) in lines.iter().skip(first).take(input_height).enumerate() {
            put(input_row + index, row)?;
        }
        self.composer_row = input_row as u16;
        self.composer_positions = positions
            .iter()
            .map(|(row, col)| {
                (
                    if usize::from(*row) < first {
                        u16::MAX
                    } else {
                        row.saturating_sub(first as u16)
                    },
                    *col,
                )
            })
            .collect();
        let footer = if let Some(action) = &self.approval {
            format!("Approve {action}? Y/N · D details")
        } else {
            format!(
                "{} · Enter {} · F2/:queue list · PgUp/Down scroll · Ctrl+C stop",
                self.notice,
                if self.busy { "queue" } else { "send" }
            )
        };
        put(
            height - 2,
            &format!(
                "\x1b[38;5;{}m{}\x1b[0m",
                theme.muted,
                "─".repeat(width.saturating_sub(1))
            ),
        )?;
        put(height - 1, &footer)?;
        let suggestions = command_suggestions(&self.input);
        if self.panel.is_none() && !suggestions.is_empty() {
            let rows = suggestions
                .iter()
                .map(|command| {
                    let description = COMMANDS
                        .iter()
                        .find(|(name, _)| name == command)
                        .map(|(_, description)| *description)
                        .unwrap_or_default();
                    format!("{command} · {description}")
                })
                .collect::<Vec<_>>();
            let rows = selected_rows(&rows, self.palette_selected, theme.accent);
            let panel_height = (rows.len() + 3)
                .min(body_end.saturating_sub(content_start))
                .max(4);
            let start = body_end.saturating_sub(panel_height).max(content_start);
            let rendered = render_inline_menu(
                "Commands",
                &rows,
                self.palette_selected,
                "↑/↓ select · Tab complete · Enter run",
                width,
                panel_height,
                width.saturating_sub(1),
            );
            for (index, row) in rendered.split("\r\n").enumerate() {
                if start + index < body_end {
                    put(start + index, row)?;
                }
            }
        }
        if let Some(panel) = &self.panel {
            let mut panel_start = content_start;
            if let Some(search) = &panel.search {
                put(
                    panel_start,
                    &format!(
                        "Search models: {}▏ · {}/{}",
                        search.query,
                        panel.rows.len(),
                        search.items.len()
                    ),
                )?;
                panel_start += 1;
            }
            let rows = if panel.rows.is_empty() && panel.search.is_some() {
                vec!["No matching models. Change the search text.".into()]
            } else {
                selected_rows(&panel.rows, panel.selected, theme.accent)
            };
            let rendered = render_inline_menu(
                &panel.title,
                &rows,
                panel.selected,
                if panel.search.is_some() {
                    "Type to search · ↑/↓ select · Enter use · Esc close"
                } else {
                    "↑/↓ move · Enter select · Esc close"
                },
                width,
                body_end.saturating_sub(panel_start),
                width.saturating_sub(1),
            );
            for (index, row) in rendered.split("\r\n").enumerate() {
                if index + panel_start < body_end {
                    put(index + panel_start, row)?;
                }
            }
        }
        drop(put);
        let resized = self.frame_size != (width, height);
        let recolored = self.frame_background != Some(background);
        let frame_cursor = if self.panel.is_none() && self.approval.is_none() {
            Some((
                cursor_pos.1,
                (input_row + usize::from(cursor_pos.0).saturating_sub(first)) as u16,
            ))
        } else {
            None
        };
        if !resized
            && !recolored
            && self.frame_rows == frame_rows
            && self.frame_cursor == frame_cursor
        {
            return Ok(());
        }
        let mut output = Vec::<u8>::new();
        queue!(
            output,
            Hide,
            SetBackgroundColor(background),
            SetForegroundColor(Color::AnsiValue(foreground))
        )
        .map_err(|e| e.to_string())?;
        if resized {
            queue!(output, MoveTo(0, 0), Clear(ClearType::All)).map_err(|e| e.to_string())?;
        }
        for (row, line) in frame_rows.iter().enumerate() {
            if resized || recolored || self.frame_rows.get(row) != Some(line) {
                queue!(
                    output,
                    MoveTo(0, row as u16),
                    SetBackgroundColor(background),
                    SetForegroundColor(Color::AnsiValue(foreground))
                )
                .map_err(|e| e.to_string())?;
                write!(output, "{line}").map_err(|e| e.to_string())?;
                queue!(output, Clear(ClearType::UntilNewLine)).map_err(|e| e.to_string())?;
            }
        }
        if self.panel.is_none() && self.approval.is_none() {
            queue!(
                output,
                MoveTo(
                    cursor_pos.1,
                    (input_row + usize::from(cursor_pos.0).saturating_sub(first)) as u16
                ),
                Show
            )
            .map_err(|e| e.to_string())?;
        }
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(&output)
            .and_then(|_| stdout.flush())
            .map_err(|e| e.to_string())?;
        self.frame_rows = frame_rows;
        self.frame_size = (width, height);
        self.frame_background = Some(background);
        self.frame_cursor = frame_cursor;
        Ok(())
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        if let Some(sender) = &self.approval_sender {
            let _ = sender.send(false);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub async fn run(options: Options) -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("--tui requires an interactive terminal".into());
    }
    let root = session_root(&options)?;
    let model = chosen_model(&options).await?;
    let session = options
        .session_id
        .clone()
        .unwrap_or_else(generate_session_id);
    let mut session_lock = lock_session(&session)?;
    let history = load_session_history(Some(&session), &root, options.project_trusted)?;
    let _guard = ScreenGuard::enter()?;
    let mut state = State {
        entries: Vec::new(),
        input: String::new(),
        cursor: 0,
        pastes: PastedBlocks::default(),
        queue_draft: None,
        scroll: 0,
        busy: false,
        notice: "Ready · :help for commands".into(),
        panel: None,
        approval: None,
        approval_preview: None,
        approval_sender: None,
        cancel: None,
        worker: None,
        receiver: None,
        jobs: None,
        answer_index: None,
        last_diff: None,
        turn_started: None,
        progress_active: false,
        history,
        session,
        model,
        root,
        config: load_user_config()?,
        composer_row: 0,
        composer_positions: Vec::new(),
        palette_selected: 0,
        frame_rows: Vec::new(),
        frame_size: (0, 0),
        frame_background: None,
        frame_cursor: None,
    };
    if let Some(mode) = &options.mode {
        state.config.agent_mode = Some(mode.clone());
    }
    if let Some(reasoning) = &options.reasoning {
        state.config.reasoning_effort = Some(reasoning.clone());
    }
    state.restore();
    if !options.prompt.is_empty() {
        state.start(&options, options.prompt.join(" "))?;
    }
    let mut dirty = true;
    let mut quitting = false;
    loop {
        dirty |= state.events();
        if CTRL_C_COUNT.load(Ordering::SeqCst) >= 2 {
            quitting = true;
        }
        if quitting && !state.busy {
            break;
        }
        if !state.busy
            && !quitting
            && !QUEUE_PAUSED.load(Ordering::SeqCst)
            && !state
                .panel
                .as_ref()
                .is_some_and(|panel| panel.title.starts_with("Queue"))
            && !state.input.starts_with(":queue edit ")
        {
            let next = MESSAGE_QUEUE
                .lock()
                .ok()
                .and_then(|mut queue| queue.pop_front());
            if let Some(prompt) = next {
                if prompt.starts_with(':') || prompt.starts_with('/') {
                    match state.command(&prompt, &options) {
                        Ok(exit) => quitting = exit,
                        Err(error) => state.notice = error,
                    }
                } else if let Err(error) = state.start(&options, prompt) {
                    state.notice = error;
                    QUEUE_PAUSED.store(true, Ordering::SeqCst);
                }
                dirty = true;
            }
        }
        if dirty {
            state.draw()?;
            dirty = false;
        }
        if state.busy && state.progress_active {
            state.draw()?;
        }
        if !event::poll(Duration::from_millis(40)).map_err(|e| e.to_string())? {
            continue;
        }
        let input_event = event::read().map_err(|e| e.to_string())?;
        if matches!(&input_event, Event::Mouse(mouse) if matches!(mouse.kind,MouseEventKind::Moved|MouseEventKind::Drag(_)))
        {
            continue;
        }
        dirty = true;
        match input_event {
            Event::Resize(_, _) => {}
            Event::Paste(text) => {
                if let Some(panel) = &mut state.panel
                    && let Some(search) = &mut panel.search
                {
                    search
                        .query
                        .push_str(&text.split_whitespace().collect::<Vec<_>>().join(" "));
                    panel.filter_models();
                    continue;
                }
                if let Some(paths) = dropped_file_paths(&text) {
                    let marker = paths
                        .iter()
                        .map(|path| format!("@{{{}}}", path.display()))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let separator = if state.input.is_empty() { "" } else { " " };
                    let insertion = format!("{separator}{marker}");
                    insert_text_at_cursor(&mut state.input, &mut state.cursor, &insertion);
                    state.notice = format!(
                        "Attached {} file{} · Enter to send",
                        paths.len(),
                        if paths.len() == 1 { "" } else { "s" }
                    );
                } else {
                    state
                        .pastes
                        .insert(&mut state.input, &mut state.cursor, &text);
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(panel) = &mut state.panel {
                        panel.selected = panel.selected.saturating_sub(1);
                    } else {
                        state.scroll = state.scroll.saturating_add(3);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(panel) = &mut state.panel {
                        panel.selected =
                            (panel.selected + 1).min(panel.rows.len().saturating_sub(1));
                    } else {
                        state.scroll = state.scroll.saturating_sub(3);
                    }
                }
                MouseEventKind::Down(MouseButton::Left) if mouse.row >= state.composer_row => {
                    state.cursor = state
                        .composer_positions
                        .iter()
                        .enumerate()
                        .filter(|(_, (row, _))| *row < 3)
                        .min_by_key(|(_, (row, col))| {
                            usize::from(*row).abs_diff(usize::from(mouse.row - state.composer_row))
                                * 1000
                                + usize::from(*col).abs_diff(usize::from(mouse.column))
                        })
                        .map(|(index, _)| index)
                        .unwrap_or(state.cursor);
                    state.cursor = state.pastes.snap_cursor(&state.input, state.cursor, true);
                }
                _ => {}
            },
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                    && state.busy
                {
                    if let Some(cancel) = &state.cancel {
                        cancel.store(true, Ordering::SeqCst);
                    }
                    if let Some(sender) = &state.approval_sender {
                        let _ = sender.send(false);
                    }
                    state.approval = None;
                    state.panel = None;
                    QUEUE_PAUSED.store(true, Ordering::SeqCst);
                    state.notice = "Stopping…".into();
                    continue;
                }
                if state
                    .panel
                    .as_ref()
                    .is_some_and(|panel| panel.title.starts_with("Queue"))
                {
                    let selected = state.panel.as_ref().unwrap().selected;
                    let action = match key.code {
                        KeyCode::Delete | KeyCode::Char('d') => {
                            Some(format!(":queue remove {}", selected + 1))
                        }
                        KeyCode::Char('p') => Some(format!(
                            ":queue {}",
                            if QUEUE_PAUSED.load(Ordering::SeqCst) {
                                "resume"
                            } else {
                                "pause"
                            }
                        )),
                        KeyCode::Char('e') => Some(format!(":queue-edit {}", selected + 1)),
                        _ => None,
                    };
                    if let Some(action) = action {
                        state.panel = None;
                        if let Err(error) = state.command(&action, &options) {
                            state.notice = error;
                        }
                        if !action.starts_with(":queue-edit") {
                            let _ = state.command(":queue", &options);
                        }
                        continue;
                    }
                }
                if let Some(panel) = &mut state.panel {
                    match key.code {
                        KeyCode::Esc => state.panel = None,
                        KeyCode::Up => panel.selected = panel.selected.saturating_sub(1),
                        KeyCode::Down => {
                            panel.selected =
                                (panel.selected + 1).min(panel.rows.len().saturating_sub(1))
                        }
                        KeyCode::PageUp => panel.selected = panel.selected.saturating_sub(10),
                        KeyCode::PageDown => {
                            panel.selected =
                                (panel.selected + 10).min(panel.rows.len().saturating_sub(1))
                        }
                        KeyCode::Home => panel.selected = 0,
                        KeyCode::End => panel.selected = panel.rows.len().saturating_sub(1),
                        KeyCode::Char(character)
                            if panel.search.is_some()
                                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            panel.search.as_mut().unwrap().query.push(character);
                            panel.filter_models();
                        }
                        KeyCode::Backspace if panel.search.is_some() => {
                            panel.search.as_mut().unwrap().query.pop();
                            panel.filter_models();
                        }
                        KeyCode::Char('u')
                            if panel.search.is_some()
                                && key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            panel.search.as_mut().unwrap().query.clear();
                            panel.filter_models();
                        }
                        KeyCode::Enter
                            if panel
                                .commands
                                .as_ref()
                                .is_some_and(|commands| !commands.is_empty()) =>
                        {
                            let command = panel
                                .commands
                                .as_ref()
                                .and_then(|commands| commands.get(panel.selected))
                                .cloned();
                            state.panel = None;
                            if let Some(command) = command {
                                if let Err(error) = state.command(&command, &options) {
                                    state.notice = error;
                                }
                            }
                        }
                        KeyCode::Enter if panel.sessions.is_some() => {
                            let next = panel
                                .sessions
                                .as_ref()
                                .and_then(|ids| ids.get(panel.selected))
                                .cloned();
                            if let Some(id) = next {
                                let switched = (|| {
                                    if id == state.session {
                                        return Ok(());
                                    }
                                    let lock = lock_session(&id)?;
                                    let history = load_session_history(
                                        Some(&id),
                                        &state.root,
                                        options.project_trusted,
                                    )?;
                                    save_session_history(
                                        Some(&state.session),
                                        &state.root,
                                        &state.history,
                                        options.project_trusted,
                                    )?;
                                    session_lock = lock;
                                    state.session = id;
                                    state.history = history;
                                    state.last_diff = None;
                                    state.restore();
                                    Ok::<_, String>(())
                                })();
                                if let Err(error) = switched {
                                    state.notice = error;
                                }
                            }
                            state.panel = None;
                        }
                        _ => {}
                    }
                    continue;
                }
                if state.approval.is_some() {
                    match key.code {
                        KeyCode::Char('y' | 'Y' | 'n' | 'N') | KeyCode::Esc => {
                            let approved = matches!(key.code, KeyCode::Char('y' | 'Y'));
                            if let Some(sender) = &state.approval_sender {
                                let _ = sender.send(approved);
                            }
                            state.approval = None;
                            state.notice = "Working…".into();
                        }
                        KeyCode::Char('d' | 'D') => state.details(),
                        _ => {}
                    }
                    continue;
                }
                let suggestions = command_suggestions(&state.input);
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if state.busy {
                            if let Some(cancel) = &state.cancel {
                                cancel.store(true, Ordering::SeqCst);
                            }
                            QUEUE_PAUSED.store(true, Ordering::SeqCst);
                            state.notice = "Stopping…".into();
                        } else if !state.input.is_empty() {
                            state.input.clear();
                            state.cursor = 0;
                        } else {
                            quitting = true;
                        }
                    }
                    KeyCode::Esc => {
                        if let Some((input, cursor, pastes)) = state.queue_draft.take() {
                            state.input = input;
                            state.cursor = cursor;
                            state.pastes = pastes;
                            state.notice = "Queue edit cancelled".into();
                        } else {
                            state.input.clear();
                            state.cursor = 0;
                            state.pastes = PastedBlocks::default();
                        }
                    }
                    KeyCode::F(2) => {
                        if let Err(error) = state.command(":queue", &options) {
                            state.notice = error;
                        }
                    }
                    KeyCode::PageUp => state.scroll = state.scroll.saturating_add(10),
                    KeyCode::PageDown => state.scroll = state.scroll.saturating_sub(10),
                    KeyCode::Up if !suggestions.is_empty() => {
                        state.palette_selected = state.palette_selected.saturating_sub(1)
                    }
                    KeyCode::Down if !suggestions.is_empty() => {
                        state.palette_selected =
                            (state.palette_selected + 1).min(suggestions.len() - 1)
                    }
                    KeyCode::Up => state.scroll = state.scroll.saturating_add(1),
                    KeyCode::Down => state.scroll = state.scroll.saturating_sub(1),
                    KeyCode::Left => {
                        state.cursor = state.pastes.snap_cursor(
                            &state.input,
                            state.cursor.saturating_sub(1),
                            false,
                        )
                    }
                    KeyCode::Right => {
                        state.cursor = state.pastes.snap_cursor(
                            &state.input,
                            (state.cursor + 1).min(state.input.chars().count()),
                            true,
                        )
                    }
                    KeyCode::Home => state.cursor = 0,
                    KeyCode::End => state.cursor = state.input.chars().count(),
                    KeyCode::Backspace => {
                        state
                            .pastes
                            .delete(&mut state.input, &mut state.cursor, true)
                    }
                    KeyCode::Delete => {
                        state
                            .pastes
                            .delete(&mut state.input, &mut state.cursor, false)
                    }
                    KeyCode::Tab if !suggestions.is_empty() => {
                        state.input =
                            suggestions[state.palette_selected.min(suggestions.len() - 1)].into();
                        state.cursor = state.input.chars().count();
                    }
                    KeyCode::Enter
                        if key
                            .modifiers
                            .contains(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                    {
                        insert_text_at_cursor(&mut state.input, &mut state.cursor, "\n")
                    }
                    KeyCode::Enter => {
                        let mut input = state.pastes.expand(&state.input);
                        if !suggestions.is_empty()
                            && !COMMANDS.iter().any(|(command, _)| *command == input)
                        {
                            input = suggestions[state.palette_selected.min(suggestions.len() - 1)]
                                .to_string();
                        }
                        let input = input.trim().to_string();
                        if input.is_empty() {
                            continue;
                        }
                        let result = if input.starts_with(':') || input.starts_with('/') {
                            state.command(&input, &options)
                        } else if state.busy {
                            enqueue_message(input.clone()).map(|_| false)
                        } else {
                            state.start(&options, input.clone()).map(|_| false)
                        };
                        match result {
                            Ok(exit) => {
                                quitting = exit;
                                if input.starts_with(":queue edit ")
                                    || input.starts_with("/queue edit ")
                                {
                                    if let Some((input, cursor, pastes)) = state.queue_draft.take()
                                    {
                                        state.input = input;
                                        state.cursor = cursor;
                                        state.pastes = pastes;
                                    } else {
                                        state.input.clear();
                                        state.cursor = 0;
                                        state.pastes = PastedBlocks::default();
                                    }
                                } else if state.input != ":proxy "
                                    && !state.input.starts_with(":queue edit ")
                                {
                                    state.input.clear();
                                    state.cursor = 0;
                                    state.pastes = PastedBlocks::default();
                                }
                                state.palette_selected = 0;
                            }
                            Err(error) => state.notice = error,
                        }
                        if quitting && state.busy {
                            if let Some(cancel) = &state.cancel {
                                cancel.store(true, Ordering::SeqCst);
                            }
                            if let Some(sender) = &state.approval_sender {
                                let _ = sender.send(false);
                            }
                        }
                    }
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        insert_text_at_cursor(
                            &mut state.input,
                            &mut state.cursor,
                            &character.to_string(),
                        );
                        state.palette_selected = 0;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    save_session_history(
        Some(&state.session),
        &state.root,
        &state.history,
        options.project_trusted,
    )?;
    drop(session_lock);
    drop(_guard);
    println!(
        "Session saved. Resume with: nio --tui --session '{}'",
        state.session
    );
    Ok(())
}
