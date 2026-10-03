use super::*;
use regex_lite::RegexBuilder;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

const RESULT_LIMIT: usize = 8_000;
const FILE_SCAN_LIMIT: usize = 16 * 1024 * 1024;
const WEB_BODY_LIMIT: usize = 1024 * 1024;
fn text_excerpt(chars: impl Iterator<Item = char>, max_chars: usize, budget: usize) -> String {
    let mut text = String::new();
    let mut encoded_bytes = 0;
    for ch in chars.take(max_chars) {
        let cost = match ch {
            '\n' | '\r' | '\t' | '"' | '\\' => 2,
            ch if ch.is_control() => 6,
            _ => ch.len_utf8(),
        };
        if encoded_bytes + cost > budget {
            break;
        }
        text.push(ch);
        encoded_bytes += cost;
    }
    text
}

fn limited_usize(args: &Value, name: &str, default: usize, max: usize) -> usize {
    args.get(name)
        .and_then(Value::as_u64)
        .map(|value| (value as usize).min(max))
        .unwrap_or(default)
}

fn candidate_files(root: &Path, args: &Value) -> Result<(Vec<String>, bool), String> {
    let input = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let path = resolve_project_path(root, input, true).map_err(|error| {
        format!("{error}. Requested path: '{}'; active project: '{}'. Use path '.' to search this project, or start Nio with --dir /path/to/project to search another folder.", input, root.display())
    })?;
    if path.is_file() {
        return Ok((
            vec![
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            ],
            false,
        ));
    }
    if !path.is_dir() {
        return Err("path must be a project file or directory".into());
    }
    let mut files = Vec::new();
    collect_files(root, &path, 0, &mut files, 10_000);
    let truncated = files.len() >= 10_000;
    Ok((files, truncated))
}

fn glob_filter(args: &Value) -> Result<Option<&str>, String> {
    let glob = args.get("glob").and_then(Value::as_str).unwrap_or("");
    if glob.len() > 200 {
        return Err("glob must be at most 200 characters".into());
    }
    Ok((!glob.is_empty()).then_some(glob))
}

fn glob_matches_path(glob: Option<&str>, path: &str) -> bool {
    glob.is_none_or(|pattern| {
        let target = if pattern.contains('/') {
            path
        } else {
            path.rsplit('/').next().unwrap_or(path)
        };
        glob_matches(pattern, target)
    })
}

pub fn find_files(root: &Path, args: &Value) -> Result<String, String> {
    let (files, scan_truncated) = candidate_files(root, args)?;
    let glob = glob_filter(args)?;
    let limit = limited_usize(args, "limit", 30, 50).max(1);
    let offset = limited_usize(args, "offset", 0, 10_000);
    let mut matches = Vec::new();
    let mut seen = 0;
    let mut output_bytes = 0;
    let mut more = false;
    for file in files {
        if !glob_matches_path(glob, &file) {
            continue;
        }
        if seen >= offset {
            let bytes = serde_json::to_string(&file).unwrap().len();
            if matches.len() >= limit || output_bytes + bytes > RESULT_LIMIT - 256 {
                more = true;
                break;
            }
            output_bytes += bytes;
            matches.push(file);
        }
        seen += 1;
    }
    Ok(json!({"files": matches, "next_offset": more.then_some(offset + matches.len()), "scan_limited": scan_truncated}).to_string())
}

pub fn search_code(root: &Path, args: &Value, cancelled: &AtomicBool) -> Result<String, String> {
    let query = required_arg(args, "query")?;
    if query.is_empty() || query.len() > 500 {
        return Err("query must be 1-500 characters".into());
    }
    let expression = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let matcher = if expression {
        Some(
            RegexBuilder::new(query)
                .case_insensitive(!case_sensitive)
                .size_limit(256 * 1024)
                .build()
                .map_err(|error| format!("invalid regex: {error}"))?,
        )
    } else {
        None
    };
    let literal = (!case_sensitive).then(|| query.to_lowercase());
    let glob = glob_filter(args)?;
    let (files, scan_limited) = candidate_files(root, args)?;
    let limit = limited_usize(args, "limit", 20, 50).max(1);
    let offset = limited_usize(args, "offset", 0, 10_000);
    let context = limited_usize(args, "context_lines", 1, 2);
    let mut matches = Vec::new();
    let mut seen = 0;
    let mut read_bytes = 0;
    let mut output_bytes = 0;
    let mut limited = scan_limited;
    'files: for file in files {
        if cancelled.load(Ordering::SeqCst) {
            limited = true;
            break;
        }
        if !glob_matches_path(glob, &file) {
            continue;
        }
        let Ok(path) = resolve_project_path(root, &file, true) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() > 512 * 1024 {
            continue;
        }
        if read_bytes + meta.len() as usize > FILE_SCAN_LIMIT {
            limited = true;
            break;
        }
        let Ok(bytes) = read_bounded(&path, FILE_LIMIT) else {
            continue;
        };
        read_bytes += bytes.len();
        if bytes.contains(&0) {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let found = matcher
                .as_ref()
                .map(|regex| regex.is_match(line))
                .unwrap_or_else(|| {
                    if case_sensitive {
                        line.contains(query)
                    } else {
                        line.to_lowercase()
                            .contains(literal.as_deref().unwrap_or(query))
                    }
                });
            if !found {
                continue;
            }
            if seen >= offset && matches.len() < limit {
                let start = index.saturating_sub(context);
                let end = (index + context + 1).min(lines.len());
                let excerpt = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(row, text)| format!("{}: {}", start + row + 1, truncate(text, 220)))
                    .collect::<Vec<_>>()
                    .join("\n");
                let row = json!({"path":file,"line":index + 1,"excerpt":excerpt});
                output_bytes += row.to_string().len();
                if output_bytes > RESULT_LIMIT - 256 {
                    limited = true;
                    break 'files;
                }
                matches.push(row);
            }
            seen += 1;
            if seen > offset + limit {
                limited = true;
                break 'files;
            }
        }
    }
    Ok(json!({"matches":matches,"next_offset":limited.then_some(offset + matches.len()),"scan_limited":limited}).to_string())
}

fn web_url(raw: &str) -> Result<reqwest::Url, String> {
    if raw.len() > 2048 {
        return Err("URL must be at most 2048 bytes".into());
    }
    let url = reqwest::Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("URL must use http or https".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL cannot contain credentials".into());
    }
    Ok(url)
}

async fn web_bytes(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if !response.status().is_success() {
        return Err(format!(
            "HTTP {} from {}",
            response.status(),
            response.url()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("response exceeds {limit} bytes"));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| web_request_error("reading webpage response", e))?;
        if bytes.len() + chunk.len() > limit {
            return Err(format!("response exceeds {limit} bytes"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest.find(';').filter(|end| *end < 16) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|digits| u32::from_str_radix(digits, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|digits| digits.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };
        if let Some(character) = decoded {
            out.push(character);
        } else {
            out.push_str(&rest[..=end]);
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut result = String::new();
    let mut cursor = 0;
    let mut skip = None::<&str>;
    while cursor < html.len() {
        let Some(start) = html[cursor..].find('<').map(|index| cursor + index) else {
            if skip.is_none() {
                result.push_str(&decode_entities(&html[cursor..]));
            }
            break;
        };
        if skip.is_none() {
            result.push_str(&decode_entities(&html[cursor..start]));
        }
        if lower[start..].starts_with("<!--") {
            cursor = lower[start + 4..]
                .find("-->")
                .map(|offset| start + 4 + offset + 3)
                .unwrap_or(html.len());
            continue;
        }
        let mut quote = None;
        let Some(end) = html[start..].char_indices().find_map(|(index, ch)| {
            if let Some(active) = quote {
                if ch == active {
                    quote = None;
                }
            } else if ch == '\'' || ch == '"' {
                quote = Some(ch);
            } else if ch == '>' {
                return Some(start + index + 1);
            }
            None
        }) else {
            break;
        };
        let tag = lower[start + 1..end - 1].trim();
        let tag_name = tag
            .trim_start_matches('/')
            .split(|ch: char| ch.is_whitespace() || ch == '/')
            .next()
            .unwrap_or("");
        if let Some(name) = skip {
            if tag.starts_with('/') && tag_name == name {
                skip = None;
                result.push('\n');
            }
        } else if matches!(tag_name, "script" | "style" | "noscript" | "svg" | "head")
            && !tag.starts_with('/')
        {
            skip = Some(match tag_name {
                "script" => "script",
                "style" => "style",
                "noscript" => "noscript",
                "svg" => "svg",
                _ => "head",
            });
        } else if matches!(
            tag_name,
            "p" | "div"
                | "br"
                | "li"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "tr"
                | "section"
                | "article"
        ) {
            result.push('\n');
        }
        cursor = end;
    }
    result
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn web_request_error(action: &str, error: reqwest::Error) -> String {
    if error.is_timeout() {
        return format!(
            "{action} timed out (20-second request limit). No complete page was read. Try another source URL; do not repeatedly fetch the same failing URL."
        );
    }
    format!("{action} failed: {error}. No complete page was read; try another source.")
}

fn web_client() -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("NioAI/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(5));
    if let Some(proxy_url) = configured_proxy_url()? {
        builder =
            builder.proxy(reqwest::Proxy::all(proxy_url).map_err(|_| "invalid web proxy URL")?);
    }
    builder
        .build()
        .map_err(|e| format!("building web client: {e}"))
}

pub async fn web_fetch(args: &Value) -> Result<String, String> {
    let url = web_url(required_arg(args, "url")?)?;
    let offset = limited_usize(args, "offset", 0, WEB_BODY_LIMIT);
    let max_chars = limited_usize(args, "max_chars", 6_000, 8_000).max(1);
    let client = web_client()?;
    let response = client
        .get(url)
        .header("Accept", "text/html,text/plain,application/json")
        .header("Accept-Encoding", "identity")
        .send()
        .await
        .map_err(|e| web_request_error("fetching webpage", e))?;
    let final_url = response.url().to_string();
    if final_url.len() > 2048 {
        return Err("redirected URL exceeds 2048 bytes".into());
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if !content_type.is_empty()
        && !content_type.contains("text/")
        && !content_type.contains("json")
        && !content_type.contains("xml")
    {
        return Err(format!("unsupported content type: {content_type}"));
    }
    let body = String::from_utf8_lossy(&web_bytes(response, WEB_BODY_LIMIT).await?).into_owned();
    let text = if content_type.contains("html") || body.trim_start().starts_with("<!DOCTYPE html") {
        html_to_text(&body)
    } else {
        body
    };
    let total_chars = text.chars().count();
    if offset > total_chars {
        return Err(format!(
            "offset {offset} is past the end of this page ({total_chars} characters)"
        ));
    }
    let budget = RESULT_LIMIT.saturating_sub(final_url.len() * 2 + 256);
    let excerpt = text_excerpt(text.chars().skip(offset), max_chars, budget);
    let end = offset + excerpt.chars().count();
    Ok(json!({"url":final_url,"text":excerpt,"total_chars":total_chars,"next_offset":(end < total_chars).then_some(end)}).to_string())
}

pub fn ask_user(args: &Value) -> Result<String, String> {
    let question = required_arg(args, "question")?.trim();
    if question.is_empty() || question.len() > 500 {
        return Err("question must be 1-500 characters".into());
    }
    let options = args["options"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .take(3)
                .map(|value| truncate(value, 100))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let prompt = if options.is_empty() {
        question.to_string()
    } else {
        format!(
            "{}\n{}",
            question,
            options
                .iter()
                .enumerate()
                .map(|(i, answer)| format!("{}. {}", i + 1, answer))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    Ok(json!({"question":prompt,"needs_user_input":true}).to_string())
}

#[derive(Default)]
struct TerminalOutput {
    text: String,
    start: u64,
    running: bool,
    exit_code: Option<i32>,
}

struct TerminalSession {
    root: PathBuf,
    output: Arc<Mutex<TerminalOutput>>,
    cancel: Arc<AtomicBool>,
    pid: u32,
    cursor: AtomicU64,
}

static TERMINAL_SESSIONS: OnceLock<Mutex<HashMap<String, TerminalSession>>> = OnceLock::new();
static TERMINAL_ID: AtomicU64 = AtomicU64::new(0);

fn sessions() -> &'static Mutex<HashMap<String, TerminalSession>> {
    TERMINAL_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn append_terminal(output: &Arc<Mutex<TerminalOutput>>, stream: &str, bytes: &[u8]) {
    let Ok(mut state) = output.lock() else {
        return;
    };
    state.text.push_str(stream);
    state.text.push_str(&String::from_utf8_lossy(bytes));
    let excess = state.text.len().saturating_sub(64 * 1024);
    if excess > 0 {
        let mut end = excess;
        while !state.text.is_char_boundary(end) {
            end += 1;
        }
        state.text.drain(..end);
        state.start += end as u64;
    }
}

fn read_terminal_pipe<R: std::io::Read + Send + 'static>(
    mut pipe: R,
    stream: &'static str,
    output: Arc<Mutex<TerminalOutput>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let mut pending = Vec::new();
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => {
                    if !pending.is_empty() {
                        append_terminal(&output, stream, &pending);
                    }
                    break;
                }
                Ok(length) => {
                    pending.extend_from_slice(&buf[..length]);
                    loop {
                        match std::str::from_utf8(&pending) {
                            Ok(_) => {
                                append_terminal(&output, stream, &pending);
                                pending.clear();
                                break;
                            }
                            Err(error) => {
                                let valid = error.valid_up_to();
                                let invalid = error.error_len();
                                if valid > 0 {
                                    append_terminal(&output, stream, &pending[..valid]);
                                    pending.drain(..valid);
                                }
                                if let Some(length) = invalid {
                                    append_terminal(&output, stream, "�".as_bytes());
                                    pending.drain(..length);
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

fn kill_terminal_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

pub fn terminal_start(
    root: &Path,
    args: &Value,
    turn_cancel: Arc<AtomicBool>,
) -> Result<String, String> {
    let command = required_arg(args, "command")?;
    if command.trim().is_empty() || command.len() > 4_000 {
        return Err("command must be 1-4000 characters".into());
    }
    let timeout = limited_usize(args, "timeout_seconds", 600, 3600).max(1);
    let mut registry = sessions()
        .lock()
        .map_err(|_| "terminal session registry unavailable")?;
    if registry
        .values()
        .filter(|session| session.output.lock().is_ok_and(|state| state.running))
        .count()
        >= 4
    {
        return Err("four terminal commands are already running".into());
    }
    if registry.len() >= 16 {
        registry.retain(|_, session| session.output.lock().is_ok_and(|state| state.running));
    }
    #[cfg(unix)]
    let mut child_command = {
        use std::os::unix::process::CommandExt;
        let mut command_builder = std::process::Command::new("sh");
        command_builder.arg("-c").arg(command).process_group(0);
        command_builder
    };
    #[cfg(windows)]
    let mut child_command = {
        let mut command_builder = std::process::Command::new("cmd");
        command_builder.arg("/C").arg(command);
        command_builder
    };
    #[cfg(not(any(unix, windows)))]
    let mut child_command = {
        let mut command_builder = std::process::Command::new("sh");
        command_builder.arg("-c").arg(command);
        command_builder
    };
    let mut child = child_command
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting terminal command: {e}"))?;
    let pid = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or("capturing terminal stdout failed")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("capturing terminal stderr failed")?;
    let output = Arc::new(Mutex::new(TerminalOutput {
        running: true,
        ..Default::default()
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_output = output.clone();
    let worker_cancel = cancel.clone();
    thread::spawn(move || {
        let out_reader = read_terminal_pipe(stdout, "[stdout] ", worker_output.clone());
        let err_reader = read_terminal_pipe(stderr, "[stderr] ", worker_output.clone());
        let started = Instant::now();
        let mut code = -1;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    code = status.code().unwrap_or(-1);
                    break;
                }
                Err(error) => {
                    append_terminal(&worker_output, "[error] ", error.to_string().as_bytes());
                    break;
                }
                Ok(None) => {}
            }
            if worker_cancel.load(Ordering::SeqCst)
                || turn_cancel.load(Ordering::SeqCst)
                || started.elapsed() >= Duration::from_secs(timeout as u64)
            {
                kill_terminal_group(pid);
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            thread::sleep(Duration::from_millis(80));
        }
        // Background descendants must not hold output pipes open after the
        // command shell exits. Sessions own the complete process group.
        kill_terminal_group(pid);
        let _ = out_reader.join();
        let _ = err_reader.join();
        if let Ok(mut state) = worker_output.lock() {
            state.running = false;
            state.exit_code = Some(code);
        }
    });
    let id = format!(
        "term-{}-{}",
        std::process::id(),
        TERMINAL_ID.fetch_add(1, AtomicOrdering::Relaxed)
    );
    registry.insert(
        id.clone(),
        TerminalSession {
            root: root.to_path_buf(),
            output,
            cancel,
            pid,
            cursor: AtomicU64::new(0),
        },
    );
    Ok(json!({"session_id":id,"status":"started","next":"terminal_read"}).to_string())
}

pub async fn terminal_read(root: &Path, args: &Value) -> Result<String, String> {
    let id = required_arg(args, "session_id")?;
    let wait_ms = limited_usize(args, "wait_ms", 200, 1_000);
    let deadline = Instant::now() + Duration::from_millis(wait_ms as u64);
    loop {
        let registry = sessions()
            .lock()
            .map_err(|_| "terminal session registry unavailable")?;
        let session = registry.get(id).ok_or_else(|| format!(
            "Unknown terminal session {id:?}. terminal_read only reads output; it cannot run commands or edit files. Use the session_id returned by a successful terminal_start in this nio process. Sessions do not survive restarts. Do not retry this missing ID. If terminal_start is unavailable in Ask or Plan mode, explain that the user must choose Build with :mode to run commands."
        ))?;
        if session.root != root {
            return Err("terminal session belongs to a different project".into());
        }
        let state = session
            .output
            .lock()
            .map_err(|_| "terminal output unavailable")?;
        let requested = args
            .get("cursor")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| session.cursor.load(AtomicOrdering::Relaxed));
        let cursor = requested.max(state.start);
        let index = (cursor - state.start) as usize;
        let available = state.text.get(index..).ok_or("invalid terminal cursor")?;
        if !available.is_empty() || !state.running || Instant::now() >= deadline {
            let excerpt = text_excerpt(available.chars(), 6_000, RESULT_LIMIT - 512);
            let end = excerpt.len();
            let next_cursor = cursor + end as u64;
            session.cursor.store(next_cursor, AtomicOrdering::Relaxed);
            return Ok(json!({"session_id":id,"running":state.running,"exit_code":state.exit_code,
                "output":excerpt,"next_cursor":next_cursor,"more_output":end < available.len(),"dropped_before_cursor":requested < state.start}).to_string());
        }
        drop(state);
        drop(registry);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub fn terminal_cancel(root: &Path, args: &Value) -> Result<String, String> {
    let id = required_arg(args, "session_id")?;
    let registry = sessions()
        .lock()
        .map_err(|_| "terminal session registry unavailable")?;
    let session = registry.get(id).ok_or("unknown terminal session")?;
    if session.root != root {
        return Err("terminal session belongs to a different project".into());
    }
    session.cancel.store(true, Ordering::SeqCst);
    Ok(json!({"session_id":id,"cancel_requested":true}).to_string())
}

pub fn shutdown_terminals() {
    if let Some(registry) = TERMINAL_SESSIONS.get() {
        if let Ok(registry) = registry.lock() {
            for session in registry.values() {
                session.cancel.store(true, Ordering::SeqCst);
                if session.output.lock().is_ok_and(|state| state.running) {
                    kill_terminal_group(session.pid);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_extracts_visible_text() {
        let html = "<html><head><title>Ignored</title></head><body><h1>Hello &amp; world</h1><script>bad()</script><p>Useful&nbsp;text</p></body></html>";
        assert_eq!(html_to_text(html), "Hello & world\nUseful text");
    }

    #[test]
    fn path_globs_match_names_and_project_paths() {
        assert!(glob_matches_path(Some("*.rs"), "src/main.rs"));
        assert!(glob_matches_path(Some("src/**/*.rs"), "src/main.rs"));
        assert!(!glob_matches_path(Some("*.ts"), "src/main.rs"));
    }
    fn test_root() -> PathBuf {
        let root = env::temp_dir().join(format!(
            "nio-extra-{}-{}",
            std::process::id(),
            TERMINAL_ID.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn search_paginates_and_respects_project_boundaries() {
        let root = test_root();
        std::fs::write(
            root.join("a.rs"),
            "first\nlet Value = 1;\nlet value = 2;\nlast\n",
        )
        .unwrap();
        std::fs::write(root.join(".env"), "value=secret").unwrap();
        std::fs::write(root.join("skip.rs"), "value").unwrap();
        std::fs::write(root.join(".gitignore"), "skip.rs\n").unwrap();
        let files: Value =
            serde_json::from_str(&find_files(&root, &json!({"glob":"*.rs"})).unwrap()).unwrap();
        assert_eq!(files["files"], json!(["a.rs"]));
        let cancel = AtomicBool::new(false);
        let first: Value = serde_json::from_str(
            &search_code(
                &root,
                &json!({"query":"let value = [0-9]", "regex":true, "limit":1}),
                &cancel,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(first["matches"][0]["line"], 2);
        assert_eq!(first["next_offset"], 1);
        assert!(
            first["matches"][0]["excerpt"]
                .as_str()
                .unwrap()
                .contains("1: first")
        );
        let second: Value = serde_json::from_str(
            &search_code(&root, &json!({"query":"value", "offset":1}), &cancel).unwrap(),
        )
        .unwrap();
        assert_eq!(second["matches"][0]["line"], 3);
        assert!(search_code(&root, &json!({"query":"[", "regex":true}), &cancel).is_err());
        assert!(find_files(&root, &json!({"path":"../"})).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn http_fixture(body: String, content_type: &str) -> (String, thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let content_type = content_type.to_string();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (url, worker)
    }

    #[tokio::test]
    async fn web_fetch_extracts_html_and_paginates() {
        let (url, worker) = http_fixture("<head>hidden</head><!-- hidden --><p title='a > b'>Hello &amp; world</p><script>hidden</script>".into(), "text/html");
        let fetched: Value =
            serde_json::from_str(&web_fetch(&json!({"url":url,"max_chars":5})).await.unwrap())
                .unwrap();
        assert_eq!(fetched["text"], "Hello");
        assert_eq!(fetched["next_offset"], 5);
        worker.join().unwrap();
        assert!(web_url("file:///etc/passwd").is_err());
        assert!(web_url("https://user:secret@example.com").is_err());
        let (url, worker) = http_fixture("binary".into(), "application/octet-stream");
        assert!(
            web_fetch(&json!({"url":url}))
                .await
                .unwrap_err()
                .contains("content type")
        );
        worker.join().unwrap();
    }

    #[test]
    fn excerpts_are_bounded_after_json_encoding_and_questions_require_answers() {
        let value = text_excerpt("🤖\n\"\\".repeat(4000).chars(), 8000, 7000);
        assert!(serde_json::to_string(&value).unwrap().len() <= 7002);
        let question: Value = serde_json::from_str(
            &ask_user(&json!({"question":"Which?","options":["A","B"]})).unwrap(),
        )
        .unwrap();
        assert_eq!(question["needs_user_input"], true);
        assert!(question["question"].as_str().unwrap().contains("1. A"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminals_return_output_and_cancel_with_project_checks() {
        let root = test_root();
        let missing = terminal_read(&root, &json!({"session_id":"missing-session"}))
            .await
            .unwrap_err();
        assert!(missing.contains("terminal_start") && missing.contains(":mode"));
        let start: Value = serde_json::from_str(
            &terminal_start(
                &root,
                &json!({"command":"printf 'hello'; sleep 10"}),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap(),
        )
        .unwrap();
        let id = start["session_id"].as_str().unwrap();
        let args = json!({"session_id":id,"wait_ms":1000});
        let first: Value =
            serde_json::from_str(&terminal_read(&root, &args).await.unwrap()).unwrap();
        assert!(first["output"].as_str().unwrap().contains("hello"));
        assert!(terminal_cancel(Path::new("/different-project"), &args).is_err());
        terminal_cancel(&root, &args).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let result: Value =
                serde_json::from_str(&terminal_read(&root, &args).await.unwrap()).unwrap();
            if result["running"] == false {
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
