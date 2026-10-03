//! Exercise the real CLI against a local provider, without API keys or hosted models.
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Project {
    directory: PathBuf,
    root: PathBuf,
    config: PathBuf,
}

impl Project {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "nio-workflows-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(directory.join("project")).unwrap();
        let root = directory.join("project").canonicalize().unwrap();
        let config = directory.join("config.json");
        std::fs::write(&config, r#"{"request_interval_seconds":0}"#).unwrap();
        Self {
            directory,
            root,
            config,
        }
    }

    fn command(&self, provider: &Provider, mode: &str, auto: bool) -> Command {
        self.command_with_prompt(provider, mode, auto, "Update the project")
    }

    fn command_with_prompt(
        &self,
        provider: &Provider,
        mode: &str,
        auto: bool,
        prompt: &str,
    ) -> Command {
        let mut command = self.command_options(provider, mode, auto);
        command.args(["--", prompt]);
        command
    }

    fn command_options(&self, provider: &Provider, mode: &str, auto: bool) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nio"));
        command
            .args([
                "run",
                "-m",
                "mock",
                "--format",
                "json",
                "--mode",
                mode,
                "--trust-project",
                "--base-url",
                &provider.url,
                "-s",
                "workflow",
                "--dir",
            ])
            .arg(&self.root)
            .env("NIO_CONFIG", &self.config)
            .env_remove("NIO_API_KEY")
            .env_remove("NIO_MODEL")
            .env_remove("NIO_PROXY")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env("NO_PROXY", "*")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if auto {
            command.arg("--auto");
        }
        command
    }

    fn run(&self, provider: &Provider, mode: &str, auto: bool) -> Output {
        finish(self.command(provider, mode, auto).spawn().unwrap())
    }

    fn session_path(&self) -> PathBuf {
        let id = "workflow"
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        self.directory.join("sessions").join(format!("{id}.json"))
    }

    fn history(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.session_path()).unwrap()).unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn finish(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("CLI timed out: {}", String::from_utf8_lossy(&output.stderr));
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

enum Response {
    Events(String),
    Json(Value),
    Pause,
}

struct Provider {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Provider {
    fn new(responses: Vec<Response>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let worker = thread::spawn(move || {
            for response in responses {
                let deadline = Instant::now() + Duration::from_secs(15);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "expected another provider request"
                            );
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept: {error}"),
                    }
                };
                // Accepted sockets can inherit the listener's nonblocking mode
                // on macOS; request reads need to wait for the CLI's bytes.
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut input = Vec::new();
                let mut chunk = [0; 4096];
                let header_end = loop {
                    let count = socket.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    input.extend_from_slice(&chunk[..count]);
                    if let Some(end) = input.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&input[..header_end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                // Avoid Expect: 100-continue deadlocks if a client adds that header.
                if headers
                    .to_ascii_lowercase()
                    .contains("expect: 100-continue")
                {
                    socket.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
                }
                while input.len() < header_end + length {
                    let count = socket.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    input.extend_from_slice(&chunk[..count]);
                }
                let request: Value =
                    serde_json::from_slice(&input[header_end..header_end + length]).unwrap();
                captured.lock().unwrap().push(request);
                let (content_type, body) = match response {
                    Response::Events(events) => ("text/event-stream", events),
                    Response::Json(value) => ("application/json", value.to_string()),
                    Response::Pause => {
                        // Let the parent test interrupt a request that is still pending.
                        thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).unwrap();
            }
        });
        Self {
            url,
            requests,
            worker: Some(worker),
        }
    }

    fn finish(&mut self) {
        self.worker.take().unwrap().join().unwrap();
    }
}

fn event(delta: Value, finish: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    )
}

fn write_events(complete: bool) -> String {
    let arguments = json!({"path":"result.txt", "content":"agent change"}).to_string();
    let boundary = arguments.len() / 2;
    let mut events = event(
        json!({"tool_calls":[{"index":0,"id":"write-1","function":{"name":"write_file","arguments":&arguments[..boundary]}}]}),
        None,
    );
    events.push_str(&event(
        json!({"tool_calls":[{"index":0,"function":{"arguments":&arguments[boundary..]}}]}),
        complete.then_some("tool_calls"),
    ));
    events
}

fn done() -> Response {
    Response::Events(event(json!({"content":"Work complete."}), Some("stop")))
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        serde_json::from_str::<Value>(line).expect("stdout must remain NDJSON");
    }
}

#[test]
fn streamed_tool_calls_write_once_and_sessions_resume_with_results() {
    let project = Project::new();
    let mut provider = Provider::new(vec![Response::Events(write_events(true)), done(), done()]);
    assert_success(&project.run(&provider, "build", true));
    assert_eq!(
        std::fs::read_to_string(project.root.join("result.txt")).unwrap(),
        "agent change"
    );
    let session = project.history();
    assert!(
        session["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["tool_call_id"] == "write-1")
    );
    assert_success(&project.run(&provider, "build", true));
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    assert!(
        requests[2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["tool_call_id"] == "write-1")
    );
    let undo = std::fs::read_dir(project.directory.join("undo"))
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let journal: Value = serde_json::from_slice(&std::fs::read(undo.path()).unwrap()).unwrap();
    assert_eq!(journal["entries"].as_array().unwrap().len(), 1);
}

#[test]
fn empty_response_after_tools_retries_with_a_text_only_answer_request() {
    let project = Project::new();
    std::fs::write(project.root.join("fixture.txt"), "Useful project context").unwrap();
    let read = event(
        json!({"tool_calls":[{"index":0,"id":"read","function":{"name":"read_file","arguments":"{\"path\":\"fixture.txt\"}"}}]}),
        Some("tool_calls"),
    );
    let empty = event(json!({}), Some("stop"));
    let mut provider = Provider::new(vec![
        Response::Events(read),
        Response::Events(empty),
        done(),
    ]);
    assert_success(&project.run(&provider, "ask", true));
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    assert!(requests[1].get("tools").is_some());
    assert!(requests[2].get("tools").is_none() && requests[2].get("tool_choice").is_none());
    let messages = requests[2]["messages"].as_array().unwrap();
    assert!(
        messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Give a concise user-facing answer")
    );
    assert!(messages.iter().any(|m| {
        m["role"] == "tool"
            && m["content"]
                .as_str()
                .unwrap()
                .contains("Useful project context")
    }));
}

#[test]
fn build_switch_requires_confirmation_and_preserves_action_approval() {
    for (answer, auto, switched, written) in [
        ("yes", true, true, true),
        ("1", true, true, true),
        ("yes", false, true, false),
        ("no", true, false, false),
        ("maybe", true, false, false),
    ] {
        let project = Project::new();
        let request = event(
            json!({"tool_calls":[{"index":0,"id":"switch","function":{"name":"request_build_mode","arguments":"{}"}}]}),
            Some("tool_calls"),
        );
        let mut provider = Provider::new(vec![
            Response::Events(request),
            Response::Events(write_events(true)),
            done(),
        ]);
        assert_success(&project.run(&provider, "ask", auto));
        assert!(!project.root.join("result.txt").exists());
        assert_success(&finish(
            project
                .command_with_prompt(&provider, "ask", auto, answer)
                .spawn()
                .unwrap(),
        ));
        provider.finish();
        assert_eq!(project.root.join("result.txt").exists(), written);
        let config: Value =
            serde_json::from_slice(&std::fs::read(&project.config).unwrap()).unwrap();
        assert_eq!(config["agent_mode"] == "build", switched);
        let requests = provider.requests.lock().unwrap();
        let has_write = requests[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "write_file");
        assert_eq!(has_write, switched);
    }
}

#[test]
fn provider_delimiter_repair_preserves_mode_and_approval_checks() {
    for suffix in ["</function>", "</function"] {
        for (mode, auto, allowed) in [
            ("build", true, true),
            ("build", false, false),
            ("ask", true, false),
            ("plan", true, false),
        ] {
            let project = Project::new();
            let payload =
                write_events(true).replace("write_file", &format!("write_file\\n{suffix}"));
            let mut provider = Provider::new(vec![Response::Events(payload), done()]);
            assert_success(&project.run(&provider, mode, auto));
            provider.finish();
            assert_eq!(project.root.join("result.txt").exists(), allowed);
            let requests = provider.requests.lock().unwrap();
            let messages = requests[1]["messages"].as_array().unwrap();
            let result = messages.iter().find(|m| m["role"] == "tool").unwrap();
            assert!(!result["content"].as_str().unwrap().contains("unknown tool"));
            assert!(
            messages
                .iter()
                .any(|m| m.pointer("/tool_calls/0/function/name") == Some(&json!("write_file")))
        );
        }
    }
}

#[test]
fn oversized_streamed_tool_names_report_the_field_without_executing() {
    let project = Project::new();
    let payload = event(
        json!({"tool_calls":[{"index":0,"id":"oversized","function":{"name":"write_file".repeat(11),"arguments":json!({"path":"result.txt","content":"must not write"}).to_string()}}]}),
        Some("tool_calls"),
    );
    let mut provider = Provider::new(vec![Response::Events(payload)]);
    let output = project.run(&provider, "build", true);
    assert!(!output.status.success());
    provider.finish();
    assert!(!project.root.join("result.txt").exists());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("tool-call name exceeded the 100-byte limit")
    );
}

#[test]
fn unknown_tools_return_available_names_and_allow_recovery() {
    let project = Project::new();
    let payload = event(
        json!({"tool_calls":[{"index":0,"id":"unknown","function":{"name":"edit\n</function>","arguments":"{}"}}]}),
        Some("tool_calls"),
    );
    let mut provider = Provider::new(vec![
        Response::Events(payload),
        Response::Events(write_events(true)),
        done(),
    ]);
    assert_success(&project.run(&provider, "build", true));
    provider.finish();
    assert_eq!(
        std::fs::read_to_string(project.root.join("result.txt")).unwrap(),
        "agent change"
    );
    let requests = provider.requests.lock().unwrap();
    let result = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    let error = result["content"].as_str().unwrap();
    assert!(
        error.contains("Unknown tool")
            && error.contains("patch_file")
            && error.contains("git_diff")
    );
    assert!(!error.contains('\n'));
}

#[test]
fn incomplete_stream_never_executes_tools_and_persists_the_user_turn() {
    let project = Project::new();
    let mut provider = Provider::new(vec![Response::Events(write_events(false))]);
    let output = project.run(&provider, "build", true);
    provider.finish();
    assert!(!output.status.success());
    assert!(!project.root.join("result.txt").exists());
    assert!(String::from_utf8_lossy(&output.stdout).contains("without a complete response"));
    assert_eq!(project.history()["messages"][0]["role"], "user");
}

#[test]
fn approval_denial_and_read_only_modes_block_provider_requested_writes() {
    for (mode, auto, expected) in [
        ("build", false, "denied"),
        ("ask", true, "mode"),
        ("plan", true, "mode"),
    ] {
        let project = Project::new();
        let mut provider = Provider::new(vec![Response::Events(write_events(true)), done()]);
        assert_success(&project.run(&provider, mode, auto));
        provider.finish();
        assert!(!project.root.join("result.txt").exists());
        let requests = provider.requests.lock().unwrap();
        let result = requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool")
            .unwrap();
        assert!(
            result["content"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains(expected),
            "{result}"
        );
    }
}

#[test]
fn invalid_tool_arguments_never_execute_even_after_a_complete_stream() {
    let project = Project::new();
    let payload = event(
        json!({"tool_calls":[{"index":0,"id":"invalid","function":{"name":"write_file","arguments":"{broken"}}]}),
        Some("tool_calls"),
    );
    let mut provider = Provider::new(vec![Response::Events(payload)]);
    assert!(!project.run(&provider, "build", true).status.success());
    provider.finish();
    assert!(!project.root.join("result.txt").exists());
}

#[test]
fn context_compaction_preserves_the_summary_and_latest_request() {
    let project = Project::new();
    std::fs::create_dir_all(project.directory.join("sessions")).unwrap();
    std::fs::write(
        project.session_path(),
        json!({
            "version":1,"project_root":project.root,"project_access":true,
            "messages":[{"role":"user","content":"Keep the public API unchanged."},
                {"role":"assistant","content":"x".repeat(370_000)}]
        })
        .to_string(),
    )
    .unwrap();
    let mut provider = Provider::new(vec![
        Response::Json(
            json!({"choices":[{"message":{"content":"Preserve the public API; prior inspection is complete."}}]}),
        ),
        done(),
    ]);
    assert_success(&project.run(&provider, "build", true));
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests[0]["stream"], false);
    assert!(
        requests[0]["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("Keep the public API unchanged")
    );
    let messages = requests[1]["messages"].as_array().unwrap();
    assert!(messages.iter().any(|m| {
        m["content"]
            .as_str()
            .is_some_and(|s| s.contains("[Nio context summary]"))
    }));
    assert_eq!(messages.last().unwrap()["content"], "Update the project");
}

#[cfg(unix)]
#[test]
fn cancellation_after_a_tool_call_saves_a_resumable_session() {
    let project = Project::new();
    let mut provider = Provider::new(vec![Response::Events(write_events(true)), Response::Pause]);
    let child = project.command(&provider, "build", true).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while provider.requests.lock().unwrap().len() < 2 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let output = finish(child);
    provider.finish();
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(
        std::fs::read_to_string(project.root.join("result.txt")).unwrap(),
        "agent change"
    );
    assert!(
        project.history()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["tool_call_id"] == "write-1")
    );
    let mut resumed = Provider::new(vec![done()]);
    assert_success(&project.run(&resumed, "build", true));
    resumed.finish();
}

fn document_fixture(path: &std::path::Path, text: &str) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    zip.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    write!(zip, "<w:document xmlns:w='word'>{text}</w:document>").unwrap();
    zip.finish().unwrap();
}

#[test]
fn document_attachments_and_read_file_work_in_ask_mode() {
    let project = Project::new();
    let path = project.root.join("report.docx");
    document_fixture(
        &path,
        "<w:p><w:t>Quarterly revenue: 42</w:t></w:p><w:p><w:t>Profit: 21</w:t></w:p>",
    );
    let call = event(
        json!({"tool_calls":[{"index":0,"id":"read-report","function":{"name":"read_file","arguments":json!({"path":"report.docx","start_line":4,"line_count":1}).to_string()}}]}),
        Some("tool_calls"),
    );
    let mut provider = Provider::new(vec![Response::Events(call), done()]);
    let before = std::fs::read(&path).unwrap();
    let mut command = project.command_options(&provider, "ask", false);
    command
        .arg("--file")
        .arg(&path)
        .args(["--", "Analyze the attached report"]);
    assert_success(&finish(command.spawn().unwrap()));
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    let attached = requests[0]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(attached.contains("Quarterly revenue: 42"), "{attached}");
    let tool = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(tool.contains("Lines 4-4"), "{tool}");
    assert!(tool.contains("Profit: 21"), "{tool}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn large_document_attachment_has_explicit_continuation() {
    let project = Project::new();
    let path = project.root.join("large report.docx");
    let paragraphs = (0..2000)
        .map(|n| format!("<w:p><w:t>Report row {n}: revenue and profit</w:t></w:p>"))
        .collect::<String>();
    document_fixture(&path, &paragraphs);
    let second = project.root.join("second.docx");
    document_fixture(&second, "<w:p><w:t>Second document content</w:t></w:p>");
    let mut provider = Provider::new(vec![done()]);
    let prompt = format!("Analyze @{{{}}} @{{{}}}", path.display(), second.display());
    assert_success(&finish(
        project
            .command_with_prompt(&provider, "ask", false, &prompt)
            .spawn()
            .unwrap(),
    ));
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    let attached = requests[0]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(attached.len() <= 24 * 1024);
    assert!(attached.contains("excerpt truncated"));
    assert!(attached.contains("Next start_line:"));
    assert!(attached.contains("Second document content"));
    assert!(attached.contains("read_file"));
    assert!(!attached.contains("Report row 1999"));
}

fn tool_response(name: &str, arguments: Value) -> Response {
    Response::Events(event(
        json!({"tool_calls":[{"index":0,"id":format!("call-{name}"),"function":{"name":name,"arguments":arguments.to_string()}}]}),
        Some("tool_calls"),
    ))
}

#[test]
fn model_plugin_installs_respect_mode_approval_and_disabled_tools() {
    for (mode, auto, expected) in [
        ("ask", false, "denied"),
        ("ask", true, "denied"),
        ("plan", false, "denied"),
        ("plan", true, "denied"),
        ("build", false, "denied"),
    ] {
        let project = Project::new();
        let mut provider = Provider::new(vec![
            tool_response("install_plugin", json!({"name":"pdf","languages":"all"})),
            done(),
        ]);
        assert_success(&project.run(&provider, mode, auto));
        provider.finish();
        assert!(!project.directory.join("plugins/registry.json").exists());
        let requests = provider.requests.lock().unwrap();
        let tool = requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool")
            .unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(tool.to_lowercase().contains(expected), "{tool}");
        assert!(
            requests[0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "install_plugin")
        );
    }
    let project = Project::new();
    let mut provider = Provider::new(vec![
        tool_response("install_plugin", json!({"name":"pdf"})),
        done(),
    ]);
    let mut command = project.command_options(&provider, "build", true);
    command.args(["--no-tools", "--", "Install PDF"]);
    assert_success(&finish(command.spawn().unwrap()));
    provider.finish();
    assert!(!project.directory.join("plugins/registry.json").exists());
}

#[test]
fn ask_mode_does_not_offer_terminal_read_or_run_a_command_through_it() {
    let project = Project::new();
    let mut provider = Provider::new(vec![
        tool_response("terminal_read", json!({"session_id":"echo test"})),
        done(),
    ]);
    assert_success(&project.run(&provider, "ask", true));
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    assert!(
        !requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "terminal_read")
    );
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("unavailable in ask mode"))
    );
}

#[test]
fn missing_pdf_attachment_reaches_model_with_install_guidance() {
    let project = Project::new();
    let path = project.root.join("report.pdf");
    std::fs::write(&path, b"%PDF-1.5\nfixture").unwrap();
    let mut provider = Provider::new(vec![done()]);
    let mut command = project.command_options(&provider, "ask", false);
    command
        .arg("--file")
        .arg(&path)
        .args(["--", "Analyze this report"]);
    assert_success(&finish(command.spawn().unwrap()));
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    let attached = requests[0]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(attached.contains("File not read"));
    assert!(attached.contains("install_plugin"));
    assert!(attached.contains("Do not claim this PDF has been inspected"));
}

#[cfg(feature = "pdf-plugin")]
#[test]
fn model_can_install_pdf_then_read_the_attachment_in_build_mode() {
    use pdf_extract::{Document, Stream, dictionary};
    let mode = "build";
    let project = Project::new();
    let mut document = Document::with_version("1.5");
    let pages = document.new_object_id();
    let font = document.add_object(
        dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"},
    );
    let resources = document.add_object(dictionary! {"Font" => dictionary! {"F1" => font}});
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 50 700 Td (Plugin PDF report revenue 4200) Tj ET".to_vec(),
    ));
    let page = document.add_object(dictionary! {"Type" => "Page", "Parent" => pages, "Contents" => content, "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()]});
    document.objects.insert(
        pages,
        dictionary! {"Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1}.into(),
    );
    let catalog = document.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages});
    document.trailer.set("Root", catalog);
    let path = project.root.join("report.pdf");
    document.save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut provider = Provider::new(vec![
        tool_response("install_plugin", json!({"name":"pdf"})),
        tool_response("read_file", json!({"path":"report.pdf"})),
        done(),
    ]);
    let mut command = project.command_options(&provider, mode, true);
    command
        .arg("--file")
        .arg(&path)
        .args(["--", "Install PDF support and analyze the attached report"]);
    assert_success(&finish(command.spawn().unwrap()));
    provider.finish();
    let registry: Value = serde_json::from_slice(
        &std::fs::read(project.directory.join("plugins/registry.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(registry[0]["name"], "pdf");
    assert_eq!(registry[0]["languages"], json!([]));
    assert!(
        project
            .directory
            .join(format!(
                "plugins/pdf/nio-pdf{}",
                std::env::consts::EXE_SUFFIX
            ))
            .is_file()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let requests = provider.requests.lock().unwrap();
    assert!(requests[2]["messages"].as_array().unwrap().iter().any(|m| {
        m["role"] == "tool"
            && m["content"]
                .as_str()
                .unwrap_or_default()
                .contains("Plugin PDF report revenue 4200")
    }));
}

#[cfg(feature = "pdf-plugin")]
#[test]
fn pdf_attachment_missing_ocr_languages_reaches_model_with_the_error() {
    use pdf_extract::{Document, Stream, dictionary};
    let project = Project::new();
    let install = Command::new(env!("CARGO_BIN_EXE_nio"))
        .args(["--plugins", "install", "pdf", "--format", "json"])
        .env("NIO_CONFIG", &project.config)
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let mut document = Document::with_version("1.5");
    let pages = document.new_object_id();
    let content = document.add_object(Stream::new(dictionary! {}, vec![]));
    let page = document.add_object(dictionary! {"Type" => "Page", "Parent" => pages, "Contents" => content, "Resources" => dictionary!{}, "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()]});
    document.objects.insert(
        pages,
        dictionary! {"Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1}.into(),
    );
    let catalog = document.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages});
    document.trailer.set("Root", catalog);
    let path = project.root.join("scanned.pdf");
    document.save(&path).unwrap();
    let mut provider = Provider::new(vec![done()]);
    let mut command = project.command_options(&provider, "ask", false);
    command
        .arg("--file")
        .arg(&path)
        .args(["--", "Analyze the scan"]);
    assert_success(&finish(command.spawn().unwrap()));
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    let attached = requests[0]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(attached.contains("File not read"));
    assert!(attached.contains("no text layer"));
    assert!(attached.contains("ask which languages"));
    assert!(attached.contains("install_plugin"));
}
