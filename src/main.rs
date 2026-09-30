use crossterm::cursor::{MoveDown, MoveTo, MoveToColumn, MoveUp};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

const KILO_BASE_URL: &str = "https://api.kilo.ai/api/gateway";
const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
const PROVIDER_PRESETS: [(&str, &str, &str); 13] = [
    ("openrouter", "OpenRouter", OPENROUTER_BASE_URL),
    ("orca", "Orca", "https://orca-ai.net/v1"),
    ("aihubmix", "AIHubMix", "https://aihubmix.com/v1"),
    ("groq", "Groq", "https://api.groq.com/openai/v1"),
    ("cerebras", "Cerebras", "https://api.cerebras.ai/v1"),
    (
        "gemini",
        "Google Gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
    ),
    ("deepseek", "DeepSeek", "https://api.deepseek.com"),
    ("together", "Together AI", "https://api.together.ai/v1"),
    (
        "fireworks",
        "Fireworks AI",
        "https://api.fireworks.ai/inference/v1",
    ),
    ("mistral", "Mistral AI", "https://api.mistral.ai/v1"),
    (
        "siliconflow",
        "SiliconFlow",
        "https://api.siliconflow.com/v1",
    ),
    ("claude", "Anthropic Claude", "https://api.anthropic.com/v1"),
    ("codex", "OpenAI Codex", "https://api.openai.com/v1"),
];

fn provider_free_label(id: &str) -> Option<&'static str> {
    match id {
        "openrouter" | "aihubmix" => Some("free"),
        _ => None,
    }
}

static CTRL_C_COUNT: AtomicUsize = AtomicUsize::new(0);
static SESSION_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
static RAW_TTY_MODE: AtomicBool = AtomicBool::new(false);
const TURN_INTERRUPTED: &str = "nio: turn interrupted";

#[derive(Debug)]
struct Options {
    command: String,
    prompt: Vec<String>,
    model: Option<String>,
    base_url: String,
    api_key: Option<String>,
    json_output: bool,
    auto_approve: bool,
    workdir: Option<PathBuf>,
    session_id: Option<String>,
    project_trusted: bool,
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCall>,
}

#[derive(Deserialize)]
struct StreamToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: StreamFunctionCall,
}

#[derive(Deserialize)]
#[serde(default)]
struct StreamFunctionCall {
    name: String,
    arguments: String,
}

impl Default for StreamFunctionCall {
    fn default() -> Self {
        Self {
            name: String::new(),
            arguments: String::new(),
        }
    }
}

#[derive(Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

struct AssistantToolCall {
    id: String,
    name: String,
    arguments: Value,
}

struct Spinner {
    task: Option<tokio::task::JoinHandle<()>>,
    started: Option<Instant>,
}

impl Spinner {
    fn start(options: &Options) -> Self {
        Self::start_with_message(options, "Thinking")
    }

    fn start_with_message(options: &Options, message: &str) -> Self {
        if options.json_output || !io::stderr().is_terminal() {
            emit_status(options, "thinking", message);
            return Self {
                task: None,
                started: None,
            };
        }

        let started = Instant::now();
        let message = message.to_string();
        let task = tokio::spawn(async move {
            let frames = [".", "..", "..."];
            let mut frame = 0;
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                ticker.tick().await;
                eprint!(
                    "\r\x1b[2K🔹 [thinking] {}{} ({}s)",
                    message,
                    frames[frame],
                    started.elapsed().as_secs()
                );
                let _ = io::stderr().flush();
                frame = (frame + 1) % frames.len();
            }
        });
        Self {
            task: Some(task),
            started: Some(started),
        }
    }

    fn stop(&mut self) {
        self.stop_with_spacing(false);
    }

    fn pause(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            eprint!("\r\x1b[2K");
            let _ = io::stderr().flush();
        }
    }

    fn stop_with_spacing(&mut self, blank_before_finished: bool) {
        self.pause();
        if let Some(started) = self.started.take() {
            let newline = if RAW_TTY_MODE.load(Ordering::SeqCst) {
                "\r\n"
            } else {
                "\n"
            };
            if blank_before_finished {
                eprint!("{newline}");
            }
            eprint!(
                "🔹 [thinking] Finished ({}s){newline}",
                started.elapsed().as_secs()
            );
            let _ = io::stderr().flush();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

struct EscapeInterrupt {
    cancelled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    terminal_available: bool,
}

impl EscapeInterrupt {
    fn new() -> Self {
        let mut interrupt = Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            listener: None,
            terminal_available: io::stdin().is_terminal() && io::stderr().is_terminal(),
        };
        interrupt.resume();
        interrupt
    }

    fn resume(&mut self) {
        if !self.terminal_available
            || self.cancelled.load(Ordering::SeqCst)
            || self.listener.is_some()
        {
            return;
        }
        self.stop.store(false, Ordering::SeqCst);
        let stop = self.stop.clone();
        let cancelled = self.cancelled.clone();
        let listener = thread::spawn(move || {
            if terminal::enable_raw_mode().is_err() {
                return;
            }
            RAW_TTY_MODE.store(true, Ordering::SeqCst);
            let mut previous_escape = None::<Instant>;
            while !stop.load(Ordering::SeqCst) {
                if !event::poll(Duration::from_millis(80)).unwrap_or(false) {
                    continue;
                }
                let Ok(Event::Key(key)) = event::read() else {
                    continue;
                };
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                match key.code {
                    KeyCode::Esc => {
                        let now = Instant::now();
                        if previous_escape.is_some_and(|last| {
                            now.duration_since(last) <= Duration::from_millis(1200)
                        }) {
                            cancelled.store(true, Ordering::SeqCst);
                            eprint!("\r\n🔹 [interrupt] Stopping the current response.\r\n");
                            let _ = io::stderr().flush();
                            break;
                        }
                        previous_escape = Some(now);
                        eprint!("\r\n🔹 [interrupt] Press Esc again to stop.\r\n");
                        let _ = io::stderr().flush();
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let count = CTRL_C_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
                        if count >= 2 {
                            cancelled.store(true, Ordering::SeqCst);
                            eprint!("\r\n🔹 [interrupt] Stopping Nio.\r\n");
                            let _ = io::stderr().flush();
                            break;
                        }
                        eprint!("\r\n🔹 [interrupt] Press Ctrl+C again to exit.\r\n");
                        let _ = io::stderr().flush();
                    }
                    _ => {
                        previous_escape = None;
                    }
                }
            }
            let _ = terminal::disable_raw_mode();
            RAW_TTY_MODE.store(false, Ordering::SeqCst);
        });
        self.listener = Some(listener);
    }

    fn pause(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
    }

    fn with_terminal_input<T>(
        &mut self,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.pause();
        let result = action();
        self.resume();
        result
    }
}

impl Drop for EscapeInterrupt {
    fn drop(&mut self) {
        self.pause();
    }
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelInfo>,
}

#[derive(Deserialize)]
struct ModelInfo {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    pricing: Option<ModelPricing>,
    #[serde(default)]
    free: Option<bool>,
}

#[derive(Deserialize)]
struct ModelPricing {
    prompt: Option<serde_json::Value>,
    completion: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Default)]
struct UserConfig {
    default_model: Option<String>,
    #[serde(default)]
    agent_mode: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    auto_approve_actions: Option<bool>,
    #[serde(default)]
    request_interval_seconds: Option<u64>,
    #[serde(default)]
    follow_up_suggestions: Option<bool>,
    #[serde(default)]
    prompt_history: Vec<String>,
    #[serde(default)]
    proxy_url: Option<String>,
    #[serde(default)]
    providers: Vec<ProviderConfig>,
    #[serde(default)]
    trusted_folders: Vec<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ProviderConfig {
    id: String,
    name: String,
    base_url: String,
    #[serde(default)]
    api_key: Option<String>,
}

const DEFAULT_REQUEST_INTERVAL_SECONDS: u64 = 2;
const DEFAULT_AGENT_MODE: &str = "build";

struct ModelChoice {
    id: String,
    name: String,
    gateway: String,
    gateway_label: String,
    free: bool,
}

impl ModelChoice {
    fn selector(&self) -> String {
        format!("{}::{}", self.gateway, self.id)
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("nio: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let mut options = parse_args(env::args().skip(1).collect())?;
    if matches!(options.command.as_str(), "interactive" | "run") {
        options.project_trusted = confirm_project_trust(&options)?;
    }
    match options.command.as_str() {
        "help" => {
            print_help();
            Ok(())
        }
        "version" => {
            println!("nio {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "interactive" => interactive(options).await,
        "models" => list_models(&options).await,
        "provider" => configure_provider().await,
        "run" => chat(&options).await,
        command => Err(format!("unknown command '{command}'. Run 'nio --help'.")),
    }
}

fn confirm_project_trust(options: &Options) -> Result<bool, String> {
    let requested_root = options.workdir.as_deref().unwrap_or(Path::new("."));
    let root = requested_root.canonicalize().map_err(|error| {
        format!(
            "resolving project directory '{}': {error}",
            requested_root.display()
        )
    })?;
    if !root.is_dir() {
        return Err(format!(
            "project path '{}' is not a directory",
            root.display()
        ));
    }

    let mut config = load_user_config()?;
    if config.trusted_folders.iter().any(|path| path == &root) {
        return Ok(true);
    }

    if !io::stdin().is_terminal() {
        eprintln!(
            "Project folder is not trusted; running without project tools. Run nio in a terminal to review and trust it."
        );
        return Ok(false);
    }

    println!("Trust this project folder?\n  {}", root.display());
    println!(
        "Trust allows Nio to read project files. Changes and commands still follow approval settings."
    );
    print!("[y] Trust  [N] No trust: ");
    io::stdout()
        .flush()
        .map_err(|error| format!("writing trust prompt: {error}"))?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("reading trust choice: {error}"))?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        config.trusted_folders.push(root);
        save_user_config(&config)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn parse_args(args: Vec<String>) -> Result<Options, String> {
    let mut args = args.into_iter();
    let first = args.next();
    let first_is_session_option = matches!(first.as_deref(), Some("-s" | "--session"));
    let command = match first.as_deref() {
        None => "interactive".to_string(),
        Some("--help") | Some("-h") | Some("help") => "help".to_string(),
        Some("--version") | Some("-V") | Some("--v") | Some("-v") => {
            return Ok(Options {
                command: "version".to_string(),
                prompt: vec![],
                model: None,
                base_url: KILO_BASE_URL.to_string(),
                api_key: None,
                json_output: false,
                auto_approve: false,
                workdir: None,
                session_id: None,
                project_trusted: false,
            });
        }
        Some("run") => "run".to_string(),
        Some("models") => "models".to_string(),
        Some("provider") => "provider".to_string(),
        Some("-s" | "--session") => "interactive".to_string(),
        Some(prompt) => {
            let mut all = vec![prompt.to_string()];
            all.extend(args);
            return Ok(Options {
                command: "run".to_string(),
                prompt: all,
                model: env::var("NIO_MODEL").ok(),
                base_url: env::var("NIO_BASE_URL").unwrap_or_else(|_| KILO_BASE_URL.into()),
                api_key: env::var("NIO_API_KEY").ok(),
                json_output: false,
                auto_approve: false,
                workdir: None,
                session_id: None,
                project_trusted: false,
            });
        }
    };
    let mut args = if first_is_session_option {
        std::iter::once(first.expect("session option was present"))
            .chain(args)
            .collect::<Vec<_>>()
            .into_iter()
    } else {
        args
    };

    let mut prompt = Vec::new();
    let mut model = env::var("NIO_MODEL").ok();
    let mut base_url_override = env::var("NIO_BASE_URL").ok();
    let mut base_url = base_url_override
        .clone()
        .unwrap_or_else(|| KILO_BASE_URL.into());
    let mut api_key = env::var("NIO_API_KEY").ok();
    let mut json_output = false;
    let mut auto_approve = false;
    let mut workdir = None;
    let mut session_id = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" | "--v" | "-v" => {
                return Ok(Options {
                    command: "version".into(),
                    prompt: vec![],
                    model: None,
                    base_url: KILO_BASE_URL.into(),
                    api_key: None,
                    json_output: false,
                    auto_approve: false,
                    workdir: None,
                    session_id: None,
                    project_trusted: false,
                });
            }
            "--model" | "-m" => model = Some(args.next().ok_or("--model requires a value")?),
            "--base-url" => {
                base_url = args.next().ok_or("--base-url requires a value")?;
                base_url_override = Some(base_url.clone());
            }
            "--api-key" => api_key = Some(args.next().ok_or("--api-key requires a value")?),
            "--all" => {}
            "--format" => {
                let format = args.next().ok_or("--format requires a value")?;
                match format.as_str() {
                    "json" => json_output = true,
                    "text" | "human" => json_output = false,
                    _ => return Err("--format must be 'json' or 'text'".into()),
                }
            }
            "--dir" => workdir = Some(PathBuf::from(args.next().ok_or("--dir requires a path")?)),
            "--auto" => auto_approve = true,
            "--pure" => {}
            "--variant" => {
                let _ = args.next().ok_or("--variant requires a value")?;
            }
            "-s" | "--session" => {
                let id = args.next().ok_or("--session requires a value")?;
                if id.is_empty() {
                    return Err("--session must not be empty".into());
                }
                session_id = Some(id);
            }
            "--help" | "-h" => {
                return Ok(Options {
                    command: "help".into(),
                    prompt,
                    model,
                    base_url,
                    api_key,
                    json_output,
                    auto_approve,
                    workdir,
                    session_id,
                    project_trusted: false,
                });
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option '{arg}'")),
            _ => prompt.push(arg),
        }
    }

    if let Some(override_url) = base_url_override {
        base_url = override_url;
    }

    Ok(Options {
        command,
        prompt,
        model,
        base_url,
        api_key,
        json_output,
        auto_approve,
        workdir,
        session_id,
        project_trusted: false,
    })
}

async fn chat(options: &Options) -> Result<(), String> {
    let model = chosen_model(options).await?;
    let mut prompt = options.prompt.join(" ");
    if prompt.trim().is_empty() {
        print!("Prompt: ");
        io::stdout()
            .flush()
            .map_err(|e| format!("writing prompt: {e}"))?;
        io::stdin()
            .read_line(&mut prompt)
            .map_err(|e| format!("reading prompt: {e}"))?;
        prompt = prompt.trim_end().to_string();
        if prompt.trim().is_empty() {
            return Err("no prompt entered".into());
        }
    }
    let session_id = options
        .session_id
        .clone()
        .or_else(|| (!options.json_output).then(generate_session_id));
    let mut history = load_session_history(session_id.as_deref())?;
    match run_agent_turn(options, &model, &prompt, &mut history).await {
        Err(error) if error == TURN_INTERRUPTED => {
            println!("\nInterrupted.");
            Ok(())
        }
        Ok(suggestions) => {
            save_session_history(session_id.as_deref(), &history)?;
            if !options.json_output {
                if !suggestions.is_empty() {
                    println!("\nSuggested follow-ups:");
                    for (index, suggestion) in suggestions.iter().enumerate() {
                        println!("  {}) {suggestion}", index + 1);
                    }
                }
                println!(
                    "\nSession saved. Continue with: nio run -s {} -m {} \"your next prompt\"",
                    shell_quote(session_id.as_deref().unwrap_or_default()),
                    shell_quote(&model)
                );
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn agent_tools(mode: &str) -> Value {
    let tools = json!([
        {"type":"function","function":{"name":"list_files","description":"List files under a project directory.","parameters":{"type":"object","properties":{"path":{"type":"string","description":"Project-relative directory, default ."}},"additionalProperties":false}}},
        {"type":"function","function":{"name":"read_file","description":"Read a UTF-8 text file or a line range from it. For long files, read subsequent sections with start_line so you do not repeat the first section.","parameters":{"type":"object","properties":{"path":{"type":"string"},"start_line":{"type":"integer","minimum":1,"description":"1-based first line to return; defaults to 1"},"line_count":{"type":"integer","minimum":1,"maximum":300,"description":"Maximum lines to return; defaults to 200"}},"required":["path"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"search_files","description":"Search project text files for a literal string.","parameters":{"type":"object","properties":{"query":{"type":"string"},"path":{"type":"string","description":"Optional project-relative directory, default ."}},"required":["query"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"write_file","description":"Create or replace a project file. Approval depends on Nio settings.","parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"run_command","description":"Run a shell command in the project. Approval depends on Nio settings.","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"],"additionalProperties":false}}}
    ]);
    let Some(tools) = tools.as_array() else {
        return json!([]);
    };
    Value::Array(
        tools
            .iter()
            .filter(|tool| {
                mode_allows_changes(mode)
                    || tool["function"]["name"] != "write_file"
                        && tool["function"]["name"] != "run_command"
            })
            .cloned()
            .collect(),
    )
}

fn mode_allows_changes(mode: &str) -> bool {
    mode == "build"
}

fn configured_agent_mode(config: &UserConfig) -> &str {
    match config.agent_mode.as_deref() {
        Some("ask") => "ask",
        Some("plan") => "plan",
        Some("build") => "build",
        _ => DEFAULT_AGENT_MODE,
    }
}

fn project_overview(root: &Path) -> String {
    let mut files = Vec::new();
    collect_files(root, root, 0, &mut files, 100);
    let mut output = format!(
        "Root: {}\nFiles (partial listing):\n{}",
        root.display(),
        files.join("\n")
    );
    for name in [
        "README.md",
        "AGENTS.md",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
    ] {
        let path = root.join(name);
        if let Ok(metadata) = std::fs::metadata(&path) {
            if metadata.is_file() && metadata.len() <= 12 * 1024 {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    output.push_str(&format!("\n\n--- {name} ---\n{contents}"));
                }
            }
        }
    }
    truncate(&output, 20_000)
}

fn collect_files(root: &Path, dir: &Path, depth: usize, output: &mut Vec<String>, limit: usize) {
    if depth > 8 || output.len() >= limit {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if output.len() >= limit {
            break;
        }
        let name = entry.file_name();
        if is_ignored_path(&name.to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_files(root, &path, depth + 1, output, limit);
        } else if kind.is_file() {
            if let Ok(relative) = path.strip_prefix(root) {
                output.push(relative.display().to_string());
            }
        }
    }
}

fn is_ignored_path(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".git"
        || lower == "target"
        || lower == "node_modules"
        || lower == "vendor"
        || lower == ".venv"
        || lower == "dist"
        || lower == "build"
        || lower == ".next"
        || lower.starts_with(".env")
        || lower == ".ssh"
        || lower == ".aws"
        || lower == "auth.json"
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower == "id_rsa"
        || lower == "id_ed25519"
}

struct MarkdownFormatter {
    enabled: bool,
    pending: String,
    bold: bool,
    wrap_width: usize,
    column: usize,
}

impl MarkdownFormatter {
    fn new(enabled: bool) -> Self {
        let wrap_width = if enabled {
            terminal::size()
                .map(|(width, _)| width as usize)
                .unwrap_or(80)
                .max(20)
        } else {
            usize::MAX
        };
        Self {
            enabled,
            pending: String::new(),
            bold: false,
            wrap_width,
            column: 6,
        }
    }

    fn push(&mut self, text: &str) -> String {
        if !self.enabled {
            return text.to_string();
        }
        self.pending.push_str(text);
        self.drain(false)
    }

    fn finish(&mut self) -> String {
        if !self.enabled {
            return std::mem::take(&mut self.pending);
        }
        let mut output = self.drain(true);
        if self.bold {
            output.push_str("\x1b[22m");
            self.bold = false;
        }
        output
    }

    fn drain(&mut self, flush_partial: bool) -> String {
        let mut output = String::new();
        while !self.pending.is_empty() {
            if self.pending.starts_with("**") {
                self.pending.drain(..2);
                self.bold = !self.bold;
                output.push_str(if self.bold { "\x1b[1m" } else { "\x1b[22m" });
                continue;
            }
            if !flush_partial && self.pending == "*" {
                break;
            }
            let character = self.pending.remove(0);
            if character == '\n' {
                output.push(character);
                self.column = 6;
                continue;
            }
            let width = terminal_character_width(character);
            if width > 0 && self.column.saturating_add(width) >= self.wrap_width {
                output.push('\n');
                self.column = 6;
            }
            output.push(character);
            self.column = self.column.saturating_add(width);
        }
        output
    }
}

fn terminal_character_width(character: char) -> usize {
    let code = character as u32;
    if character.is_control()
        || matches!(code, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0xFE00..=0xFE0F)
    {
        return 0;
    }
    if matches!(
        code,
        0x1100..=0x115F
            | 0x2329..=0x232A
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE6F
            | 0xFF00..=0xFF60
            | 0x1F300..=0x1FAFF
    ) {
        2
    } else {
        1
    }
}

fn compact_tool_messages(messages: &mut [Value]) {
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let Some(content) = message.get("content").and_then(Value::as_str) else {
            continue;
        };
        if content.chars().count() > 1_600 {
            message["content"] = json!(format!(
                "{}\n[Tool output shortened to make room for the final response.]",
                truncate(content, 1_600)
            ));
        }
    }
}

fn process_sse_line(
    line: &str,
    options: &Options,
    answer: &mut String,
    tools: &mut std::collections::BTreeMap<usize, PendingToolCall>,
    response_started: &mut bool,
    formatter: &mut MarkdownFormatter,
) -> Result<(), String> {
    let Some(data) = line.strip_prefix("data:").map(str::trim) else {
        return Ok(());
    };
    if data == "[DONE]" || data.is_empty() {
        return Ok(());
    }
    let chunk: StreamChunk =
        serde_json::from_str(data).map_err(|e| format!("invalid model stream event: {e}"))?;
    for choice in chunk.choices {
        if let Some(content) = choice.delta.content {
            if !content.is_empty() && !*response_started {
                emit_assistant_start(options)?;
                *response_started = true;
            }
            answer.push_str(&content);
            let formatted = formatter.push(&content);
            if !formatted.is_empty() {
                emit_text(options, &formatted)?;
            }
        }
        for partial in choice.delta.tool_calls {
            let call = tools.entry(partial.index).or_default();
            if let Some(id) = partial.id {
                call.id.push_str(&id);
            }
            call.name.push_str(&partial.function.name);
            call.arguments.push_str(&partial.function.arguments);
        }
    }
    Ok(())
}

fn process_json_completion(
    payload: Value,
    options: &Options,
    answer: &mut String,
    tools: &mut std::collections::BTreeMap<usize, PendingToolCall>,
    response_started: &mut bool,
    formatter: &mut MarkdownFormatter,
) -> Result<(), String> {
    let Some(choice) = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|x| x.first())
    else {
        return Err("provider returned a JSON response without a completion choice".into());
    };
    if let Some(content) = choice
        .pointer("/message/content")
        .and_then(Value::as_str)
        .or_else(|| choice.get("text").and_then(Value::as_str))
    {
        if !content.is_empty() {
            emit_assistant_start(options)?;
            *response_started = true;
            answer.push_str(content);
            let formatted = formatter.push(content);
            if !formatted.is_empty() {
                emit_text(options, &formatted)?;
            }
        }
    }
    if let Some(calls) = choice
        .pointer("/message/tool_calls")
        .and_then(Value::as_array)
    {
        for (index, call) in calls.iter().enumerate() {
            let function = call.get("function").unwrap_or(&Value::Null);
            let arguments = match function.get("arguments") {
                Some(Value::String(arguments)) => serde_json::from_str(arguments)
                    .unwrap_or_else(|_| json!({"_invalid_arguments": arguments})),
                Some(arguments) => arguments.clone(),
                None => json!({}),
            };
            tools.insert(
                index,
                PendingToolCall {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("json-tool-{index}")),
                    name: function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: arguments.to_string(),
                    ..PendingToolCall::default()
                },
            );
        }
    }
    Ok(())
}

fn emit_assistant_start(options: &Options) -> Result<(), String> {
    if !options.json_output {
        let newline = if RAW_TTY_MODE.load(Ordering::SeqCst) {
            "\r\n"
        } else {
            "\n"
        };
        print!("{newline}🔹 🤖 nio:{newline}      ");
        io::stdout()
            .flush()
            .map_err(|e| format!("writing response label: {e}"))?;
    }
    Ok(())
}

fn emit_status(options: &Options, status: &str, message: &str) {
    if options.json_output {
        // NoIDE understands this OpenCode/Kilo-compatible reasoning event.
        emit_json(
            &json!({"type":"reasoning","part":{"type":"reasoning","text":format!("{status}: {message}")}}),
        );
    } else {
        let newline = if RAW_TTY_MODE.load(Ordering::SeqCst) {
            "\r\n"
        } else {
            "\n"
        };
        eprint!("🔹 [{status}] {message}{newline}");
    }
}

fn emit_text(options: &Options, text: &str) -> Result<(), String> {
    if options.json_output {
        emit_json(&json!({"type":"text","part":{"type":"text","text":text}}));
    } else {
        let mut stdout = io::stdout().lock();
        if RAW_TTY_MODE.load(Ordering::SeqCst) {
            stdout
                .write_all(&indent_response_lines(text, "\r\n"))
                .map_err(|e| format!("writing response: {e}"))?;
        } else {
            stdout
                .write_all(&indent_response_lines(text, "\n"))
                .map_err(|e| format!("writing response: {e}"))?;
        }
        stdout
            .flush()
            .map_err(|e| format!("writing response: {e}"))?;
    }
    Ok(())
}

fn emit_json(value: &Value) {
    let newline = if RAW_TTY_MODE.load(Ordering::SeqCst) {
        "\r\n"
    } else {
        "\n"
    };
    print!("{value}{newline}");
}

fn indent_response_lines(text: &str, newline: &str) -> Vec<u8> {
    let mut output =
        Vec::with_capacity(text.len() + text.matches('\n').count() * (newline.len() + 6));
    let mut previous_was_cr = false;
    for byte in text.bytes() {
        if byte == b'\n' && !previous_was_cr {
            output.extend_from_slice(newline.as_bytes());
            output.extend_from_slice(b"      ");
            previous_was_cr = false;
            continue;
        } else if byte == b'\n' {
            output.extend_from_slice(b"      ");
            previous_was_cr = false;
            continue;
        }
        output.push(byte);
        previous_was_cr = byte == b'\r';
    }
    output
}

fn emit_tool_event(
    options: &Options,
    call: &AssistantToolCall,
    status: &str,
    input: &Value,
    output: Option<&str>,
) {
    if !options.json_output {
        return;
    }
    emit_json(
        &json!({"type":"tool_use","part":{"type":"tool","callID":call.id,"tool":call.name,"state":{"status":status,"input":input,"output":output,"title":format!("{} {}",call.name,tool_hint(&call.name,input))}}}),
    );
}

fn tool_hint(name: &str, args: &Value) -> String {
    match name {
        "read_file" | "list_files" => args.get("path").and_then(Value::as_str).unwrap_or("."),
        "search_files" => args.get("query").and_then(Value::as_str).unwrap_or(""),
        "write_file" => args.get("path").and_then(Value::as_str).unwrap_or(""),
        "run_command" => args.get("command").and_then(Value::as_str).unwrap_or(""),
        _ => "",
    }
    .to_string()
}

async fn execute_agent_tool(
    root: &Path,
    call: &AssistantToolCall,
    auto_approve: bool,
    interrupt: &mut EscapeInterrupt,
) -> Result<String, String> {
    let args = &call.arguments;
    match call.name.as_str() {
        "list_files" => {
            let input = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let dir = resolve_project_path(root, input, true)?;
            if !dir.is_dir() {
                return Err(format!("'{}' is not a directory", input));
            }
            let mut files = Vec::new();
            collect_files(root, &dir, 0, &mut files, 200);
            Ok(files.join("\n"))
        }
        "read_file" => {
            let input = required_arg(args, "path")?;
            let path = resolve_project_path(root, input, true)?;
            if is_excluded_project_path(root, &path) {
                return Err("file is excluded from automatic project access".into());
            }
            let metadata =
                std::fs::metadata(&path).map_err(|e| format!("reading file metadata: {e}"))?;
            if !metadata.is_file() {
                return Err("path is not a regular file".into());
            }
            if metadata.len() > 512 * 1024 {
                return Err("file is larger than the 512 KiB read limit".into());
            }
            let contents = std::fs::read_to_string(&path)
                .map_err(|e| format!("file is not readable UTF-8 text: {e}"))?;
            let lines = contents.lines().collect::<Vec<_>>();
            if lines.is_empty() {
                return Ok(format!("File '{input}' is empty."));
            }
            let start = args
                .get("start_line")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .saturating_sub(1) as usize;
            let requested_count = args
                .get("line_count")
                .and_then(Value::as_u64)
                .unwrap_or(200)
                .clamp(1, 300) as usize;
            if start >= lines.len() {
                return Err(format!(
                    "start_line {} is past the end of this file ({} lines)",
                    start + 1,
                    lines.len()
                ));
            }
            let mut excerpt = String::new();
            let mut end = start;
            for (index, line) in lines.iter().enumerate().skip(start).take(requested_count) {
                let row = format!("{line}\n");
                if excerpt.len() + row.len() > 9_000 {
                    break;
                }
                excerpt.push_str(&row);
                end = index + 1;
            }
            let mut result = format!(
                "Lines {}-{} of {} in {}:\n{}",
                start + 1,
                end,
                lines.len(),
                input,
                excerpt
            );
            if end < lines.len() {
                result.push_str(&format!(
                    "\n[More lines available. Read the next section with start_line: {}.]",
                    end + 1
                ));
            }
            Ok(result)
        }
        "search_files" => {
            let query = required_arg(args, "query")?;
            if query.is_empty() {
                return Err("search query must not be empty".into());
            }
            let input = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let dir = resolve_project_path(root, input, true)?;
            let mut files = Vec::new();
            collect_files(root, &dir, 0, &mut files, 1000);
            let needle = query.to_lowercase();
            let mut matches = Vec::new();
            for file in files {
                if matches.len() >= 50 {
                    break;
                }
                let path = root.join(&file);
                let Ok(metadata) = std::fs::metadata(&path) else {
                    continue;
                };
                if metadata.len() > 512 * 1024 {
                    continue;
                }
                let Ok(contents) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (line_no, line) in contents.lines().enumerate() {
                    if line.to_lowercase().contains(&needle) {
                        matches.push(format!("{file}:{}: {}", line_no + 1, line.trim()));
                        if matches.len() >= 50 {
                            break;
                        }
                    }
                }
            }
            Ok(if matches.is_empty() {
                "No matches found.".into()
            } else {
                matches.join("\n")
            })
        }
        "write_file" => {
            let input = required_arg(args, "path")?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or("missing string argument 'content'")?;
            if content.len() > 512 * 1024 {
                return Err("file content is larger than the 512 KiB write limit".into());
            }
            let path = resolve_project_path(root, input, false)?;
            if is_excluded_project_path(root, &path) {
                return Err("file is excluded from automatic project access".into());
            }
            if !interrupt.with_terminal_input(|| {
                confirm_tool(
                    auto_approve,
                    &format!("Write {} ({} bytes)", path.display(), content.len()),
                )
            })? {
                return Err("user denied file write".into());
            }
            std::fs::write(&path, content).map_err(|e| format!("writing file: {e}"))?;
            Ok(format!(
                "Wrote {} ({} bytes)",
                path.display(),
                content.len()
            ))
        }
        "run_command" => {
            let command = required_arg(args, "command")?;
            if !interrupt.with_terminal_input(|| {
                confirm_tool(auto_approve, &format!("Run command: {command}"))
            })? {
                return Err("user denied command".into());
            }
            let mut child = tokio::process::Command::new("sh")
                .arg("-lc")
                .arg(command)
                .current_dir(root)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| format!("starting command: {e}"))?;
            let stdout = child
                .stdout
                .take()
                .ok_or("capturing command stdout failed")?;
            let stderr = child
                .stderr
                .take()
                .ok_or("capturing command stderr failed")?;
            let wait = async {
                let (stdout, stderr, status) = tokio::join!(
                    read_capped(stdout, 64 * 1024),
                    read_capped(stderr, 64 * 1024),
                    child.wait()
                );
                Ok::<_, String>((
                    stdout.map_err(|e| format!("reading command stdout: {e}"))?,
                    stderr.map_err(|e| format!("reading command stderr: {e}"))?,
                    status.map_err(|e| format!("waiting for command: {e}"))?,
                ))
            };
            let (stdout, stderr, status) =
                tokio::time::timeout(std::time::Duration::from_secs(120), wait)
                    .await
                    .map_err(|_| "command timed out after 120 seconds".to_string())??;
            Ok(truncate(
                &format!(
                    "exit code: {}\nstdout:\n{}\nstderr:\n{}",
                    status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr)
                ),
                12_000,
            ))
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    Ok(output)
}

fn required_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument '{name}'"))
}

fn resolve_project_path(root: &Path, input: &str, must_exist: bool) -> Result<PathBuf, String> {
    let requested = Path::new(input);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let resolved = if must_exist || candidate.exists() {
        candidate
            .canonicalize()
            .map_err(|e| format!("resolving '{}': {e}", input))?
    } else {
        let parent = candidate.parent().ok_or("path has no parent directory")?;
        let parent = parent
            .canonicalize()
            .map_err(|e| format!("resolving parent of '{}': {e}", input))?;
        let name = candidate.file_name().ok_or("path has no filename")?;
        parent.join(name)
    };
    if !resolved.starts_with(root) {
        return Err("path must stay inside the project directory".into());
    }
    Ok(resolved)
}

fn is_excluded_project_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).ok().is_some_and(|relative| {
        relative.components().any(|component| {
            let name = component.as_os_str().to_string_lossy();
            is_ignored_path(&name)
        })
    })
}

fn confirm_tool(auto_approve: bool, action: &str) -> Result<bool, String> {
    if auto_approve {
        return Ok(true);
    }
    loop {
        if load_user_config()?.auto_approve_actions.unwrap_or(false) {
            return Ok(true);
        }
        eprint!(
            "\nApprove {action}? [y/N; :approval or /approval enables auto-approve; Ctrl+C exits] "
        );
        io::stderr()
            .flush()
            .map_err(|error| format!("writing approval prompt: {error}"))?;
        let Some(answer) = read_approval_line()? else {
            CTRL_C_COUNT.store(2, Ordering::SeqCst);
            return Err(TURN_INTERRUPTED.into());
        };
        match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            command => {
                let command = command
                    .strip_prefix(':')
                    .or_else(|| command.strip_prefix('/'));
                match command {
                    Some("approval") => toggle_auto_approval()?,
                    Some("setting" | "settings") => configure_settings()?,
                    Some("help") => {
                        eprintln!("Commands: :approval (or /approval), :setting (or /setting)");
                    }
                    _ => eprintln!("Enter y or n, or use :approval / :setting (slash also works)."),
                }
            }
        }
    }
}

fn read_approval_line() -> Result<Option<String>, String> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|error| format!("reading approval: {error}"))?;
        return Ok(Some(answer));
    }

    terminal::enable_raw_mode().map_err(|error| format!("enabling approval input: {error}"))?;
    RAW_TTY_MODE.store(true, Ordering::SeqCst);
    let result = (|| {
        let mut answer = String::new();
        loop {
            let event = event::read().map_err(|error| format!("reading approval: {error}"))?;
            let Event::Key(key) = event else { continue };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::Enter => {
                    eprint!("\r\n");
                    io::stderr()
                        .flush()
                        .map_err(|error| format!("finishing approval input: {error}"))?;
                    return Ok(Some(answer));
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    eprint!("^C\r\n");
                    io::stderr()
                        .flush()
                        .map_err(|error| format!("showing approval interrupt: {error}"))?;
                    return Ok(None);
                }
                KeyCode::Esc => {
                    eprint!("\r\n");
                    return Ok(Some(String::new()));
                }
                KeyCode::Backspace => {
                    if answer.pop().is_some() {
                        eprint!("\x08 \x08");
                        io::stderr()
                            .flush()
                            .map_err(|error| format!("updating approval input: {error}"))?;
                    }
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    answer.push(character);
                    eprint!("{character}");
                    io::stderr()
                        .flush()
                        .map_err(|error| format!("updating approval input: {error}"))?;
                }
                _ => {}
            }
        }
    })();
    let restore = terminal::disable_raw_mode();
    RAW_TTY_MODE.store(false, Ordering::SeqCst);
    restore.map_err(|error| format!("restoring terminal input: {error}"))?;
    result
}

async fn run_agent_turn(
    options: &Options,
    model: &str,
    prompt: &str,
    history: &mut Vec<Value>,
) -> Result<Vec<String>, String> {
    let mut interrupt = EscapeInterrupt::new();
    let cancelled = interrupt.cancelled.clone();
    let result = tokio::select! {
        result = async {
            run_agent_turn_inner(options, model, prompt, history, &mut interrupt).await?;
            if options.json_output
                || !load_user_config()?
                    .follow_up_suggestions
                    .unwrap_or(true)
            {
                Ok(Vec::new())
            } else {
                Ok(generate_followup_suggestions(options, model, history).await)
            }
        } => result,
        _ = wait_for_interrupt(cancelled) => Err(TURN_INTERRUPTED.into()),
    };
    interrupt.pause();
    result
}

async fn generate_followup_suggestions(
    options: &Options,
    model: &str,
    history: &[Value],
) -> Vec<String> {
    match request_followup_suggestions(options, model, history).await {
        Ok(suggestions) => suggestions,
        Err(_error) => Vec::new(),
    }
}

async fn request_followup_suggestions(
    options: &Options,
    model: &str,
    history: &[Value],
) -> Result<Vec<String>, String> {
    let (gateway, model_id) = split_model_selector(model)?;
    let (base_url, key) = resolve_model_provider(options, gateway, model_id)?;

    let mut context = history
        .iter()
        .rev()
        .filter_map(|message| {
            let role = message.get("role")?.as_str()?;
            let content = message.get("content")?.as_str()?;
            matches!(role, "user" | "assistant")
                .then(|| json!({"role":role,"content":truncate(content, 1800)}))
        })
        .take(6)
        .collect::<Vec<_>>();
    context.reverse();
    let mut messages = vec![json!({
        "role":"system",
        "content":"Suggest two or three concise next-step prompts based specifically on the latest user request and assistant answer. Each must refer to details from this conversation and offer a distinct action; do not use generic prompts such as reviewing key files or explaining components unless directly relevant. Return only a JSON array of strings, with each prompt under 100 characters."
    })];
    messages.extend(context);

    let _spinner = Spinner::start_with_message(options, "Preparing follow-up suggestions");
    let delay = load_user_config()?
        .request_interval_seconds
        .unwrap_or(DEFAULT_REQUEST_INTERVAL_SECONDS);
    if delay > 0 {
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }

    let client = build_http_client()?;
    let url = endpoint(&base_url, "chat/completions");
    let mut retry_count = 0;
    let response = loop {
        let mut body = json!({
            "model":model_id,
            "messages":messages,
            "stream":false,
            "max_tokens":256
        });
        if let Some(effort) = load_user_config()?.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        let mut request = client.post(&url).json(&body);
        if let Some(key) = key.as_deref() {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("request failed: {error}"))?;
        if response.status().as_u16() != 429 {
            break response;
        }
        if retry_count >= 3 {
            let body = response.text().await.unwrap_or_default();
            return Err(format_provider_error(429, &body, gateway));
        }
        let Some(delay) = rate_limit_retry_delay(response.headers(), retry_count) else {
            let body = response.text().await.unwrap_or_default();
            return Err(format_provider_error(429, &body, gateway));
        };
        emit_status(
            options,
            "retrying",
            &format!(
                "Provider rate limit reached; retrying in {}s",
                delay.as_secs()
            ),
        );
        tokio::time::sleep(delay).await;
        retry_count += 1;
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format_provider_error(status.as_u16(), &body, gateway));
    }
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| format!("invalid suggestions response: {error}"))?;
    let content = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or("model returned no suggestions")?;
    let suggestions = parse_followup_suggestions(content);
    Ok(complete_followups(suggestions))
}

fn complete_followups(mut suggestions: Vec<String>) -> Vec<String> {
    for suggestion in &mut suggestions {
        *suggestion = suggestion
            .replace("**", "")
            .replace("__", "")
            .replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
    }
    suggestions.retain(|suggestion| {
        let lower = suggestion.to_ascii_lowercase();
        !suggestion.trim().is_empty()
            && !lower.starts_with("review the key files for bugs")
            && !lower.starts_with("explain how the main components fit")
    });
    suggestions.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    suggestions.truncate(3);
    suggestions
}

fn parse_followup_suggestions(content: &str) -> Vec<String> {
    let trimmed = content.trim().trim_matches('`').trim();
    let json_value = serde_json::from_str::<Value>(trimmed).ok().or_else(|| {
        let start = trimmed.find(['[', '{'])?;
        let end = if trimmed[start..].starts_with('[') {
            trimmed.rfind(']')?
        } else {
            trimmed.rfind('}')?
        };
        serde_json::from_str::<Value>(&trimmed[start..=end]).ok()
    });
    let from_json = json_value
        .as_ref()
        .and_then(|value| {
            value.as_array().or_else(|| {
                value
                    .get("suggestions")
                    .or_else(|| value.get("follow_ups"))
                    .or_else(|| value.get("followups"))
                    .and_then(Value::as_array)
            })
        })
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str().or_else(|| {
                        item.get("prompt")
                            .or_else(|| item.get("suggestion"))
                            .or_else(|| item.get("text"))
                            .or_else(|| item.get("title"))
                            .and_then(Value::as_str)
                    })
                })
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .take(3)
                .map(|item| truncate(item, 100))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !from_json.is_empty() {
        return from_json;
    }

    content
        .lines()
        .filter_map(|line| {
            let mut line = line
                .trim()
                .trim_matches(|ch: char| matches!(ch, '`' | '"' | '\''))
                .trim();
            let lower = line.to_ascii_lowercase();
            if line.is_empty()
                || line.starts_with(['{', '}', '[', ']'])
                || ["here are", "suggestions:", "follow-ups:", "based on this"]
                    .iter()
                    .any(|prefix| lower.starts_with(prefix))
            {
                return None;
            }
            for prefix in ["- ", "* ", "• "] {
                if let Some(item) = line.strip_prefix(prefix) {
                    line = item.trim();
                    break;
                }
            }
            let number_end = line
                .find(['.', ')'])
                .filter(|end| *end > 0 && line[..*end].chars().all(|ch| ch.is_ascii_digit()));
            if let Some(end) = number_end {
                line = line[end + 1..].trim();
            }
            let line = line.trim_matches(|ch: char| matches!(ch, '`' | '*' | '"' | '\''));
            (line.split_whitespace().count() >= 3).then(|| truncate(line, 100))
        })
        .take(3)
        .collect()
}

async fn wait_for_interrupt(cancelled: Arc<AtomicBool>) {
    while !cancelled.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn run_agent_turn_inner(
    options: &Options,
    model: &str,
    prompt: &str,
    history: &mut Vec<Value>,
    interrupt: &mut EscapeInterrupt,
) -> Result<(), String> {
    let (gateway, model_id) = split_model_selector(model)?;
    let (base_url, key) = resolve_model_provider(options, gateway, model_id)?;
    let client = build_http_client()?;

    let root = options.workdir.as_deref().unwrap_or(Path::new("."));
    let root = root
        .canonicalize()
        .map_err(|e| format!("resolving project directory '{}': {e}", root.display()))?;
    if !root.is_dir() {
        return Err(format!(
            "project path '{}' is not a directory",
            root.display()
        ));
    }
    if options.project_trusted {
        emit_status(options, "exploring", "Scanning project files");
    }
    let user_config = load_user_config()?;
    let mode = configured_agent_mode(&user_config);
    let auto_approve_actions =
        options.auto_approve || user_config.auto_approve_actions.unwrap_or(false);
    let mode_instructions = match mode {
        "ask" => {
            "Mode: Ask. Answer questions and clarify requests. You may inspect project files for context, but never make changes or run commands."
        }
        "plan" => {
            "Mode: Plan. Inspect the project as needed and return a clear implementation plan. Do not change files or run commands."
        }
        _ => {
            "Mode: Build. Carry out the user's requested work. Inspect first, then make changes and run commands when appropriate. Ask before writing files or executing shell commands unless auto-approval was explicitly enabled."
        }
    };
    let system = if options.project_trusted {
        format!(
            "You are NioAI, a coding agent working in the project at {}. Start by inspecting relevant files when needed; do not claim you cannot access the project. Read and search tools are automatic. Stay within the project directory. Be concise. {}",
            root.display(),
            mode_instructions
        )
    } else {
        format!(
            "You are NioAI. The user has not trusted the current project folder, so you have no access to its files and must not claim to have inspected them. Answer general questions and ask the user to trust the folder in an interactive terminal if project access is needed. Be concise. {}",
            mode_instructions
        )
    };
    let overview = if options.project_trusted {
        format!("\n\nProject overview:\n{}", project_overview(&root))
    } else {
        String::new()
    };
    let mut messages = vec![json!({"role":"system", "content": format!("{system}{overview}")})];
    if history.len() > 32 {
        history.drain(..history.len() - 32);
    }
    messages.extend(history.iter().cloned());
    let user_message = json!({"role":"user", "content":prompt});
    messages.push(user_message.clone());
    history.push(user_message);

    let url = endpoint(&base_url, "chat/completions");
    let request_interval = load_user_config()?
        .request_interval_seconds
        .unwrap_or(DEFAULT_REQUEST_INTERVAL_SECONDS);
    let mut last_request_started = None::<Instant>;
    let reasoning_effort = user_config.reasoning_effort.as_deref();
    let tools = if options.project_trusted {
        agent_tools(mode)
    } else {
        json!([])
    };
    let mut retried_empty_response = false;
    loop {
        let mut retry_count = 0u32;
        let (response, mut spinner) = loop {
            if let Some(last_started) = last_request_started {
                let interval = Duration::from_secs(request_interval);
                let elapsed = last_started.elapsed();
                if elapsed < interval {
                    tokio::time::sleep(interval - elapsed).await;
                }
            }
            last_request_started = Some(Instant::now());
            let mut spinner = Spinner::start(options);
            let mut body = json!({
                "model": model_id,
                "messages": messages.clone(),
                "tools": tools.clone(),
                "tool_choice": "auto",
                "stream": true
            });
            if let Some(effort) = reasoning_effort {
                body["reasoning_effort"] = json!(effort);
            }
            let mut request = client.post(&url).json(&body);
            if let Some(key) = key.as_deref() {
                request = request.bearer_auth(key);
            }
            let response = request
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            if response.status().as_u16() != 429 {
                spinner.pause();
                break (response, spinner);
            }
            spinner.stop();
            if retry_count >= 3 {
                let body = response.text().await.unwrap_or_default();
                return Err(format_provider_error(429, &body, gateway));
            }
            let Some(delay) = rate_limit_retry_delay(response.headers(), retry_count) else {
                let body = response.text().await.unwrap_or_default();
                return Err(format_provider_error(429, &body, gateway));
            };
            emit_status(
                options,
                "retrying",
                &format!(
                    "Provider rate limit reached; retrying in {}s ({}/3)",
                    delay.as_secs(),
                    retry_count + 1
                ),
            );
            tokio::time::sleep(delay).await;
            retry_count += 1;
        };
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            spinner.stop();
            return Err(format_provider_error(status.as_u16(), &body, gateway));
        }
        let mut answer = String::new();
        let mut response_started = false;
        let mut formatter =
            MarkdownFormatter::new(!options.json_output && io::stdout().is_terminal());
        let mut pending_tools = std::collections::BTreeMap::<usize, PendingToolCall>::new();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_ascii_lowercase);
        let is_event_stream = content_type
            .as_deref()
            .is_none_or(|value| value.contains("text/event-stream"));
        if is_event_stream {
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            while let Some(part) = stream.next().await {
                let bytes = part.map_err(|e| format!("response stream failed: {e}"))?;
                buffer.extend_from_slice(&bytes);
                while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line = String::from_utf8_lossy(&buffer[..pos])
                        .trim_end_matches('\r')
                        .to_string();
                    buffer.drain(..=pos);
                    process_sse_line(
                        &line,
                        options,
                        &mut answer,
                        &mut pending_tools,
                        &mut response_started,
                        &mut formatter,
                    )?;
                }
            }
            if !buffer.is_empty() {
                let line = String::from_utf8_lossy(&buffer)
                    .trim_end_matches('\r')
                    .to_string();
                process_sse_line(
                    &line,
                    options,
                    &mut answer,
                    &mut pending_tools,
                    &mut response_started,
                    &mut formatter,
                )?;
            }
        } else {
            let payload = response
                .json::<Value>()
                .await
                .map_err(|error| format!("invalid provider completion response: {error}"))?;
            process_json_completion(
                payload,
                options,
                &mut answer,
                &mut pending_tools,
                &mut response_started,
                &mut formatter,
            )?;
        }
        let formatted_tail = formatter.finish();
        if !formatted_tail.is_empty() {
            emit_text(options, &formatted_tail)?;
        }
        let calls = pending_tools
            .into_values()
            .map(|pending| {
                let arguments: Value = serde_json::from_str(&pending.arguments)
                    .unwrap_or_else(|_| json!({"_invalid_arguments": pending.arguments}));
                AssistantToolCall {
                    id: pending.id,
                    name: pending.name,
                    arguments,
                }
            })
            .collect::<Vec<_>>();
        if !options.json_output && response_started && !answer.ends_with('\n') {
            let newline = if RAW_TTY_MODE.load(Ordering::SeqCst) {
                "\r\n"
            } else {
                "\n"
            };
            print!("{newline}");
            io::stdout()
                .flush()
                .map_err(|error| format!("finishing response line: {error}"))?;
        }
        spinner
            .stop_with_spacing(calls.is_empty() && response_started && !answer.trim().is_empty());
        if calls.is_empty() {
            if answer.trim().is_empty() {
                if !retried_empty_response {
                    retried_empty_response = true;
                    compact_tool_messages(&mut messages);
                    compact_tool_messages(history);
                    emit_status(
                        options,
                        "retrying",
                        "Provider completed without an answer; requesting the final response again",
                    );
                    continue;
                }
                return Err("The provider completed without sending answer text or another tool call, even after one retry. Earlier project tool results were preserved; try again or switch models with :model.".into());
            }
            if options.json_output {
                emit_status(options, "working", "Finishing response");
                emit_json(&json!({"type":"step_finish"}));
            }
            let assistant = json!({"role":"assistant", "content":answer});
            history.push(assistant);
            return Ok(());
        }
        let tool_call_messages = calls
            .iter()
            .map(|call| {
                json!({
                    "id":call.id,
                    "type":"function",
                    "function":{"name":call.name,"arguments":call.arguments.to_string()}
                })
            })
            .collect::<Vec<_>>();
        let assistant = json!({"role":"assistant", "content":if answer.is_empty() { Value::Null } else { json!(answer) }, "tool_calls":tool_call_messages});
        messages.push(assistant.clone());
        history.push(assistant);
        for call in calls {
            let input = call.arguments.clone();
            let tool_label = format!("{} {}", call.name, tool_hint(&call.name, &input));
            let status = if matches!(call.name.as_str(), "write_file" | "run_command") {
                "working"
            } else {
                "exploring"
            };
            emit_status(options, status, &tool_label);
            emit_tool_event(options, &call, "running", &input, None);
            let result = if !options.project_trusted {
                Err("project folder is not trusted; project tools are disabled".to_string())
            } else if !mode_allows_changes(mode)
                && matches!(call.name.as_str(), "write_file" | "run_command")
            {
                Err(format!(
                    "{} mode does not allow project changes or commands",
                    mode
                ))
            } else {
                execute_agent_tool(&root, &call, auto_approve_actions, interrupt).await
            };
            if matches!(&result, Err(error) if error == TURN_INTERRUPTED) {
                return Err(TURN_INTERRUPTED.into());
            }
            let tool_status = if result.is_ok() { "completed" } else { "error" };
            let output = result.as_deref().unwrap_or_else(|error| error.as_str());
            emit_tool_event(options, &call, tool_status, &input, Some(output));
            let content = match result {
                Ok(output) => output,
                Err(error) => format!("Tool error: {error}"),
            };
            let tool_message =
                json!({"role":"tool", "tool_call_id":call.id, "content":truncate(&content, 12000)});
            messages.push(tool_message.clone());
            history.push(tool_message);
        }
    }
}

async fn list_models(options: &Options) -> Result<(), String> {
    let choices = fetch_model_choices(options).await?;
    if choices.is_empty() {
        println!("No models found.");
        return Ok(());
    }
    println!("Models · free first · {} available", choices.len());
    let terminal_width = terminal::size()
        .map(|(width, _)| width as usize)
        .unwrap_or(110)
        .clamp(64, 160);
    let model_width = choices
        .iter()
        .map(|choice| choice.name.chars().count())
        .max()
        .unwrap_or(5)
        .clamp(12, 36);
    let provider_width = choices
        .iter()
        .map(|choice| choice.gateway_label.chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 18);
    let selector_width = terminal_width
        .saturating_sub(16 + model_width + provider_width)
        .max(12);
    let number_rule = "─".repeat(5);
    let model_rule = "─".repeat(model_width + 2);
    let provider_rule = "─".repeat(provider_width + 2);
    let selector_rule = "─".repeat(selector_width + 2);
    println!("┌{number_rule}┬{model_rule}┬{provider_rule}┬{selector_rule}┐");
    println!(
        "│ {:^3} │ {:<model_width$} │ {:<provider_width$} │ {:<selector_width$} │",
        "#", "MODEL", "PROVIDER", "SELECTOR"
    );
    println!("├{number_rule}┼{model_rule}┼{provider_rule}┼{selector_rule}┤");
    for (index, choice) in choices.iter().enumerate() {
        let name = truncate(&choice.name, model_width.saturating_sub(1));
        let selector = choice.selector();
        let chunks = selector
            .chars()
            .collect::<Vec<_>>()
            .chunks(selector_width)
            .map(|chunk| chunk.iter().collect::<String>())
            .collect::<Vec<_>>();
        for (line_index, chunk) in chunks.iter().enumerate() {
            if line_index == 0 {
                println!(
                    "│ {:>3} │ {:<model_width$} │ {:<provider_width$} │ {:<selector_width$} │",
                    index + 1,
                    name,
                    truncate(&choice.gateway_label, provider_width.saturating_sub(1)),
                    chunk
                );
            } else {
                println!(
                    "│     │ {:<model_width$} │ {:<provider_width$} │ {:<selector_width$} │",
                    "", "", chunk
                );
            }
        }
    }
    println!("└{number_rule}┴{model_rule}┴{provider_rule}┴{selector_rule}┘");
    println!("\nRun a model with: nio run -m <SELECTOR> <prompt>");
    Ok(())
}

fn configured_proxy_url() -> Result<Option<String>, String> {
    if let Ok(url) = env::var("NIO_PROXY") {
        if !url.trim().is_empty() {
            return Ok(Some(url));
        }
    }
    Ok(load_user_config()?.proxy_url)
}

fn build_http_client() -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder();
    if let Some(proxy_url) = configured_proxy_url()? {
        let proxy = reqwest::Proxy::all(&proxy_url).map_err(|_| {
            "invalid proxy URL; expected http://host:port or https://host:port".to_string()
        })?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|error| format!("building HTTP client: {error}"))
}

async fn fetch_model_choices(options: &Options) -> Result<Vec<ModelChoice>, String> {
    let client = build_http_client()?;
    let mut choices = Vec::new();
    let mut errors = Vec::new();
    let mut providers = vec![ProviderConfig {
        id: "kilo".into(),
        name: "Kilo Gateway".into(),
        base_url: KILO_BASE_URL.into(),
        api_key: model_api_key(options, "kilo"),
    }];
    let config = load_user_config()?;
    if let Some(openrouter) = config.providers.iter().find(|p| p.id == "openrouter") {
        providers.push(openrouter.clone());
    } else if let Some(key) = model_api_key(options, "openrouter") {
        providers.push(ProviderConfig {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            base_url: OPENROUTER_BASE_URL.into(),
            api_key: Some(key),
        });
    }
    providers.extend(
        config
            .providers
            .into_iter()
            .filter(|p| p.id != "openrouter" && p.id != "kilo"),
    );
    for provider in providers {
        let key = model_api_key(options, &provider.id).or(provider.api_key.clone());
        match fetch_models(&client, &provider.base_url, key.as_deref()).await {
            Ok(catalog) => choices.extend(choices_from_catalog(
                catalog.data,
                &provider.id,
                &provider.name,
            )),
            Err(error) => errors.push(format!("{}: {error}", provider.name)),
        }
    }
    if choices.is_empty() && !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    for error in errors {
        eprintln!("nio: skipped provider model catalog: {error}");
    }
    choices.sort_by(|a, b| {
        b.free
            .cmp(&a.free)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| {
                a.gateway_label
                    .to_lowercase()
                    .cmp(&b.gateway_label.to_lowercase())
            })
    });
    Ok(choices)
}

async fn fetch_models(
    client: &reqwest::Client,
    base_url: &str,
    key: Option<&str>,
) -> Result<ModelList, String> {
    let mut request = client.get(endpoint(base_url, "models"));
    if let Some(key) = key.filter(|value| !value.trim().is_empty()) {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("request to {base_url} failed: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "model catalog at {base_url} returned {status}: {}",
            truncate(&body, 1200)
        ));
    }
    response
        .json()
        .await
        .map_err(|e| format!("invalid model catalog at {base_url}: {e}"))
}

fn choices_from_catalog(models: Vec<ModelInfo>, gateway: &str, label: &str) -> Vec<ModelChoice> {
    let mut choices = Vec::new();
    let mut has_kilo_auto_free = false;
    for model in models {
        has_kilo_auto_free |= gateway == "kilo" && model.id == "kilo-auto/free";
        let free = model.is_free();
        choices.push(ModelChoice {
            name: model.name.unwrap_or_else(|| model.id.clone()),
            id: model.id,
            gateway: gateway.to_string(),
            gateway_label: label.to_string(),
            free,
        });
    }
    if gateway == "kilo" && !has_kilo_auto_free {
        choices.push(ModelChoice {
            id: "kilo-auto/free".into(),
            name: "Kilo Auto Free".into(),
            gateway: gateway.to_string(),
            gateway_label: label.to_string(),
            free: true,
        });
    }
    choices
}

async fn interactive(options: Options) -> Result<(), String> {
    ctrlc::set_handler(|| {
        CTRL_C_COUNT.fetch_add(1, Ordering::SeqCst);
    })
    .map_err(|e| format!("setting Ctrl+C behavior: {e}"))?;
    let session_id = options
        .session_id
        .clone()
        .unwrap_or_else(generate_session_id);
    let mut model = chosen_model(&options).await?;
    let mut history = load_session_history(Some(&session_id))?;
    let mut prompt_history = load_user_config()?.prompt_history;
    if prompt_history.len() > 100 {
        prompt_history.drain(..prompt_history.len() - 100);
    }
    print_session_header(&model, &session_id)?;
    if options.project_trusted {
        println!("Project tools are available automatically.");
    } else {
        println!("Project tools are disabled because this folder is not trusted.");
    }
    println!("Type : or / for commands; :help for help.");
    print_prompt_divider()?;

    let mut visible_followups = Vec::<String>::new();
    let mut command_mode = false;
    loop {
        if CTRL_C_COUNT.load(Ordering::SeqCst) >= 2 {
            break;
        }
        let prompt = if command_mode { "$ " } else { "🤖 nio> " };
        let line = match read_interactive_line(prompt, &prompt_history, &visible_followups)? {
            PromptInput::Line(line) => {
                CTRL_C_COUNT.store(0, Ordering::SeqCst);
                let entry = line.trim();
                if !entry.is_empty() && prompt_history.last().map(String::as_str) != Some(entry) {
                    prompt_history.push(entry.to_string());
                    if prompt_history.len() > 100 {
                        prompt_history.remove(0);
                    }
                    if let Err(error) = save_prompt_history(&prompt_history) {
                        eprintln!("nio: could not save prompt history: {error}");
                    }
                }
                if !entry.is_empty() {
                    visible_followups.clear();
                }
                line
            }
            PromptInput::Cancelled => {
                let count = CTRL_C_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
                if count >= 2 {
                    break;
                }
                println!("🔹 Cancelled. Press Ctrl+C again to exit, or Esc to exit now.");
                continue;
            }
            PromptInput::Exit | PromptInput::Eof => break,
        };
        let input = line.trim();
        let normalized_input;
        let input = if !command_mode && let Some(command) = input.strip_prefix('/') {
            normalized_input = format!(":{command}");
            normalized_input.as_str()
        } else {
            input
        };
        if input.is_empty() {
            continue;
        }
        if input == ":quit" || input == ":q" || input == ":exit" {
            break;
        }
        if input == ":help" {
            println!(
                "Commands: :clear, :help, :model, :mode, :approval, :reasoning, :provider, :proxy, :path, :setting, :bash, :ai, :quit (use : or /)"
            );
            continue;
        }
        if input == ":bash" || input == ":command" {
            command_mode = true;
            visible_followups.clear();
            println!("Command prompt enabled. Type :ai to return to Nio.");
            continue;
        }
        if input == ":ai" {
            command_mode = false;
            visible_followups.clear();
            println!("Returned to the Nio prompt.");
            continue;
        }
        if !command_mode && input == ":model" {
            if let Some(selected) = select_and_save_model(&options).await? {
                model = selected;
                println!("Switched to model {model}");
            } else {
                println!("Model unchanged.");
            }
            continue;
        }
        if !command_mode && input == ":mode" {
            configure_agent_mode()?;
            continue;
        }
        if !command_mode && input == ":approval" {
            toggle_auto_approval()?;
            continue;
        }
        if !command_mode && input == ":reasoning" {
            configure_reasoning_effort()?;
            continue;
        }
        if !command_mode && input == ":provider" {
            configure_provider().await?;
            continue;
        }
        if !command_mode && input == ":proxy" {
            configure_proxy().await?;
            continue;
        }
        if !command_mode && (input == ":path" || input == ":workingpath") {
            print_working_path(&options)?;
            continue;
        }
        if !command_mode && input == ":clear" {
            history.clear();
            visible_followups.clear();
            save_session_history(Some(&session_id), &history)?;
            let mut stdout = io::stdout();
            execute!(
                stdout,
                Clear(ClearType::Purge),
                Clear(ClearType::All),
                MoveTo(0, 0)
            )
            .map_err(|error| format!("clearing terminal: {error}"))?;
            print_session_header(&model, &session_id)?;
            println!("Conversation history cleared.");
            print_prompt_divider()?;
            continue;
        }
        if !command_mode && (input == ":setting" || input == ":settings") {
            configure_settings()?;
            continue;
        }
        if !command_mode && input.starts_with(':') {
            eprintln!("Unknown command. Type :help for commands.");
            continue;
        }
        if command_mode {
            let status = tokio::process::Command::new("sh")
                .arg("-lc")
                .arg(input)
                .current_dir(options.workdir.as_deref().unwrap_or(Path::new(".")))
                .status()
                .await
                .map_err(|error| format!("starting command: {error}"))?;
            println!("[exit {}]", status.code().unwrap_or(-1));
            continue;
        }
        match run_agent_turn(&options, &model, input, &mut history).await {
            Ok(suggestions) => {
                save_session_history(Some(&session_id), &history)?;
                visible_followups = suggestions;
            }
            Err(error) if error == TURN_INTERRUPTED => {
                if CTRL_C_COUNT.load(Ordering::SeqCst) >= 2 {
                    break;
                }
                if history.last().is_some_and(|message| {
                    message.get("role").and_then(Value::as_str) == Some("user")
                }) {
                    history.pop();
                }
                println!("\nInterrupted.");
            }
            Err(error) => {
                if history.last().is_some_and(|message| {
                    message.get("role").and_then(Value::as_str) == Some("user")
                }) {
                    history.pop();
                }
                eprintln!("nio: {error}");
            }
        }
    }
    println!(
        "\nSession saved. Resume with: nio --session {}",
        shell_quote(&session_id)
    );
    Ok(())
}

fn print_session_header(model: &str, session_id: &str) -> Result<(), String> {
    let config = load_user_config()?;
    let mode = configured_agent_mode(&config);
    let effort = config
        .reasoning_effort
        .as_deref()
        .unwrap_or("provider default");
    print_prompt_divider()?;
    println!("🤖 NioAI · model {model}");
    println!("Session ID: {session_id}");
    println!(
        "Mode: {} · Reasoning: {}",
        title_case(mode),
        title_case(effort)
    );
    println!(
        "Approval: {}",
        if config.auto_approve_actions.unwrap_or(false) {
            "Automatic"
        } else {
            "Ask before writes and commands"
        }
    );
    Ok(())
}

fn print_prompt_divider() -> Result<(), String> {
    let width = terminal::size()
        .map(|(width, _)| width as usize)
        .unwrap_or(80)
        .max(2);
    queue!(io::stdout(), SetForegroundColor(Color::DarkGrey))
        .map_err(|error| format!("styling prompt divider: {error}"))?;
    print!("{}", "─".repeat(width - 1));
    queue!(io::stdout(), ResetColor).map_err(|error| format!("styling prompt divider: {error}"))?;
    println!();
    io::stdout()
        .flush()
        .map_err(|error| format!("writing prompt divider: {error}"))
}

const COMMANDS: [(&str, &str); 12] = [
    (":clear", "Clear conversation history"),
    (":help", "Show available commands"),
    (":model", "Switch model"),
    (":mode", "Choose Ask, Plan, or Build mode"),
    (
        ":approval",
        "Toggle automatic approval for writes and commands",
    ),
    (":provider", "Configure model providers"),
    (":proxy", "Route provider requests through a proxy"),
    (":path", "Show the current project directory"),
    (":reasoning", "Set reasoning effort"),
    (":bash", "Switch to a direct shell prompt"),
    (
        ":setting",
        "Configure mode, reasoning, approvals, and other settings",
    ),
    (":quit", "Exit Nio"),
];

enum PromptInput {
    Line(String),
    Cancelled,
    Exit,
    Eof,
}

struct PaletteScreen {
    active: bool,
}

fn draw_followup_buttons(stdout: &mut io::Stdout, suggestions: &[String]) -> Result<(), String> {
    let width = terminal::size().map(|(width, _)| width).unwrap_or(80) as usize;
    for (index, suggestion) in suggestions.iter().enumerate() {
        queue!(stdout, SetForegroundColor(Color::DarkCyan))
            .map_err(|error| format!("drawing follow-up button: {error}"))?;
        write!(stdout, "  [{}]", index + 1)
            .map_err(|error| format!("drawing follow-up button: {error}"))?;
        queue!(stdout, ResetColor).map_err(|error| format!("drawing follow-up button: {error}"))?;
        write!(
            stdout,
            " {}\r\n",
            truncate(suggestion, width.saturating_sub(7))
        )
        .map_err(|error| format!("drawing follow-up button: {error}"))?;
    }
    Ok(())
}

impl PaletteScreen {
    fn new() -> Self {
        Self { active: false }
    }

    fn enter(&mut self, stdout: &mut io::Stdout) -> Result<(), String> {
        self.active = true;
        execute!(
            stdout,
            EnterAlternateScreen,
            Clear(ClearType::All),
            MoveTo(0, 0)
        )
        .map_err(|e| format!("opening command palette: {e}"))?;
        Ok(())
    }

    fn leave(&mut self, stdout: &mut io::Stdout) -> Result<(), String> {
        if self.active {
            execute!(stdout, LeaveAlternateScreen)
                .map_err(|e| format!("closing command palette: {e}"))?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for PaletteScreen {
    fn drop(&mut self) {
        if self.active {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
    }
}

fn read_interactive_line(
    prompt: &str,
    history: &[String],
    suggestions: &[String],
) -> Result<PromptInput, String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        if !suggestions.is_empty() {
            println!("\nFollow-ups (enter a number to ask, or type your own):");
            for (index, suggestion) in suggestions.iter().enumerate() {
                println!("  {}) {suggestion}", index + 1);
            }
        }
        print!("\n{prompt}");
        io::stdout()
            .flush()
            .map_err(|e| format!("writing prompt: {e}"))?;
        let mut line = String::new();
        return match io::stdin().read_line(&mut line) {
            Ok(0) => Ok(PromptInput::Eof),
            Ok(_) => {
                let trimmed = line.trim();
                if let Ok(index) = trimmed.parse::<usize>() {
                    if let Some(suggestion) = index
                        .checked_sub(1)
                        .and_then(|index| suggestions.get(index))
                    {
                        return Ok(PromptInput::Line(suggestion.clone()));
                    }
                }
                Ok(PromptInput::Line(line))
            }
            Err(e) => Err(format!("reading prompt: {e}")),
        };
    }

    terminal::enable_raw_mode().map_err(|e| format!("enabling interactive input: {e}"))?;
    let result = read_interactive_line_raw(prompt, history, suggestions);
    let restore = terminal::disable_raw_mode();
    restore.map_err(|e| format!("restoring terminal input: {e}"))?;
    result
}

fn read_interactive_line_raw(
    prompt: &str,
    history: &[String],
    suggestions: &[String],
) -> Result<PromptInput, String> {
    let mut stdout = io::stdout();
    let mut input = String::new();
    let mut selected = 0usize;
    let mut history_cursor = None::<usize>;
    let mut history_draft = None::<String>;
    let mut palette = PaletteScreen::new();
    let mut mode_indicator_visible = false;
    write!(stdout, "\r\n").map_err(|e| format!("writing prompt: {e}"))?;
    if !suggestions.is_empty() {
        write!(
            stdout,
            "Follow-ups (type a number then Enter, or type your own):\r\n"
        )
        .map_err(|error| format!("drawing follow-up buttons: {error}"))?;
        draw_followup_buttons(&mut stdout, suggestions)?;
        print_prompt_divider()?;
        write!(stdout, "\r\n").map_err(|error| format!("spacing prompt divider: {error}"))?;
    }
    draw_input(&mut stdout, prompt, &input)?;

    loop {
        let event = event::read().map_err(|e| format!("reading prompt input: {e}"))?;
        let Event::Key(key) = event else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }

        let command_suggestions = command_suggestions(&input);
        match key.code {
            KeyCode::Enter => {
                if let Some(suggestion) = input
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| suggestions.get(index))
                {
                    let suggestion = suggestion.clone();
                    palette.leave(&mut stdout)?;
                    queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
                        .map_err(|error| format!("selecting follow-up: {error}"))?;
                    write!(stdout, "You: {suggestion}\r\n")
                        .map_err(|error| format!("selecting follow-up: {error}"))?;
                    stdout
                        .flush()
                        .map_err(|error| format!("selecting follow-up: {error}"))?;
                    return Ok(PromptInput::Line(suggestion));
                }
                if !command_suggestions.is_empty()
                    && !COMMANDS.iter().any(|(command, _)| *command == input)
                {
                    input = command_suggestions[selected.min(command_suggestions.len() - 1)]
                        .to_string();
                }
                palette.leave(&mut stdout)?;
                queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
                    .map_err(|e| format!("updating prompt: {e}"))?;
                write!(stdout, "{prompt}{input}\r\n")
                    .map_err(|e| format!("writing prompt: {e}"))?;
                stdout.flush().map_err(|e| format!("writing prompt: {e}"))?;
                return Ok(PromptInput::Line(input));
            }
            KeyCode::Tab if input.is_empty() => {
                cycle_agent_mode()?;
                let config = load_user_config()?;
                let mode = title_case(configured_agent_mode(&config));
                palette.leave(&mut stdout)?;
                if mode_indicator_visible {
                    queue!(
                        stdout,
                        MoveUp(1),
                        MoveToColumn(0),
                        Clear(ClearType::CurrentLine)
                    )
                    .map_err(|error| format!("updating mode indicator: {error}"))?;
                    write!(stdout, "Mode: {mode}")
                        .map_err(|error| format!("updating mode indicator: {error}"))?;
                    queue!(
                        stdout,
                        MoveDown(1),
                        MoveToColumn(0),
                        Clear(ClearType::CurrentLine)
                    )
                    .map_err(|error| format!("updating prompt: {error}"))?;
                } else {
                    queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
                        .map_err(|error| format!("updating prompt: {error}"))?;
                    write!(stdout, "Mode: {mode}\r\n")
                        .map_err(|error| format!("showing mode: {error}"))?;
                    mode_indicator_visible = true;
                }
                draw_input(&mut stdout, prompt, &input)?;
            }
            KeyCode::Tab if !command_suggestions.is_empty() => {
                input =
                    command_suggestions[selected.min(command_suggestions.len() - 1)].to_string();
                selected = 0;
            }
            KeyCode::Up | KeyCode::Left if palette.active && !command_suggestions.is_empty() => {
                selected = selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Right if palette.active && !command_suggestions.is_empty() => {
                selected = (selected + 1).min(command_suggestions.len() - 1);
            }
            KeyCode::Up if !palette.active && !history.is_empty() => {
                let cursor = match history_cursor {
                    Some(cursor) => cursor.saturating_sub(1),
                    None => {
                        history_draft = Some(input.clone());
                        history.len() - 1
                    }
                };
                history_cursor = Some(cursor);
                input = history[cursor].clone();
            }
            KeyCode::Down if !palette.active => {
                if let Some(cursor) = history_cursor {
                    if cursor + 1 < history.len() {
                        let next = cursor + 1;
                        history_cursor = Some(next);
                        input = history[next].clone();
                    } else {
                        history_cursor = None;
                        input = history_draft.take().unwrap_or_default();
                    }
                }
            }
            KeyCode::Backspace => {
                input.pop();
                selected = 0;
                history_cursor = None;
                history_draft = None;
                if palette.active && input.is_empty() {
                    palette.leave(&mut stdout)?;
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                palette.leave(&mut stdout)?;
                queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
                    .map_err(|e| format!("updating prompt: {e}"))?;
                write!(stdout, "{prompt}^C\r\n").map_err(|e| format!("writing prompt: {e}"))?;
                stdout.flush().map_err(|e| format!("writing prompt: {e}"))?;
                return Ok(PromptInput::Cancelled);
            }
            KeyCode::Esc => {
                palette.leave(&mut stdout)?;
                queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
                    .map_err(|e| format!("updating prompt: {e}"))?;
                write!(stdout, "{prompt}(exit)\r\n").map_err(|e| format!("writing prompt: {e}"))?;
                stdout.flush().map_err(|e| format!("writing prompt: {e}"))?;
                return Ok(PromptInput::Exit);
            }
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL) && input.is_empty() =>
            {
                palette.leave(&mut stdout)?;
                return Ok(PromptInput::Eof);
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if input.is_empty() && matches!(character, ':' | '/') {
                    palette.enter(&mut stdout)?;
                }
                input.push(character);
                selected = 0;
                history_cursor = None;
                history_draft = None;
            }
            _ => {}
        }
        if palette.active {
            draw_command_palette(&mut stdout, prompt, &input, selected)?;
        } else {
            draw_input(&mut stdout, prompt, &input)?;
        }
    }
}

fn command_suggestions(input: &str) -> Vec<&'static str> {
    let Some(prefix) = input.strip_prefix(':').or_else(|| input.strip_prefix('/')) else {
        return Vec::new();
    };
    COMMANDS
        .iter()
        .filter(|(command, _)| {
            command
                .strip_prefix(':')
                .is_some_and(|name| name.starts_with(prefix))
        })
        .map(|(command, _)| *command)
        .collect()
}

fn terminal_text_width(text: &str) -> usize {
    text.chars()
        .map(|ch| {
            let code = ch as u32;
            if ch == '\0'
                || ch.is_control()
                || matches!(code, 0x0300..=0x036F | 0xFE00..=0xFE0F | 0x200D)
            {
                0
            } else if matches!(
                code,
                0x1100..=0x115F
                    | 0x2329..=0x232A
                    | 0x2E80..=0xA4CF
                    | 0xAC00..=0xD7A3
                    | 0xF900..=0xFAFF
                    | 0xFE10..=0xFE6F
                    | 0xFF00..=0xFF60
                    | 0xFFE0..=0xFFE6
                    | 0x1F000..=0x1FAFF
                    | 0x20000..=0x3FFFD
            ) {
                2
            } else {
                1
            }
        })
        .sum()
}

fn draw_input(stdout: &mut io::Stdout, prompt: &str, input: &str) -> Result<(), String> {
    queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
        .map_err(|e| format!("updating prompt: {e}"))?;
    write!(stdout, "{prompt}{input}").map_err(|e| format!("writing prompt: {e}"))?;
    // Leave the cursor where the terminal placed it after rendering the text.
    // Counting Unicode characters as columns misplaces the cursor for wide glyphs.
    stdout.flush().map_err(|e| format!("updating prompt: {e}"))
}

fn draw_command_palette(
    stdout: &mut io::Stdout,
    prompt: &str,
    input: &str,
    selected: usize,
) -> Result<(), String> {
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))
        .map_err(|e| format!("drawing command palette: {e}"))?;
    write!(stdout, "Commands\r\n").map_err(|e| format!("drawing command palette: {e}"))?;
    let commands = command_suggestions(input);
    for (index, command) in commands.iter().enumerate() {
        if index == selected {
            queue!(stdout, SetAttribute(Attribute::Reverse))
                .map_err(|e| format!("styling command palette: {e}"))?;
            write!(
                stdout,
                "› {command:<12} {}\r\n",
                COMMANDS[index_for_command(command)].1
            )
            .map_err(|e| format!("drawing command palette: {e}"))?;
            queue!(stdout, SetAttribute(Attribute::NoReverse))
                .map_err(|e| format!("styling command palette: {e}"))?;
        } else {
            write!(
                stdout,
                "  {command:<12} {}\r\n",
                COMMANDS[index_for_command(command)].1
            )
            .map_err(|e| format!("drawing command palette: {e}"))?;
        }
    }
    write!(stdout, "\r\n").map_err(|e| format!("drawing command palette: {e}"))?;
    write!(stdout, "{prompt}{input}\r\n").map_err(|e| format!("drawing command palette: {e}"))?;
    write!(
        stdout,
        "↑/↓ select  Tab mode/complete  Enter run  Esc exit\r\n"
    )
    .map_err(|e| format!("drawing command palette: {e}"))?;
    let (width, _) = terminal::size().unwrap_or((80, 24));
    let cursor_column = (terminal_text_width(prompt) + terminal_text_width(input))
        .min(width.saturating_sub(1) as usize) as u16;
    let prompt_row = commands.len().saturating_add(2) as u16;
    queue!(stdout, MoveTo(cursor_column, prompt_row))
        .map_err(|e| format!("positioning command palette cursor: {e}"))?;
    stdout
        .flush()
        .map_err(|e| format!("drawing command palette: {e}"))
}

fn index_for_command(command: &str) -> usize {
    COMMANDS
        .iter()
        .position(|(name, _)| *name == command)
        .unwrap_or(0)
}

async fn chosen_model(options: &Options) -> Result<String, String> {
    if let Some(model) = options.model.as_deref() {
        Ok(model.to_string())
    } else if let Some(model) = read_saved_model()? {
        Ok(model)
    } else {
        select_and_save_model(options)
            .await?
            .ok_or_else(|| "model selection cancelled".to_string())
    }
}

async fn select_and_save_model(options: &Options) -> Result<Option<String>, String> {
    let choices = fetch_model_choices(options).await?;
    if choices.is_empty() {
        return Err("no models are available from the configured providers".into());
    }
    let current_model = options.model.clone().or(read_saved_model()?);
    let Some(index) = choose_model_index(&choices, current_model.as_deref())? else {
        return Ok(None);
    };
    let choice = &choices[index];
    let selector = choice.selector();
    save_default_model(&selector)?;
    println!(
        "Saved default model: {} ({})",
        choice.name, choice.gateway_label
    );
    Ok(Some(selector))
}

fn choose_model_index(
    choices: &[ModelChoice],
    current_model: Option<&str>,
) -> Result<Option<usize>, String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        println!("Choose your default model (free models listed first):");
        for (index, choice) in choices.iter().enumerate() {
            println!(
                "  {}{}) {} ({})",
                if current_model == Some(choice.selector().as_str()) {
                    "✓ "
                } else {
                    "  "
                },
                index + 1,
                choice.name,
                choice.gateway_label
            );
        }
        print!("Select a model [1-{}]: ", choices.len());
        io::stdout()
            .flush()
            .map_err(|e| format!("writing model selection: {e}"))?;
        let mut selection = String::new();
        io::stdin()
            .read_line(&mut selection)
            .map_err(|e| format!("reading model selection: {e}"))?;
        let index = selection
            .trim()
            .parse::<usize>()
            .map_err(|_| "enter the number shown beside a model".to_string())?;
        return index
            .checked_sub(1)
            .filter(|index| *index < choices.len())
            .map(Some)
            .ok_or_else(|| "model selection is out of range".to_string());
    }

    terminal::enable_raw_mode().map_err(|e| format!("enabling model picker: {e}"))?;
    let result = choose_model_index_raw(choices, current_model);
    let restore = terminal::disable_raw_mode();
    restore.map_err(|e| format!("restoring terminal input: {e}"))?;
    result
}

fn choose_model_index_raw(
    choices: &[ModelChoice],
    current_model: Option<&str>,
) -> Result<Option<usize>, String> {
    const PAGE_SIZE: usize = 25;
    let mut stdout = io::stdout();
    let mut screen = PaletteScreen::new();
    screen.enter(&mut stdout)?;
    let mut query = String::new();
    let mut selected = current_model
        .and_then(|model| choices.iter().position(|choice| choice.selector() == model))
        .unwrap_or(0);
    draw_model_picker(&mut stdout, choices, selected, current_model, &query)?;
    loop {
        let event = event::read().map_err(|e| format!("reading model selection: {e}"))?;
        let Event::Key(key) = event else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Down => {
                let matches = filtered_model_indices(choices, &query);
                selected = selected
                    .saturating_add(1)
                    .min(matches.len().saturating_sub(1));
            }
            KeyCode::Left => {
                selected = selected.saturating_sub(PAGE_SIZE);
            }
            KeyCode::Right => {
                let matches = filtered_model_indices(choices, &query);
                selected = selected
                    .saturating_add(PAGE_SIZE)
                    .min(matches.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let matches = filtered_model_indices(choices, &query);
                if let Some(choice_index) = matches.get(selected).copied() {
                    screen.leave(&mut stdout)?;
                    return Ok(Some(choice_index));
                }
            }
            KeyCode::Esc => {
                if !query.is_empty() {
                    query.clear();
                    selected = 0;
                    draw_model_picker(&mut stdout, choices, selected, current_model, &query)?;
                    continue;
                }
                screen.leave(&mut stdout)?;
                return Ok(None);
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let count = CTRL_C_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
                screen.leave(&mut stdout)?;
                if count >= 2 {
                    return Ok(None);
                }
                return Ok(None);
            }
            KeyCode::Backspace => {
                query.pop();
                selected = 0;
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                query.push(character);
                selected = 0;
            }
            _ => {}
        }
        let matches = filtered_model_indices(choices, &query);
        selected = selected.min(matches.len().saturating_sub(1));
        draw_model_picker(&mut stdout, choices, selected, current_model, &query)?;
    }
}

fn filtered_model_indices(choices: &[ModelChoice], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    choices
        .iter()
        .enumerate()
        .filter_map(|(index, choice)| {
            let matches = query.is_empty()
                || choice.name.to_lowercase().contains(&query)
                || choice.gateway_label.to_lowercase().contains(&query)
                || choice.selector().to_lowercase().contains(&query);
            matches.then_some(index)
        })
        .collect()
}

fn draw_model_picker(
    stdout: &mut io::Stdout,
    choices: &[ModelChoice],
    selected: usize,
    current_model: Option<&str>,
    query: &str,
) -> Result<(), String> {
    const PAGE_SIZE: usize = 25;
    let (width, height) = terminal::size().unwrap_or((80, 24));
    let matches = filtered_model_indices(choices, query);
    let page = selected / PAGE_SIZE;
    let page_start = page * PAGE_SIZE;
    let page_end = (page_start + PAGE_SIZE).min(matches.len());
    let visible_rows = height.saturating_sub(7).max(1) as usize;
    let page_offset = selected.saturating_sub(page_start);
    let visible_start = page_offset
        .saturating_sub(visible_rows / 2)
        .min(page_end.saturating_sub(page_start + visible_rows));
    let start = page_start + visible_start;
    let end = (start + visible_rows).min(page_end);
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))
        .map_err(|e| format!("drawing model picker: {e}"))?;
    write!(
        stdout,
        "Choose a model  ↑/↓ move  ←/→ page (25)  Enter select  Esc clear/cancel\r\n"
    )
    .map_err(|e| format!("drawing model picker: {e}"))?;
    let search_prompt = "Search model/provider: ";
    let terminal_columns = (width as usize).max(1);
    let query_chars = query.chars().collect::<Vec<_>>();
    let query_columns = terminal_columns
        .saturating_sub(search_prompt.chars().count())
        .max(1);
    let visible_query = query_chars
        .iter()
        .skip(query_chars.len().saturating_sub(query_columns))
        .collect::<String>();
    write!(stdout, "{search_prompt}{visible_query}\r\n")
        .map_err(|e| format!("drawing model picker: {e}"))?;
    if matches.is_empty() {
        write!(
            stdout,
            "  No matches. Edit the search or press Esc to clear it.\r\n"
        )
        .map_err(|e| format!("drawing model picker: {e}"))?;
    }
    for (visible_index, choice_index) in matches.iter().enumerate().take(end).skip(start) {
        let choice = &choices[*choice_index];
        let free_tag = if choice.free { " · free" } else { "" };
        let row = format!("{} ({}){free_tag}", choice.name, choice.gateway_label);
        let marker = if current_model == Some(choice.selector().as_str()) {
            "✓"
        } else {
            " "
        };
        if visible_index == selected {
            queue!(stdout, SetAttribute(Attribute::Reverse))
                .map_err(|e| format!("styling model picker: {e}"))?;
            write!(
                stdout,
                "› {} {}\r\n",
                marker,
                truncate(&row, width.saturating_sub(5) as usize)
            )
            .map_err(|e| format!("drawing model picker: {e}"))?;
            queue!(stdout, SetAttribute(Attribute::NoReverse))
                .map_err(|e| format!("styling model picker: {e}"))?;
        } else {
            write!(
                stdout,
                "  {} {}\r\n",
                marker,
                truncate(&row, width.saturating_sub(5) as usize)
            )
            .map_err(|e| format!("drawing model picker: {e}"))?;
        }
    }
    write!(
        stdout,
        "\r\nPage {}/{} · {}–{} of {} matches\r\n",
        if matches.is_empty() { 0 } else { page + 1 },
        matches.len().div_ceil(PAGE_SIZE),
        if matches.is_empty() {
            0
        } else {
            page_start + 1
        },
        page_end,
        matches.len()
    )
    .map_err(|e| format!("drawing model picker: {e}"))?;
    let cursor_column = (terminal_text_width(search_prompt) + terminal_text_width(&visible_query))
        .min(terminal_columns.saturating_sub(1)) as u16;
    queue!(stdout, MoveTo(cursor_column, 1))
        .map_err(|e| format!("positioning model search cursor: {e}"))?;
    stdout
        .flush()
        .map_err(|e| format!("drawing model picker: {e}"))
}

fn config_path() -> Result<PathBuf, String> {
    if let Ok(path) = env::var("NIO_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or("cannot locate config directory; set NIO_CONFIG or HOME")?;
    Ok(base.join("nio").join("config.json"))
}

fn read_saved_model() -> Result<Option<String>, String> {
    Ok(load_user_config()?.default_model)
}

fn load_user_config() -> Result<UserConfig, String> {
    let path = config_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UserConfig::default());
        }
        Err(error) => return Err(format!("reading {}: {error}", path.display())),
    };
    serde_json::from_str(&contents)
        .map_err(|error| format!("invalid config at {}: {error}", path.display()))
}

fn save_default_model(model: &str) -> Result<(), String> {
    let mut config = load_user_config()?;
    config.default_model = Some(model.to_string());
    save_user_config(&config)
}

fn save_prompt_history(history: &[String]) -> Result<(), String> {
    let mut config = load_user_config()?;
    config.prompt_history = history.to_vec();
    save_user_config(&config)
}

fn session_history_path(session_id: &str) -> Result<PathBuf, String> {
    if session_id.is_empty() {
        return Err("session ID must not be empty".into());
    }
    if session_id.len() > 120 {
        return Err("session ID must be 120 bytes or fewer".into());
    }
    let config = config_path()?;
    let directory = config
        .parent()
        .ok_or("config file path has no parent directory")?
        .join("sessions");
    let encoded_id = session_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(directory.join(format!("{encoded_id}.json")))
}

fn generate_session_id() -> String {
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or_default();
        let count = SESSION_ID_COUNTER.fetch_add(1, Ordering::Relaxed) as u64;
        let mut value = nanos ^ (u64::from(std::process::id()) << 32) ^ count;
        value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^= value >> 31;
        let mut id = [b'0'; 8];
        for character in id.iter_mut().rev() {
            *character = ALPHABET[(value % ALPHABET.len() as u64) as usize];
            value /= ALPHABET.len() as u64;
        }
        let id = format!("nio-{}", String::from_utf8_lossy(&id));
        if session_history_path(&id)
            .map(|path| !path.exists())
            .unwrap_or(true)
        {
            return id;
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn load_session_history(session_id: Option<&str>) -> Result<Vec<Value>, String> {
    let Some(session_id) = session_id else {
        return Ok(Vec::new());
    };
    let path = session_history_path(session_id)?;
    match std::fs::read(&path) {
        Ok(contents) => serde_json::from_slice(&contents)
            .map_err(|error| format!("invalid session history at {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!(
            "reading session history {}: {error}",
            path.display()
        )),
    }
}

fn save_session_history(session_id: Option<&str>, history: &[Value]) -> Result<(), String> {
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let path = session_history_path(session_id)?;
    let parent = path
        .parent()
        .ok_or("session history path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating session directory {}: {error}", parent.display()))?;
    let contents = serde_json::to_vec(history)
        .map_err(|error| format!("serializing session history: {error}"))?;
    std::fs::write(&path, contents)
        .map_err(|error| format!("writing session history {}: {error}", path.display()))
}

fn save_user_config(config: &UserConfig) -> Result<(), String> {
    let path = config_path()?;
    let parent = path
        .parent()
        .ok_or("config file path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    let contents = serde_json::to_vec_pretty(config)
        .map_err(|error| format!("serializing config: {error}"))?;
    std::fs::write(&path, contents)
        .map_err(|error| format!("writing {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path)
            .map_err(|error| format!("checking permissions for {}: {error}", path.display()))?
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&path, permissions).map_err(|error| {
            format!(
                "protecting provider credentials in {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

async fn configure_provider() -> Result<(), String> {
    let mut config = load_user_config()?;
    println!("\nModel providers");
    for (index, (id, name, _)) in PROVIDER_PRESETS.iter().enumerate() {
        let free_tag = provider_free_label(id)
            .map(|label| format!(" ({label})"))
            .unwrap_or_default();
        println!("  {}) {name}{free_tag}", index + 1);
    }
    let custom_option = PROVIDER_PRESETS.len() + 1;
    let remove_option = PROVIDER_PRESETS.len() + 2;
    println!("  {custom_option}) Custom OpenAI-compatible provider");
    println!("  {remove_option}) Remove a saved provider");
    if !config.providers.is_empty() {
        println!(
            "\nSaved: {}",
            config
                .providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!("🔹 (free) = provider offers a free model catalog");
    println!(
        "🔹 Most providers ask you to bring your own API key (BYOK). Free access may still need a key."
    );
    print!("Choose [1-{remove_option}], or Enter to cancel: ");
    io::stdout()
        .flush()
        .map_err(|error| format!("writing provider prompt: {error}"))?;
    let mut selection = String::new();
    io::stdin()
        .read_line(&mut selection)
        .map_err(|error| format!("reading provider choice: {error}"))?;
    let selected = selection.trim().parse::<usize>().ok();
    let id = match selected {
        Some(number) if (1..=PROVIDER_PRESETS.len()).contains(&number) => {
            PROVIDER_PRESETS[number - 1].0.to_string()
        }
        Some(number) if number == custom_option => {
            print!("Provider ID (lowercase, e.g. orca): ");
            io::stdout()
                .flush()
                .map_err(|error| format!("writing provider ID prompt: {error}"))?;
            let mut custom_id = String::new();
            io::stdin()
                .read_line(&mut custom_id)
                .map_err(|error| format!("reading provider ID: {error}"))?;
            custom_id.trim().to_ascii_lowercase()
        }
        Some(number) if number == remove_option && !config.providers.is_empty() => {
            println!("Choose a provider to remove:");
            for (index, provider) in config.providers.iter().enumerate() {
                println!("  {}) {}", index + 1, provider.id);
            }
            print!("Provider number, or Enter to cancel: ");
            io::stdout()
                .flush()
                .map_err(|error| format!("writing remove prompt: {error}"))?;
            let mut number = String::new();
            io::stdin()
                .read_line(&mut number)
                .map_err(|error| format!("reading provider choice: {error}"))?;
            let Ok(index) = number.trim().parse::<usize>() else {
                return Ok(());
            };
            if index == 0 || index > config.providers.len() {
                return Ok(());
            }
            let removed = config.providers.remove(index - 1).id;
            save_user_config(&config)?;
            println!("Removed provider '{removed}'.");
            return Ok(());
        }
        None if selection.trim().is_empty() => return Ok(()),
        _ => {
            println!("Choose a number from the list.");
            return Ok(());
        }
    };
    if id.is_empty() {
        return Ok(());
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(
            "provider ID may contain lowercase letters, numbers, '.', '_' and '-' only".into(),
        );
    }
    if id == "kilo" {
        return Err("Kilo Gateway is built in and does not need provider setup".into());
    }
    let existing = config
        .providers
        .iter()
        .find(|provider| provider.id == id)
        .cloned();
    let preset = PROVIDER_PRESETS
        .iter()
        .find(|(provider_id, _, _)| *provider_id == id)
        .map(|(_, _, url)| *url);
    let default_url = existing
        .as_ref()
        .map(|provider| provider.base_url.as_str())
        .or(preset)
        .unwrap_or("");
    print!("OpenAI-compatible base URL [{default_url}]: ");
    io::stdout()
        .flush()
        .map_err(|error| format!("writing provider URL prompt: {error}"))?;
    let mut base_url = String::new();
    io::stdin()
        .read_line(&mut base_url)
        .map_err(|error| format!("reading provider URL: {error}"))?;
    let base_url = if base_url.trim().is_empty() {
        default_url.to_string()
    } else {
        base_url.trim().trim_end_matches('/').to_string()
    };
    let parsed = reqwest::Url::parse(&base_url).map_err(|_| {
        "enter a valid provider base URL, such as https://host.example/v1".to_string()
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("provider URL must use http:// or https:// and include a host".into());
    }
    let key =
        read_provider_key("API key (visible; Enter keeps existing, type 'clear' to remove): ")?;
    let key = if key.trim().is_empty() {
        existing.and_then(|provider| provider.api_key)
    } else if key.trim().eq_ignore_ascii_case("clear") {
        None
    } else {
        Some(key.trim().to_string())
    };
    let provider = ProviderConfig {
        id: id.clone(),
        name: PROVIDER_PRESETS
            .iter()
            .find(|(provider_id, _, _)| *provider_id == id)
            .map(|(_, name, _)| (*name).to_string())
            .unwrap_or_else(|| id.clone()),
        base_url: base_url.clone(),
        api_key: key,
    };
    if let Some(index) = config.providers.iter().position(|current| current.id == id) {
        config.providers[index] = provider;
    } else {
        config.providers.push(provider);
    }
    save_user_config(&config)?;
    println!("Saved provider '{id}' ({base_url}). Use :model to browse its models.");
    Ok(())
}

async fn configure_proxy() -> Result<(), String> {
    let mut config = load_user_config()?;
    let active = configured_proxy_url()?;
    match active.as_deref() {
        Some(url) => println!("Active proxy: {}", safe_proxy_label(url)),
        None => {
            println!("Active proxy: none configured (system proxy environment may still apply)")
        }
    }
    println!(
        "Use a proxy you own or are authorized to use. Public proxies can expose API traffic and credentials."
    );
    println!("Proxy presets (the service must already be installed and running):");
    println!("  1) Tinyproxy  http://127.0.0.1:8888");
    println!("  2) Squid      http://127.0.0.1:3128");
    print!("Choose 1/2, enter a custom URL, 'off' to disable, or Enter to keep: ");
    io::stdout()
        .flush()
        .map_err(|error| format!("writing proxy prompt: {error}"))?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| format!("reading proxy URL: {error}"))?;
    let input = match input.trim() {
        "1" => "http://127.0.0.1:8888",
        "2" => "http://127.0.0.1:3128",
        value => value,
    };
    if input.is_empty() {
        return Ok(());
    }
    if input.eq_ignore_ascii_case("off") {
        config.proxy_url = None;
        save_user_config(&config)?;
        if env::var("NIO_PROXY").is_ok_and(|value| !value.trim().is_empty()) {
            println!("Saved proxy disabled. NIO_PROXY still overrides this setting.");
        } else {
            println!("Saved proxy disabled.");
        }
        return Ok(());
    }
    validate_proxy_url(input)?;
    config.proxy_url = Some(input.to_string());
    save_user_config(&config)?;
    println!("Saved proxy {}.", safe_proxy_label(input));
    check_provider_connectivity(input, &config.providers).await?;
    Ok(())
}

fn print_working_path(options: &Options) -> Result<(), String> {
    let path = options.workdir.as_deref().unwrap_or(Path::new("."));
    let path = path
        .canonicalize()
        .map_err(|error| format!("resolving working directory '{}': {error}", path.display()))?;
    println!("Working path: {}", path.display());
    Ok(())
}

fn validate_proxy_url(input: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(input)
        .map_err(|_| "enter a valid proxy URL such as http://proxy.example:8080".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("proxy URL must use http:// or https:// and include a host".into());
    }
    reqwest::Proxy::all(input)
        .map(|_| ())
        .map_err(|_| "proxy URL is invalid or uses an unsupported proxy scheme".into())
}

fn safe_proxy_label(input: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(input) else {
        return "configured proxy".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.to_string()
}

async fn check_provider_connectivity(
    proxy_url: &str,
    configured_providers: &[ProviderConfig],
) -> Result<(), String> {
    let proxy =
        reqwest::Proxy::all(proxy_url).map_err(|_| "invalid proxy configuration".to_string())?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("creating proxy client: {}", error.without_url()))?;
    let mut targets = vec![("Kilo Gateway".to_string(), KILO_BASE_URL.to_string())];
    targets.extend(
        configured_providers
            .iter()
            .filter(|provider| provider.id != "kilo")
            .map(|provider| (provider.name.clone(), provider.base_url.clone())),
    );
    if configured_providers
        .iter()
        .all(|provider| provider.id != "openrouter")
        && (env::var("OPENROUTER_API_KEY").is_ok() || env::var("NIO_OPENROUTER_API_KEY").is_ok())
    {
        targets.push(("OpenRouter".into(), OPENROUTER_BASE_URL.into()));
    }
    println!(
        "Checking {} configured provider endpoint(s) without sending API keys...",
        targets.len()
    );
    let results = futures_util::future::join_all(targets.iter().map(|(name, base_url)| {
        let client = &client;
        async move {
            let result = probe_provider_models(client, base_url).await;
            (name, result)
        }
    }))
    .await;
    for (name, result) in results {
        match result {
            Ok((status, _server))
                if status.is_success() || status == reqwest::StatusCode::UNAUTHORIZED =>
            {
                println!("  {name}: reachable (HTTP {status})");
            }
            Ok((status, server)) => {
                let server = server
                    .map(|value| format!(", server: {value}"))
                    .unwrap_or_default();
                println!("  {name}: HTTP {status}{server}");
            }
            Err(error) => println!("  {name}: connection failed ({error})"),
        }
    }
    println!(
        "HTTP 401 usually means the endpoint is reachable; this check does not test provider authentication."
    );
    Ok(())
}

async fn probe_provider_models(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<(reqwest::StatusCode, Option<String>), String> {
    let response = client
        .get(endpoint(base_url, "models"))
        .send()
        .await
        .map_err(|error| format!("{:?}", error.without_url()))?;
    let server = response
        .headers()
        .get(reqwest::header::SERVER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    Ok((response.status(), server))
}

fn read_provider_key(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .map_err(|error| format!("writing provider key prompt: {error}"))?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| format!("reading provider key: {error}"))?;
    Ok(value.trim_end().to_string())
}

fn configure_settings() -> Result<(), String> {
    let mut config = load_user_config()?;
    let followups_enabled = config.follow_up_suggestions.unwrap_or(true);
    let mode = configured_agent_mode(&config);
    let effort = config
        .reasoning_effort
        .as_deref()
        .unwrap_or("provider default");
    println!("Settings");
    println!(
        "  1) Minimum delay between model requests: {}s",
        config
            .request_interval_seconds
            .unwrap_or(DEFAULT_REQUEST_INTERVAL_SECONDS)
    );
    println!(
        "  2) Follow-up suggestions: {}",
        if followups_enabled { "On" } else { "Off" }
    );
    println!("  3) Agent mode: {}", title_case(mode));
    println!("  4) Reasoning effort: {}", title_case(effort));
    let auto_approve = config.auto_approve_actions.unwrap_or(false);
    println!(
        "  5) Auto-approve writes and commands: {}",
        if auto_approve { "On" } else { "Off" }
    );
    print!("Choose a setting [1-5] or Enter to cancel: ");
    io::stdout()
        .flush()
        .map_err(|error| format!("writing settings menu: {error}"))?;
    let mut selection = String::new();
    io::stdin()
        .read_line(&mut selection)
        .map_err(|error| format!("reading settings choice: {error}"))?;
    match selection.trim() {
        "1" => configure_request_interval(&mut config),
        "2" => {
            config.follow_up_suggestions = Some(!followups_enabled);
            save_user_config(&config)?;
            println!(
                "Follow-up suggestions {}.",
                if !followups_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            Ok(())
        }
        "3" => configure_agent_mode(),
        "4" => configure_reasoning_effort(),
        "5" => toggle_auto_approval(),
        "" => Ok(()),
        _ => {
            eprintln!("Choose 1–5. Settings unchanged.");
            Ok(())
        }
    }
}

fn toggle_auto_approval() -> Result<(), String> {
    let mut config = load_user_config()?;
    let enabled = !config.auto_approve_actions.unwrap_or(false);
    config.auto_approve_actions = Some(enabled);
    save_user_config(&config)?;
    if enabled {
        println!("Automatic approval enabled: file writes and shell commands run without asking.");
    } else {
        println!("Automatic approval disabled: Nio asks before file writes and shell commands.");
    }
    Ok(())
}

fn configure_agent_mode() -> Result<(), String> {
    let mut config = load_user_config()?;
    let current = configured_agent_mode(&config);
    println!("Agent mode (current: {})", title_case(current));
    println!("  1) Ask   Answer questions; inspect files for context, no changes or commands");
    println!("  2) Plan  Inspect files and return a plan; no changes or commands");
    println!("  3) Build Implement changes; ask before edits and commands");
    print!("Choose mode [1-3] or Enter to keep: ");
    io::stdout()
        .flush()
        .map_err(|error| format!("writing mode prompt: {error}"))?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| format!("reading agent mode: {error}"))?;
    let mode = match value.trim() {
        "1" => "ask",
        "2" => "plan",
        "3" => "build",
        "" => return Ok(()),
        _ => {
            eprintln!("Choose 1, 2, or 3. Mode unchanged.");
            return Ok(());
        }
    };
    config.agent_mode = Some(mode.to_string());
    save_user_config(&config)?;
    println!("Agent mode set to {}.", title_case(mode));
    Ok(())
}

fn cycle_agent_mode() -> Result<(), String> {
    let mut config = load_user_config()?;
    let current = configured_agent_mode(&config);
    let next = match current {
        "ask" => "plan",
        "plan" => "build",
        _ => "ask",
    };
    config.agent_mode = Some(next.to_string());
    save_user_config(&config)?;
    Ok(())
}

fn configure_reasoning_effort() -> Result<(), String> {
    let mut config = load_user_config()?;
    let current = config
        .reasoning_effort
        .as_deref()
        .unwrap_or("provider default");
    println!("Reasoning effort (current: {})", title_case(current));
    println!("  1) Low      Faster, lighter reasoning");
    println!("  2) Medium   Balanced reasoning");
    println!("  3) High     More detailed reasoning");
    println!("  4) Provider default   Let the model provider decide");
    print!("Choose effort [1-4] or Enter to keep: ");
    io::stdout()
        .flush()
        .map_err(|error| format!("writing effort prompt: {error}"))?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| format!("reading reasoning effort: {error}"))?;
    let effort = match value.trim() {
        "1" => Some("low"),
        "2" => Some("medium"),
        "3" => Some("high"),
        "4" => None,
        "" => return Ok(()),
        _ => {
            eprintln!("Choose 1, 2, 3, or 4. Effort unchanged.");
            return Ok(());
        }
    };
    config.reasoning_effort = effort.map(str::to_string);
    save_user_config(&config)?;
    println!(
        "Reasoning effort set to {}.",
        effort
            .map(title_case)
            .unwrap_or_else(|| "provider default".to_string())
    );
    Ok(())
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

fn configure_request_interval(config: &mut UserConfig) -> Result<(), String> {
    let current = config
        .request_interval_seconds
        .unwrap_or(DEFAULT_REQUEST_INTERVAL_SECONDS);
    println!("Minimum delay between model requests: {current}s");
    print!("New delay in seconds (0–60, Enter to keep): ");
    io::stdout()
        .flush()
        .map_err(|error| format!("writing settings prompt: {error}"))?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| format!("reading setting: {error}"))?;
    let value = value.trim();
    if value.is_empty() {
        println!("Keeping {current}s.");
        return Ok(());
    }
    let Ok(seconds) = value.parse::<u64>() else {
        eprintln!("Enter a whole number from 0 to 60. Setting unchanged.");
        return Ok(());
    };
    if seconds > 60 {
        eprintln!("Request delay must be between 0 and 60 seconds. Setting unchanged.");
        return Ok(());
    }
    config.request_interval_seconds = Some(seconds);
    save_user_config(&config)?;
    println!("Saved request delay: {seconds}s.");
    Ok(())
}

fn endpoint(base: &str, suffix: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        suffix.trim_start_matches('/')
    )
}

fn split_model_selector(model: &str) -> Result<(Option<&str>, &str), String> {
    if let Some((gateway, model_id)) = model.split_once("::") {
        if gateway.is_empty() || model_id.is_empty() {
            return Err("model selector must be written as gateway::model-id".into());
        }
        Ok((Some(gateway), model_id))
    } else if let Some(model_id) = model.strip_prefix("kilo/") {
        Ok((Some("kilo"), model_id))
    } else if let Some(model_id) = model.strip_prefix("openrouter/") {
        Ok((Some("openrouter"), model_id))
    } else {
        Ok((None, model))
    }
}

fn model_api_key(options: &Options, gateway: &str) -> Option<String> {
    let explicit = options
        .api_key
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let env_key = match gateway {
        "kilo" => env::var("KILO_API_KEY").ok(),
        "openrouter" => env::var("OPENROUTER_API_KEY").ok(),
        "orca" => env::var("ORCA_API_KEY")
            .ok()
            .or_else(|| env::var("NIO_ORCA_API_KEY").ok()),
        "claude" => env::var("ANTHROPIC_API_KEY")
            .ok()
            .or_else(|| env::var("NIO_CLAUDE_API_KEY").ok()),
        "codex" => env::var("OPENAI_API_KEY")
            .ok()
            .or_else(|| env::var("NIO_CODEX_API_KEY").ok()),
        other => env::var(format!(
            "NIO_{}_API_KEY",
            other
                .to_ascii_uppercase()
                .replace('-', "_")
                .replace('.', "_")
        ))
        .ok(),
    };
    let saved_key = load_user_config().ok().and_then(|config| {
        config
            .providers
            .into_iter()
            .find(|provider| provider.id == gateway)
            .and_then(|provider| provider.api_key)
    });
    env_key
        .filter(|key| !key.trim().is_empty())
        .or(saved_key.filter(|key| !key.trim().is_empty()))
        .or_else(|| explicit.map(str::to_string))
}

fn resolve_model_provider(
    options: &Options,
    gateway: Option<&str>,
    model_id: &str,
) -> Result<(String, Option<String>), String> {
    let Some(gateway) = gateway.or_else(|| model_id.starts_with("kilo-auto/").then_some("kilo"))
    else {
        return Ok((options.base_url.clone(), options.api_key.clone()));
    };
    let config = load_user_config()?;
    let saved = config
        .providers
        .iter()
        .find(|provider| provider.id == gateway);
    let base_url = match gateway {
        "kilo" => KILO_BASE_URL.to_string(),
        "openrouter" => saved
            .map(|provider| provider.base_url.clone())
            .unwrap_or_else(|| OPENROUTER_BASE_URL.to_string()),
        other => saved
            .map(|provider| provider.base_url.clone())
            .ok_or_else(|| {
                format!("provider '{other}' is not configured; use :provider to add it")
            })?,
    };
    let key = model_api_key(options, gateway);
    Ok((base_url, key))
}

impl ModelInfo {
    fn is_free(&self) -> bool {
        if self.free == Some(true)
            || self.id.ends_with(":free")
            || self.id.ends_with("-free")
            || self.id == "kilo-auto/free"
        {
            return true;
        }
        let Some(pricing) = &self.pricing else {
            return false;
        };
        matches!(pricing.prompt.as_ref(), Some(value) if is_zero(value))
            && matches!(pricing.completion.as_ref(), Some(value) if is_zero(value))
    }
}

fn is_zero(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(number) => number.as_f64() == Some(0.0),
        serde_json::Value::String(text) => text.parse::<f64>().ok() == Some(0.0),
        _ => false,
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    format!("{}…", value.chars().take(max).collect::<String>())
}

fn format_provider_error(status: u16, body: &str, gateway: Option<&str>) -> String {
    if status == 429 {
        let own_key = match gateway {
            Some("openrouter") => "OPENROUTER_API_KEY",
            Some("kilo") => "KILO_API_KEY",
            _ => "NIO_API_KEY",
        };
        return format!(
            "This model is temporarily rate-limited by its provider (HTTP 429).\n\
             Try again in a few minutes, choose another model with `nio models`, or set your own provider key (`{own_key}`) to use your own limits."
        );
    }

    if matches!(status, 500 | 502 | 503 | 504) {
        return format!(
            "The model provider is temporarily unavailable (HTTP {status}). This model may be under high demand. Try again shortly or switch models with `:model`."
        );
    }

    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(provider_error_message)
        .unwrap_or_else(|| truncate(body.trim(), 500));
    if detail.is_empty() {
        format!("The provider returned HTTP {status}.")
    } else {
        format!("The provider returned HTTP {status}: {detail}")
    }
}

fn provider_error_message(value: Value) -> Option<String> {
    match value {
        Value::Array(values) => values.into_iter().find_map(provider_error_message),
        Value::Object(mut object) => {
            for key in ["error", "message", "detail"] {
                if let Some(value) = object.remove(key) {
                    if let Some(message) = provider_error_message(value) {
                        return Some(message);
                    }
                }
            }
            None
        }
        Value::String(message) if !message.trim().is_empty() => Some(message),
        _ => None,
    }
}

fn rate_limit_retry_delay(
    headers: &reqwest::header::HeaderMap,
    retry_count: u32,
) -> Option<std::time::Duration> {
    const MAX_SERVER_WAIT: u64 = 120;
    if let Some(seconds) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        return (seconds <= MAX_SERVER_WAIT).then(|| std::time::Duration::from_secs(seconds));
    }
    if let Some(reset) = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        let reset = if reset > 10_000_000_000 {
            reset / 1000
        } else {
            reset
        };
        let seconds = reset.saturating_sub(now);
        return (seconds <= MAX_SERVER_WAIT).then(|| std::time::Duration::from_secs(seconds));
    }
    Some(std::time::Duration::from_secs(2u64 << retry_count.min(2)))
}

fn print_help() {
    println!(
        "NioAI — a lightweight AI coding agent for the terminal\n\
\
Usage:\n\
  nio [OPTIONS]                 Start the interactive prompt UI\n\
  nio run [OPTIONS] <prompt>\n\
  nio models\n\
  nio provider                 Configure model providers\n\
  nio --help | --version (-v, --v)\n\
\
Options:\n\
  -m, --model <SELECTOR> Model selector from 'nio models' (or NIO_MODEL)\n\
  -s, --session <ID> Resume a saved conversation\n\
  --base-url <URL>   OpenAI-compatible API base URL (or NIO_BASE_URL)\n\
  --api-key <KEY>    API key (or NIO_API_KEY / OPENROUTER_API_KEY)\n\
  --format json      Emit NoIDE-compatible NDJSON events\n\
  --dir <PATH>       Set the project working directory\n\
  -s, --session <ID> Resume a persistent conversation session\n\
  --auto             Approve file writes and shell commands\n\
\n\
Interactive commands:\n\
  :clear             Clear conversation history\n\
  :model             Switch the active model (free catalog)\n\
  :bash              Switch to a direct shell command prompt (:ai returns)\n\
  :mode              Choose Ask, Plan, or Build mode\n\
  :approval          Toggle automatic approval for writes and commands\n\
  :reasoning         Set reasoning effort\n\
  :provider          Add or update an OpenAI-compatible provider\n\
  :proxy             Configure a proxy for model API requests\n\
  :path              Show the current project directory\n\
  :setting           Configure mode, reasoning, and approvals\n\
  :quit              Exit\n\
\
Example:\n\
  nio run -m kilo::kilo-auto/free \"Explain this project\""
    );
}
