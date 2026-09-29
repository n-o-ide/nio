use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use tokio::io::AsyncReadExt;

const KILO_BASE_URL: &str = "https://api.kilo.ai/api/gateway";
const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Debug)]
struct Options {
    command: String,
    prompt: Vec<String>,
    model: Option<String>,
    base_url: String,
    api_key: Option<String>,
    all_models: bool,
    json_output: bool,
    auto_approve: bool,
    workdir: Option<PathBuf>,
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
}

struct ModelChoice {
    id: String,
    name: String,
    gateway: &'static str,
    gateway_label: &'static str,
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
    let options = parse_args(env::args().skip(1).collect())?;
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
        "run" => chat(&options).await,
        command => Err(format!("unknown command '{command}'. Run 'nio --help'.")),
    }
}

fn parse_args(args: Vec<String>) -> Result<Options, String> {
    let mut args = args.into_iter();
    let first = args.next();
    let command = match first.as_deref() {
        None => "interactive".to_string(),
        Some("--help") | Some("-h") | Some("help") => "help".to_string(),
        Some("--version") | Some("-V") => {
            return Ok(Options {
                command: "version".to_string(),
                prompt: vec![],
                model: None,
                base_url: KILO_BASE_URL.to_string(),
                api_key: None,
                all_models: false,
                json_output: false,
                auto_approve: false,
                workdir: None,
            });
        }
        Some("run") => "run".to_string(),
        Some("models") => "models".to_string(),
        Some(prompt) => {
            let mut all = vec![prompt.to_string()];
            all.extend(args);
            return Ok(Options {
                command: "run".to_string(),
                prompt: all,
                model: env::var("NIO_MODEL").ok(),
                base_url: env::var("NIO_BASE_URL").unwrap_or_else(|_| KILO_BASE_URL.into()),
                api_key: env::var("NIO_API_KEY").ok(),
                all_models: false,
                json_output: false,
                auto_approve: false,
                workdir: None,
            });
        }
    };

    let mut prompt = Vec::new();
    let mut model = env::var("NIO_MODEL").ok();
    let mut base_url_override = env::var("NIO_BASE_URL").ok();
    let mut base_url = base_url_override
        .clone()
        .unwrap_or_else(|| KILO_BASE_URL.into());
    let mut api_key = env::var("NIO_API_KEY").ok();
    let mut all_models = false;
    let mut json_output = false;
    let mut auto_approve = false;
    let mut workdir = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" | "-m" => model = Some(args.next().ok_or("--model requires a value")?),
            "--base-url" => {
                base_url = args.next().ok_or("--base-url requires a value")?;
                base_url_override = Some(base_url.clone());
            }
            "--api-key" => api_key = Some(args.next().ok_or("--api-key requires a value")?),
            "--all" => all_models = true,
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
                let _ = args.next().ok_or("--session requires a value")?;
            }
            "--help" | "-h" => {
                return Ok(Options {
                    command: "help".into(),
                    prompt,
                    model,
                    base_url,
                    api_key,
                    all_models,
                    json_output,
                    auto_approve,
                    workdir,
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
        all_models,
        json_output,
        auto_approve,
        workdir,
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
    run_agent_turn(options, &model, &prompt, &mut Vec::new()).await
}

fn agent_tools() -> Value {
    json!([
        {"type":"function","function":{"name":"list_files","description":"List files under a project directory.","parameters":{"type":"object","properties":{"path":{"type":"string","description":"Project-relative directory, default ."}},"additionalProperties":false}}},
        {"type":"function","function":{"name":"read_file","description":"Read a UTF-8 text file in the project.","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"search_files","description":"Search project text files for a literal string.","parameters":{"type":"object","properties":{"query":{"type":"string"},"path":{"type":"string","description":"Optional project-relative directory, default ."}},"required":["query"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"write_file","description":"Create or replace a project file. Requires user approval.","parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"run_command","description":"Run a shell command in the project. Requires user approval.","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"],"additionalProperties":false}}}
    ])
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

fn tool_definitions() -> Value {
    agent_tools()
}

fn process_sse_line(
    line: &str,
    options: &Options,
    answer: &mut String,
    tools: &mut std::collections::BTreeMap<usize, PendingToolCall>,
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
            answer.push_str(&content);
            emit_text(options, &content)?;
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

fn emit_status(options: &Options, status: &str, message: &str) {
    if options.json_output {
        // NoIDE understands this OpenCode/Kilo-compatible reasoning event.
        emit_json(
            &json!({"type":"reasoning","part":{"type":"reasoning","text":format!("{status}: {message}")}}),
        );
    } else {
        eprintln!("[{status}] {message}");
    }
}

fn emit_text(options: &Options, text: &str) -> Result<(), String> {
    if options.json_output {
        emit_json(&json!({"type":"text","part":{"type":"text","text":text}}));
    } else {
        print!("{text}");
        io::stdout()
            .flush()
            .map_err(|e| format!("writing response: {e}"))?;
    }
    Ok(())
}

fn emit_json(value: &Value) {
    println!("{value}");
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
            std::fs::read_to_string(&path)
                .map_err(|e| format!("file is not readable UTF-8 text: {e}"))
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
            if !confirm_tool(
                auto_approve,
                &format!("Write {} ({} bytes)", path.display(), content.len()),
            )? {
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
            if !confirm_tool(auto_approve, &format!("Run command: {command}"))? {
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
    eprint!("\nApprove {action}? [y/N] ");
    io::stderr()
        .flush()
        .map_err(|e| format!("writing approval prompt: {e}"))?;
    let mut answer = String::new();
    let bytes = io::stdin()
        .read_line(&mut answer)
        .map_err(|e| format!("reading approval: {e}"))?;
    Ok(bytes > 0 && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

async fn run_agent_turn(
    options: &Options,
    model: &str,
    prompt: &str,
    history: &mut Vec<Value>,
) -> Result<(), String> {
    let (gateway, model_id) = split_model_selector(model)?;
    let base_url = match gateway {
        Some("kilo") => KILO_BASE_URL,
        Some("openrouter") => OPENROUTER_BASE_URL,
        Some(other) => return Err(format!("unknown model gateway '{other}'")),
        None if model_id.starts_with("kilo-auto/") => KILO_BASE_URL,
        None => options.base_url.as_str(),
    };
    let key = match gateway {
        Some(provider) => model_api_key(options, provider),
        None if base_url == KILO_BASE_URL => model_api_key(options, "kilo"),
        None => options.api_key.clone(),
    };
    let client = reqwest::Client::new();
    if key.is_none() && !supports_anonymous_gateway(base_url) {
        let hint = match gateway {
            Some("kilo") => "this endpoint requires an API key; set KILO_API_KEY or NIO_API_KEY",
            Some("openrouter") => "set OPENROUTER_API_KEY or NIO_API_KEY to use OpenRouter models",
            _ => "this endpoint requires an API key; set NIO_API_KEY",
        };
        return Err(hint.into());
    }

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
    emit_status(options, "exploring", "Scanning project files");
    let system = format!(
        "You are NioAI, a coding agent working in the project at {}. You can inspect, search, and change project files with the provided tools. Start by inspecting the relevant files; do not claim you cannot access the project. Read and search tools are automatic. Before writing files or executing shell commands, call the tool: Nio will ask the user for approval unless auto-approval was explicitly enabled. Stay within the project directory. Be concise and report what you changed.",
        root.display()
    );
    let overview = project_overview(&root);
    let mut messages = vec![
        json!({"role":"system", "content": format!("{system}\n\nProject overview:\n{overview}")}),
    ];
    if history.len() > 32 {
        history.drain(..history.len() - 32);
    }
    messages.extend(history.iter().cloned());
    let user_message = json!({"role":"user", "content":prompt});
    messages.push(user_message.clone());
    history.push(user_message);

    let url = endpoint(base_url, "chat/completions");
    for _ in 0..8 {
        emit_status(options, "thinking", "Thinking");
        let mut request = client.post(&url).json(&json!({
            "model": model_id,
            "messages": messages.clone(),
            "tools": tool_definitions(),
            "tool_choice": "auto",
            "stream": true
        }));
        if let Some(key) = key.as_deref() {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "provider returned {status}: {}",
                truncate(&body, 1200)
            ));
        }
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut answer = String::new();
        let mut pending_tools = std::collections::BTreeMap::<usize, PendingToolCall>::new();
        while let Some(part) = stream.next().await {
            let bytes = part.map_err(|e| format!("response stream failed: {e}"))?;
            buffer.extend_from_slice(&bytes);
            while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = String::from_utf8_lossy(&buffer[..pos])
                    .trim_end_matches('\r')
                    .to_string();
                buffer.drain(..=pos);
                process_sse_line(&line, options, &mut answer, &mut pending_tools)?;
            }
        }
        if !buffer.is_empty() {
            let line = String::from_utf8_lossy(&buffer)
                .trim_end_matches('\r')
                .to_string();
            process_sse_line(&line, options, &mut answer, &mut pending_tools)?;
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
        if calls.is_empty() {
            emit_status(options, "working", "Finishing response");
            if options.json_output {
                emit_json(&json!({"type":"step_finish"}));
            } else {
                println!();
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
            let result = execute_agent_tool(&root, &call, options.auto_approve).await;
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
    Err("stopped after 8 tool rounds; please narrow the request".into())
}

async fn list_models(options: &Options) -> Result<(), String> {
    let choices = fetch_model_choices(options, options.all_models).await?;
    for choice in choices {
        println!(
            "{} ({})\t{}",
            choice.name,
            choice.gateway_label,
            choice.selector()
        );
    }
    Ok(())
}

async fn fetch_model_choices(
    options: &Options,
    include_paid: bool,
) -> Result<Vec<ModelChoice>, String> {
    let client = reqwest::Client::new();
    let kilo_key = model_api_key(options, "kilo");
    let kilo = fetch_models(&client, KILO_BASE_URL, kilo_key.as_deref()).await?;
    let mut choices = choices_from_catalog(kilo.data, "kilo", "Kilo Gateway", include_paid);

    if let Some(openrouter_key) = env::var("OPENROUTER_API_KEY")
        .ok()
        .or_else(|| options.api_key.clone())
    {
        let openrouter = fetch_models(&client, OPENROUTER_BASE_URL, Some(&openrouter_key)).await?;
        choices.extend(choices_from_catalog(
            openrouter.data,
            "openrouter",
            "OpenRouter",
            include_paid,
        ));
    }
    choices.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
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

fn choices_from_catalog(
    models: Vec<ModelInfo>,
    gateway: &'static str,
    label: &'static str,
    include_paid: bool,
) -> Vec<ModelChoice> {
    let mut choices = Vec::new();
    let mut has_kilo_auto_free = false;
    for model in models {
        has_kilo_auto_free |= gateway == "kilo" && model.id == "kilo-auto/free";
        if include_paid || model.is_free() {
            choices.push(ModelChoice {
                name: model.name.unwrap_or_else(|| model.id.clone()),
                id: model.id,
                gateway,
                gateway_label: label,
            });
        }
    }
    if gateway == "kilo" && !include_paid && !has_kilo_auto_free {
        choices.push(ModelChoice {
            id: "kilo-auto/free".into(),
            name: "Kilo Auto Free".into(),
            gateway,
            gateway_label: label,
        });
    }
    choices
}

async fn interactive(options: Options) -> Result<(), String> {
    let model = chosen_model(&options).await?;
    let mut history = Vec::new();
    println!("NioAI · model {model}");
    println!("Project tools are available automatically. Type :help for commands.");

    loop {
        print!("\nnio> ");
        io::stdout()
            .flush()
            .map_err(|e| format!("writing prompt: {e}"))?;
        let mut line = String::new();
        if io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("reading prompt: {e}"))?
            == 0
        {
            break;
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input == ":quit" || input == ":q" || input == ":exit" {
            break;
        }
        if input == ":help" {
            println!("Commands: :clear (clear conversation), :quit");
            continue;
        }
        if input == ":clear" {
            history.clear();
            println!("Cleared conversation history.");
            continue;
        }
        if input.starts_with(':') {
            eprintln!("Unknown command. Type :help for commands.");
            continue;
        }
        run_agent_turn(&options, &model, input, &mut history).await?;
    }
    Ok(())
}

async fn chosen_model(options: &Options) -> Result<String, String> {
    if let Some(model) = options.model.as_deref() {
        Ok(model.to_string())
    } else if let Some(model) = read_saved_model()? {
        Ok(model)
    } else {
        select_and_save_model(options).await
    }
}

async fn select_and_save_model(options: &Options) -> Result<String, String> {
    println!("Choose your default model (free models are shown):");
    let choices = fetch_model_choices(options, false).await?;
    if choices.is_empty() {
        return Err("no free models are available from the configured gateways".into());
    }
    for (index, choice) in choices.iter().enumerate() {
        println!(
            "  {}) {} ({})",
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
    let choice = choices
        .get(
            index
                .checked_sub(1)
                .ok_or("model selection is out of range")?,
        )
        .ok_or("model selection is out of range")?;
    let selector = choice.selector();
    save_default_model(&selector)?;
    println!(
        "Saved default model: {} ({})",
        choice.name, choice.gateway_label
    );
    Ok(selector)
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
    let path = config_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("reading {}: {error}", path.display())),
    };
    let config: UserConfig = serde_json::from_str(&contents)
        .map_err(|error| format!("invalid config at {}: {error}", path.display()))?;
    Ok(config.default_model)
}

fn save_default_model(model: &str) -> Result<(), String> {
    let path = config_path()?;
    let parent = path
        .parent()
        .ok_or("config file path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    let contents = serde_json::to_vec_pretty(&UserConfig {
        default_model: Some(model.to_string()),
    })
    .map_err(|error| format!("serializing config: {error}"))?;
    std::fs::write(&path, contents).map_err(|error| format!("writing {}: {error}", path.display()))
}

fn endpoint(base: &str, suffix: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        suffix.trim_start_matches('/')
    )
}

fn supports_anonymous_gateway(base: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base) else {
        return false;
    };
    // Kilo limits unauthenticated access to free models. Its catalog may mark
    // free models without the `:free` suffix, so let the gateway enforce access.
    url.scheme() == "https"
        && url.host_str() == Some("api.kilo.ai")
        && url.path().trim_end_matches('/') == "/api/gateway"
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
    let provider_key = match gateway {
        "kilo" => env::var("KILO_API_KEY").ok(),
        "openrouter" => env::var("OPENROUTER_API_KEY").ok(),
        _ => None,
    };
    provider_key.or_else(|| explicit.map(str::to_string))
}

impl ModelInfo {
    fn is_free(&self) -> bool {
        if self.free == Some(true) || self.id.ends_with(":free") || self.id == "kilo-auto/free" {
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

fn print_help() {
    println!(
        "NioAI — a lightweight AI coding agent for the terminal\n\
\
Usage:\n\
  nio [OPTIONS]                 Start the interactive prompt UI\n\
  nio run [OPTIONS] <prompt>\n\
  nio models [--all]\n\
  nio --help | --version\n\
\
Options:\n\
  -m, --model <SELECTOR> Model selector from 'nio models' (or NIO_MODEL)\n\
      --all              Include paid models in model listing\n\
  --base-url <URL>   OpenAI-compatible API base URL (or NIO_BASE_URL)\n\
  --api-key <KEY>    API key (or NIO_API_KEY / OPENROUTER_API_KEY)\n\
  --format json      Emit NoIDE-compatible NDJSON events\n\
  --dir <PATH>       Set the project working directory\n\
  --auto             Approve file writes and shell commands\n\
\n\
Interactive commands:\n\
  :clear             Clear conversation history\n\
  :quit              Exit\n\
\
Example:\n\
  nio run -m kilo::kilo-auto/free \"Explain this project\""
    );
}
