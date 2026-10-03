use super::*;
use crossterm::cursor::{Hide, Show};

#[derive(Default)]
pub struct Draft {
    pub input: String,
    pub cursor: usize,
    pub pastes: PastedBlocks,
}
pub static DRAFT: Mutex<Option<Draft>> = Mutex::new(None);

fn snapshot() -> Vec<String> {
    MESSAGE_QUEUE
        .lock()
        .map(|queue| queue.iter().cloned().collect())
        .unwrap_or_default()
}
fn rows(messages: &[String], selected: usize) -> Vec<String> {
    if messages.is_empty() {
        return vec!["    No pending messages. Type a message and Enter to queue it.".into()];
    }
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            format!(
                "{} {}. {}",
                if index == selected { "› ✓" } else { "   " },
                index + 1,
                strip_terminal_ansi(message)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect()
}
fn replace(index: usize, text: String) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > 24 * 1024 {
        return Err("message must contain text and fit the 24 KiB limit".into());
    }
    let mut queue = MESSAGE_QUEUE.lock().map_err(|_| "queue unavailable")?;
    let item = queue
        .get_mut(index)
        .ok_or("selected message is no longer queued")?;
    *item = text;
    Ok(())
}
fn remove(index: usize) {
    if let Ok(mut queue) = MESSAGE_QUEUE.lock() {
        queue.remove(index);
    }
}

pub fn manage() -> Result<(), String> {
    if !io::stdin().is_terminal() {
        return queue_command(":queue list-text");
    }
    let mut guard = RawModeGuard::acquire()?;
    let mut stdout = io::stdout();
    let mut frame = InlineMenuFrame::default();
    let mut selected = 0;
    write!(stdout, "\r\n").map_err(|e| e.to_string())?;
    loop {
        let messages = snapshot();
        selected = selected.min(messages.len().saturating_sub(1));
        frame.draw(
            &mut stdout,
            &format!(
                "Queue · {} pending · {}",
                messages.len(),
                if QUEUE_PAUSED.load(Ordering::SeqCst) {
                    "paused"
                } else {
                    "running"
                }
            ),
            &rows(&messages, selected),
            selected,
            "↑/↓ select · Enter/e edit · Delete/d remove · p pause/resume · Esc done",
        )?;
        let Event::Key(key) = event::read().map_err(|e| e.to_string())? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Down => selected = (selected + 1).min(messages.len().saturating_sub(1)),
            KeyCode::Delete | KeyCode::Char('d') => remove(selected),
            KeyCode::Char('p') => {
                QUEUE_PAUSED.fetch_xor(true, Ordering::SeqCst);
            }
            KeyCode::Enter | KeyCode::Char('e') if !messages.is_empty() => {
                frame.clear(&mut stdout)?;
                let edited =
                    read_interactive_line_raw("🤖 edit> ", &[], &[], None, &messages[selected]);
                if let Ok(PromptInput::Line(text)) = edited {
                    if let Err(error) = replace(selected, text) {
                        write!(stdout, "{error}\r\n").map_err(|e| e.to_string())?;
                    }
                }
            }
            KeyCode::Esc | KeyCode::F(2) => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
            _ => {}
        }
    }
    frame.clear(&mut stdout)?;
    stdout.flush().map_err(|e| e.to_string())?;
    guard.release();
    Ok(())
}

struct Worker {
    cancel: Arc<AtomicBool>,
    approve: mpsc::Sender<bool>,
    answer: mpsc::Sender<Option<String>>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        let _ = self.approve.send(false);
        let _ = self.answer.send(None);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
struct ApprovalPrompt {
    action: String,
    details: Option<String>,
    selected_yes: bool,
    details_visible: bool,
}
struct Live {
    draft: Draft,
    saved_draft: Option<Draft>,
    editing: Option<usize>,
    queue_open: bool,
    selected: usize,
    palette_selected: usize,
    status: String,
    status_started: Instant,
    last_frame: Instant,
    pending: String,
    formatter: Option<MarkdownFormatter>,
    screen: InputRenderState,
    approval: Option<ApprovalPrompt>,
}
impl Live {
    fn commit(&mut self, text: &str) -> Result<(), String> {
        let mut stdout = io::stdout();
        clear_input_region(&mut stdout, &mut self.screen)?;
        write!(
            stdout,
            "{}",
            text.replace("\r\n", "\n").replace('\n', "\r\n")
        )
        .map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())
    }
    fn flush_partial(&mut self) -> Result<(), String> {
        if let Some(mut formatter) = self.formatter.take() {
            let started = formatter.output_started;
            let tail = formatter.finish();
            if !started && !tail.is_empty() {
                self.pending.push_str(ASSISTANT_PREFIX);
            }
            self.pending.push_str(
                &String::from_utf8(indent_response_lines(&tail, "\r\n")).unwrap_or_default(),
            );
        }
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.commit(&format!("{pending}\r\n"))?;
        }
        Ok(())
    }
    fn stream(&mut self, text: &str) -> Result<(), String> {
        if self.formatter.is_none() {
            self.formatter = Some(MarkdownFormatter::new(true));
        }
        let formatter = self.formatter.as_mut().unwrap();
        let started = formatter.output_started;
        let formatted = formatter.push(text);
        if !started && !formatted.is_empty() {
            self.pending.push_str(ASSISTANT_PREFIX);
        }
        self.pending.push_str(
            &String::from_utf8(indent_response_lines(&formatted, "\r\n")).unwrap_or_default(),
        );
        if let Some(end) = self.pending.rfind('\n') {
            let complete = self.pending[..=end].to_string();
            self.pending.drain(..=end);
            self.commit(&complete)?;
        }
        Ok(())
    }
    fn draw(&mut self) -> Result<(), String> {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        let width = width as usize;
        let messages = snapshot();
        self.selected = self.selected.min(messages.len().saturating_sub(1));
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame =
            frames[(self.status_started.elapsed().as_millis() / 80) as usize % frames.len()];
        let mut lines = vec![clip_terminal_text(
            &format!(
                "\x1b[36m{frame}\x1b[0m {} \x1b[2m({}s)\x1b[0m",
                self.status,
                self.status_started.elapsed().as_secs()
            ),
            width.saturating_sub(1),
        )];
        self.last_frame = Instant::now();
        if !self.pending.is_empty() {
            lines.push(clip_terminal_text(&self.pending, width.saturating_sub(1)));
        }
        if let Some(approval) = &self.approval {
            lines.clear();
            if approval.details_visible
                && let Some(details) = &approval.details
            {
                lines.extend(
                    wrap_saved_message(details, width.saturating_sub(2))
                        .into_iter()
                        .take(4)
                        .map(|line| clip_terminal_text(&line, width.saturating_sub(1))),
                );
                lines.push(String::new());
            }
            let choices = [
                format!(
                    " {}  Yes · Approve this action",
                    if approval.selected_yes {
                        "\x1b[1;36m›\x1b[0m"
                    } else {
                        " "
                    }
                ),
                format!(
                    " {}  No · Decline",
                    if approval.selected_yes {
                        " "
                    } else {
                        "\x1b[1;36m›\x1b[0m"
                    }
                ),
            ];
            let selected = if approval.selected_yes { 0 } else { 1 };
            let rendered = render_inline_menu(
                &format!("Approve · {}", approval.action),
                &choices,
                selected,
                if approval.details.is_some() {
                    if approval.details_visible {
                        "↑/↓ choose · Enter confirm · y/n shortcut · d hide details · Esc cancel"
                    } else {
                        "↑/↓ choose · Enter confirm · y/n shortcut · d show details · Esc cancel"
                    }
                } else {
                    "↑/↓ choose · Enter confirm · y/n shortcut · Esc cancel"
                },
                width,
                height as usize,
                78,
            );
            lines.extend(rendered.split("\r\n").map(str::to_string));
            let excess = lines.len().saturating_sub(height.max(1) as usize);
            lines.drain(..excess);
            let mut stdout = io::stdout();
            clear_input_region(&mut stdout, &mut self.screen)?;
            for (index, line) in lines.iter().enumerate() {
                if index > 0 {
                    write!(stdout, "\r\n").map_err(|e| e.to_string())?;
                }
                write!(stdout, "{line}").map_err(|e| e.to_string())?;
            }
            queue!(stdout, Hide).map_err(|e| e.to_string())?;
            let end_row = lines.len().saturating_sub(1) as u16;
            self.screen = InputRenderState {
                rows: lines.len() as u16,
                end_row,
                end_column: lines
                    .last()
                    .map(|row| terminal_text_width(row) as u16)
                    .unwrap_or(0),
                cursor_row: end_row,
                cursor_column: 0,
            };
            return stdout.flush().map_err(|e| e.to_string());
        }
        if self.queue_open {
            let rendered = render_inline_menu(
                &format!(
                    "Queue · {}",
                    if QUEUE_PAUSED.load(Ordering::SeqCst) {
                        "paused"
                    } else {
                        "running"
                    }
                ),
                &rows(&messages, self.selected),
                self.selected,
                "↑/↓ select · Enter/e edit · Delete/d remove · p pause/resume · Esc close",
                width,
                (height as usize / 2).max(4),
                78,
            );
            lines.extend(rendered.split("\r\n").map(str::to_string));
        } else {
            let suggestions = command_suggestions(&self.draft.input);
            if !suggestions.is_empty() {
                self.palette_selected = self.palette_selected.min(suggestions.len() - 1);
                let items = suggestions
                    .iter()
                    .enumerate()
                    .map(|(index, command)| {
                        format!(
                            "{} {command}",
                            if index == self.palette_selected {
                                "› ✓"
                            } else {
                                "   "
                            }
                        )
                    })
                    .collect::<Vec<_>>();
                lines.extend(
                    render_inline_menu(
                        "Commands",
                        &items,
                        self.palette_selected,
                        "↑/↓ select · Tab complete · Enter run",
                        width,
                        (height as usize / 3).max(4),
                        78,
                    )
                    .split("\r\n")
                    .map(str::to_string),
                );
            }
        }
        lines.push(String::new());
        lines.push(clip_terminal_text(
            &format!(
                "\x1b[2mQueued {} · Enter queues · F2 / :queue list\x1b[0m",
                messages.len()
            ),
            width.saturating_sub(1),
        ));
        let prompt = if self.editing.is_some() {
            "🤖 edit> "
        } else {
            "🤖 queue> "
        };
        let (input, _, positions) = render_input_text(prompt, &self.draft.input, width);
        let position = positions.get(self.draft.cursor).copied().unwrap_or((0, 0));
        let input_rows = input.split("\r\n").collect::<Vec<_>>();
        let first = usize::from(position.0).saturating_sub(2);
        let cursor_row = lines.len() + usize::from(position.0).saturating_sub(first);
        lines.extend(
            input_rows
                .iter()
                .skip(first)
                .take(3)
                .map(|row| row.to_string()),
        );
        // Keep the editable region inside the viewport so rewinding never
        // removes committed scrollback on small terminals.
        let excess = lines.len().saturating_sub(height.max(1) as usize);
        lines.drain(..excess);
        let cursor_row = cursor_row.saturating_sub(excess);
        let mut stdout = io::stdout();
        clear_input_region(&mut stdout, &mut self.screen)?;
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                write!(stdout, "\r\n").map_err(|e| e.to_string())?;
            }
            write!(stdout, "{line}").map_err(|e| e.to_string())?;
        }
        let end_row = lines.len().saturating_sub(1) as u16;
        let cursor_row = (cursor_row as u16).min(end_row);
        if end_row > cursor_row {
            queue!(stdout, MoveUp(end_row - cursor_row)).map_err(|e| e.to_string())?;
        }
        queue!(stdout, MoveToColumn(position.1)).map_err(|e| e.to_string())?;
        queue!(stdout, Show).map_err(|e| e.to_string())?;
        self.screen = InputRenderState {
            rows: lines.len() as u16,
            end_row,
            end_column: lines
                .last()
                .map(|row| terminal_text_width(row) as u16)
                .unwrap_or(0),
            cursor_row,
            cursor_column: position.1,
        };
        stdout.flush().map_err(|e| e.to_string())
    }
    fn close_queue(&mut self) {
        self.queue_open = false;
        if let Some(draft) = self.saved_draft.take() {
            self.draft = draft;
        }
        self.editing = None;
    }
    fn open_queue(&mut self) {
        self.queue_open = true;
        self.selected = 0;
    }
    fn key(&mut self, key: event::KeyEvent, worker: &Worker) -> Result<(), String> {
        if key.kind == KeyEventKind::Release {
            return Ok(());
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.approve.send(false);
            self.approval = None;
            QUEUE_PAUSED.store(true, Ordering::SeqCst);
            self.status = "Stopping; queue preserved".into();
            return Ok(());
        }
        if let Some(approval) = &mut self.approval {
            let answer = match key.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    approval.selected_yes = !approval.selected_yes;
                    None
                }
                KeyCode::Enter => Some(approval.selected_yes),
                KeyCode::Char('y' | 'Y') => Some(true),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(false),
                KeyCode::Char('d' | 'D') => {
                    approval.details_visible = !approval.details_visible;
                    None
                }
                _ => None,
            };
            if let Some(approved) = answer {
                let _ = worker.approve.send(approved);
                self.approval = None;
            }
            return Ok(());
        }
        if self.queue_open && self.editing.is_none() {
            let messages = snapshot();
            match key.code {
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => {
                    self.selected = (self.selected + 1).min(messages.len().saturating_sub(1))
                }
                KeyCode::Delete | KeyCode::Char('d') => remove(self.selected),
                KeyCode::Char('p') => {
                    QUEUE_PAUSED.fetch_xor(true, Ordering::SeqCst);
                }
                KeyCode::Enter | KeyCode::Char('e') if !messages.is_empty() => {
                    self.saved_draft = Some(std::mem::take(&mut self.draft));
                    self.draft.input = messages[self.selected].clone();
                    self.draft.cursor = self.draft.input.chars().count();
                    self.editing = Some(self.selected);
                }
                KeyCode::Esc | KeyCode::F(2) => self.close_queue(),
                _ => {}
            }
            return Ok(());
        }
        match key.code {
            KeyCode::F(2) => {
                if self.queue_open {
                    self.close_queue();
                } else {
                    self.open_queue();
                }
            }
            KeyCode::Esc => {
                if self.editing.is_some() {
                    self.close_queue();
                } else {
                    self.draft = Draft::default();
                }
            }
            KeyCode::Left => {
                self.draft.cursor = self.draft.pastes.snap_cursor(
                    &self.draft.input,
                    self.draft.cursor.saturating_sub(1),
                    false,
                )
            }
            KeyCode::Right => {
                self.draft.cursor = self.draft.pastes.snap_cursor(
                    &self.draft.input,
                    (self.draft.cursor + 1).min(self.draft.input.chars().count()),
                    true,
                )
            }
            KeyCode::Home => self.draft.cursor = 0,
            KeyCode::End => self.draft.cursor = self.draft.input.chars().count(),
            KeyCode::Backspace => {
                self.draft
                    .pastes
                    .delete(&mut self.draft.input, &mut self.draft.cursor, true)
            }
            KeyCode::Delete => {
                self.draft
                    .pastes
                    .delete(&mut self.draft.input, &mut self.draft.cursor, false)
            }
            KeyCode::Up if !command_suggestions(&self.draft.input).is_empty() => {
                self.palette_selected = self.palette_selected.saturating_sub(1)
            }
            KeyCode::Down if !command_suggestions(&self.draft.input).is_empty() => {
                self.palette_selected = (self.palette_selected + 1)
                    .min(command_suggestions(&self.draft.input).len() - 1)
            }
            KeyCode::Tab => {
                if let Some(command) =
                    command_suggestions(&self.draft.input).get(self.palette_selected)
                {
                    self.draft.input = command.to_string();
                    self.draft.cursor = self.draft.input.chars().count();
                }
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                insert_text_at_cursor(&mut self.draft.input, &mut self.draft.cursor, "\n")
            }
            KeyCode::Enter => {
                let mut text = self.draft.pastes.expand(&self.draft.input);
                let suggestions = command_suggestions(&text);
                if self.editing.is_none()
                    && !suggestions.is_empty()
                    && !COMMANDS.iter().any(|(command, _)| *command == text)
                {
                    text =
                        suggestions[self.palette_selected.min(suggestions.len() - 1)].to_string();
                }
                let normalized = text
                    .trim()
                    .strip_prefix('/')
                    .map(|text| format!(":{text}"))
                    .unwrap_or_else(|| text.trim().to_string());
                if let Some(index) = self.editing {
                    replace(index, text)?;
                    self.close_queue();
                    self.status = "Queued message updated".into();
                } else if normalized == ":queue" || normalized == ":queue list" {
                    self.draft = Draft::default();
                    self.open_queue();
                } else if normalized == ":stop" {
                    worker.cancel.store(true, Ordering::SeqCst);
                    let _ = worker.approve.send(false);
                    QUEUE_PAUSED.store(true, Ordering::SeqCst);
                    self.draft = Draft::default();
                } else if normalized.starts_with(":queue ") {
                    self.clear()?;
                    queue_command(&normalized)?;
                    self.draft = Draft::default();
                } else if !text.trim().is_empty() {
                    self.clear()?;
                    enqueue_message(text)?;
                    self.draft = Draft::default();
                    self.status = "Working · message queued".into();
                }
                self.palette_selected = 0;
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                insert_text_at_cursor(
                    &mut self.draft.input,
                    &mut self.draft.cursor,
                    &character.to_string(),
                );
                self.palette_selected = 0;
            }
            _ => {}
        }
        Ok(())
    }
    fn clear(&mut self) -> Result<(), String> {
        clear_input_region(&mut io::stdout(), &mut self.screen)
    }
}

pub async fn run(
    options: &Options,
    model: &str,
    prompt: &str,
    history: &mut Vec<Value>,
) -> Result<Vec<String>, String> {
    let started = Instant::now();
    let mut options = options.clone();
    options.command = "inline-worker".into();
    options.json_output = true;
    options.auto_approve |= load_user_config()?.auto_approve_actions.unwrap_or(false);
    let followups = load_user_config()?.follow_up_suggestions.unwrap_or(false);
    let model = model.to_string();
    let prompt = prompt.to_string();
    let mut saved_history = history.clone();
    let (sender, receiver) = mpsc::channel();
    let (approve, approvals) = mpsc::channel();
    let (answer, answers) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelled = cancel.clone();
    let thread = thread::spawn(move || {
        tui::configure_worker(sender.clone(), approvals, answers, cancelled);
        let result = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime.block_on(async {
                run_agent_turn(&options, &model, &prompt, &mut saved_history).await?;
                if followups {
                    Ok(generate_followup_suggestions(&options, &model, &saved_history).await)
                } else {
                    Ok(Vec::new())
                }
            }),
            Err(error) => Err(error.to_string()),
        };
        let (suggestions, error) = match result {
            Ok(suggestions) => (suggestions, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let _=sender.send(json!({"type":"inline_complete","history":saved_history,"suggestions":suggestions,"error":error}));
    });
    let worker = Worker {
        cancel,
        approve,
        answer,
        thread: Some(thread),
    };
    let mut guard = RawModeGuard::acquire()?;
    let _ = execute!(io::stdout(), EnableBracketedPaste);
    let mut live = Live {
        draft: DRAFT
            .lock()
            .ok()
            .and_then(|mut draft| draft.take())
            .unwrap_or_default(),
        saved_draft: None,
        editing: None,
        queue_open: false,
        selected: 0,
        palette_selected: 0,
        status: "Thinking".into(),
        status_started: Instant::now(),
        last_frame: Instant::now(),
        pending: String::new(),
        formatter: None,
        screen: InputRenderState::default(),
        approval: None,
    };
    write!(io::stdout(), "\r\n").map_err(|e| e.to_string())?;
    live.draw()?;
    let mut complete = None;
    let mut elapsed = Duration::ZERO;
    loop {
        let mut dirty = false;
        for event in receiver.try_iter() {
            dirty = true;
            match event["type"].as_str() {
                Some("text") => live.stream(event["part"]["text"].as_str().unwrap_or_default())?,
                Some("reasoning") => {
                    let text = event["part"]["text"].as_str().unwrap_or("Working");
                    let (kind, message) = text.split_once(": ").unwrap_or(("working", text));
                    let status = if matches!(kind, "thinking" | "exploring" | "working") {
                        message.to_string()
                    } else {
                        format!("[{kind}] {message}")
                    };
                    if live.status != status {
                        live.status = status;
                        live.status_started = Instant::now();
                    }
                }
                Some("status") => {
                    live.status = event["message"].as_str().unwrap_or("Working").to_string();
                }
                Some("tool_use") => {
                    let part = &event["part"];
                    let state = part["state"]["status"].as_str().unwrap_or_default();
                    if matches!(state, "completed" | "error") {
                        live.flush_partial()?;
                        let title = part["state"]["title"].as_str().unwrap_or("tool");
                        if title.trim() == "ask_user" || title.trim() == "request_build_mode" {
                            continue;
                        }
                        let output = part["state"]["output"].as_str().unwrap_or_default();
                        if output.starts_with("Edited ") && output.contains("diff --git") {
                            if let Ok(mut cache) = LAST_EDIT_DETAILS.lock() {
                                *cache = Some(output.into());
                            }
                            live.commit(&compact_edit_view(
                                output,
                                terminal::size()
                                    .map(|(width, _)| width as usize)
                                    .unwrap_or(80),
                            ))?;
                        } else {
                            live.commit(&format!(
                                "{} {title} \x1b[2m({:.1}s)\x1b[0m{}\r\n",
                                if state == "error" {
                                    "\x1b[31m✖\x1b[0m"
                                } else {
                                    "\x1b[32m✔\x1b[0m"
                                },
                                part["state"]["duration"].as_f64().unwrap_or(0.0),
                                if state == "error"
                                    || output.starts_with("No changes")
                                    || output.starts_with("Updated ")
                                {
                                    format!(
                                        " — {}",
                                        truncate(output.lines().next().unwrap_or_default(), 200)
                                    )
                                } else {
                                    String::new()
                                }
                            ))?;
                        }
                    }
                }
                Some("approval") => {
                    let details = event["preview"].as_str().map(str::to_string);
                    live.approval = Some(ApprovalPrompt {
                        action: event["action"].as_str().unwrap_or("action").into(),
                        details_visible: details.is_some(),
                        details,
                        selected_yes: false,
                    });
                }
                Some("question") => {
                    live.flush_partial()?;
                    live.clear()?;
                    guard.release();
                    let answer_result = interactive_question(&event["arguments"]);
                    let _ = worker
                        .answer
                        .send(answer_result.as_ref().ok().cloned().flatten());
                    guard = RawModeGuard::acquire()?;
                    answer_result?;
                    live.draw()?;
                }
                Some("inline_complete") => {
                    elapsed = started.elapsed();
                    *history = event["history"].as_array().cloned().unwrap_or_default();
                    live.flush_partial()?;
                    let error = event["error"].as_str().map(str::to_string);
                    let suggestions = event["suggestions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect();
                    complete = Some(if let Some(error) = error {
                        Err(error)
                    } else {
                        Ok(suggestions)
                    });
                    live.status = "Finished · close Queue to continue".into();
                }
                _ => {}
            }
        }
        if complete.is_some() && !live.queue_open {
            break;
        }
        if dirty || (complete.is_none() && live.last_frame.elapsed() >= Duration::from_millis(80)) {
            live.draw()?;
        }
        if !event::poll(Duration::from_millis(40)).map_err(|e| e.to_string())? {
            continue;
        }
        match event::read().map_err(|e| e.to_string())? {
            Event::Key(key) => {
                if let Err(error) = live.key(key, &worker) {
                    live.status = error;
                }
            }
            Event::Paste(text) => {
                if !live.queue_open || live.editing.is_some() {
                    live.draft
                        .pastes
                        .insert(&mut live.draft.input, &mut live.draft.cursor, &text);
                }
            }
            Event::Resize(_, _) => {}
            _ => continue,
        }
        live.draw()?;
    }
    live.clear()?;
    if !live.draft.input.is_empty() {
        if let Ok(mut draft) = DRAFT.lock() {
            *draft = Some(std::mem::take(&mut live.draft));
        }
    }
    let _ = execute!(io::stdout(), DisableBracketedPaste);
    guard.release();
    drop(worker);
    if complete.as_ref().is_some_and(Result::is_ok) {
        println!("\n✓ Finished ({:.1}s)", elapsed.as_secs_f32());
    }
    complete.unwrap()
}
