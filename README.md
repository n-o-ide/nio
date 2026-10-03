# NioAI

<p align="center">
  <strong>An ultra-lightweight, blazing-fast AI coding agent for the terminal.</strong><br>
  Written in Rust · Zero runtime dependencies · Sub-10ms startup · Open Source (MIT)
</p>

<p align="center">
  <a href="#why-nioai">Why NioAI?</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#screenshots">Screenshots</a> •
  <a href="#features">Features</a> •
  <a href="#installation">Installation</a> •
  <a href="#key-commands">Key Commands</a> •
  <a href="#privacy">Privacy</a>
</p>

---

## Why NioAI?

- ⚡ **Blazing Fast Startup (<10ms)**: Built in native Rust. Starts instantly in your terminal without the startup lag or runtime tax of Python, Node.js, or Electron.
- 🪶 **Ultra-Lightweight Footprint**: Consumes under ~20MB of RAM. Keep it running in the background without draining your battery or hogging CPU.
- 🎙️ **Voice & Multimodal Audio Input**: Speak naturally to your agent with `:voice`. Records microphone audio natively and sends transcribed or raw multimodal audio to LLMs.
- 🛡️ **Atomic Undo & Local Reliability**: Every file change is backed by an atomic journal with instant rollback (`:undo`). Never lose code to an unexpected model hallucination.
- 🔌 **Universal Provider Support & Failover**: Works out of the box with any OpenAI-compatible provider (OpenRouter, Groq, Cerebras, Claude, OpenAI, Gemini, DeepSeek, or local Ollama). Automatically offers failover when a provider drops.
- 🎨 **Dual Terminal Interface**: Use the distraction-free inline CLI with non-blocking message queuing (`queue>`) or switch to the full-screen, themeable terminal TUI (`nio --tui`).
- 🔒 **Privacy-First**: Zero telemetry, zero analytics, zero external logging. Your API keys, code, and session history remain 100% on your local machine.

---

## Quick start

Launch instantly with zero installation (requires Node.js):

```sh
npx @nio-labs/nio-ai
```

Or install the pre-compiled native binary:

```sh
# macOS, Linux, Termux
curl -fsSL https://raw.githubusercontent.com/nio-labs/nio/main/install.sh | bash

# Windows (PowerShell)
irm https://raw.githubusercontent.com/nio-labs/nio/main/install.ps1 | iex
```

Start the interactive agent:

```sh
nio
```

On first launch, Nio automatically fetches available models, highlights free models, and saves your preference.

Override the model for a one-shot query:

```sh
nio run -m kilo::kilo-auto/free "Explain this project"
```

---

## Screenshots

Inline CLI showing an interactive project analysis:

![NioAI inline CLI](screenshots/nio.png)

Full-screen TUI with theme support and command palette:

![NioAI full-screen TUI](screenshots/tui.png)

---

## Installation

### 1. Pre-built native binary (Recommended)

```sh
# Linux, macOS, Termux
curl -fsSL https://raw.githubusercontent.com/nio-labs/nio/main/install.sh | bash

# Windows (PowerShell)
irm https://raw.githubusercontent.com/nio-labs/nio/main/install.ps1 | iex
```

Custom installation options:
```sh
# Pin a specific release
curl -fsSL https://raw.githubusercontent.com/nio-labs/nio/main/install.sh | NIO_VERSION=v0.3.3 bash

# Custom installation directory
curl -fsSL https://raw.githubusercontent.com/nio-labs/nio/main/install.sh | NIO_INSTALL_DIR="$HOME/.local/bin" bash
```

### 2. npm launcher (Zero-install or Global)

```sh
# Zero-install execution
npx @nio-labs/nio-ai

# Global npm installation
npm install -g @nio-labs/nio-ai
nio
```

The npm launcher caches verified binaries by package version and platform, verifies SHA-256 checksums before extraction, and checks binary authenticity before execution.

### 3. Build from source (Rust)

```sh
cargo install --locked --git https://github.com/nio-labs/nio
# Or clone and build locally:
cargo build --release
```

---

## Features

<table>
  <tr>
    <td align="left" valign="top"><strong>Ask, Plan, or Build mode</strong><br><br>Choose how much autonomy Nio uses, from read-only architectural guidance to fully approved code changes.</td>
    <td align="left" valign="top"><strong>Universal Model Discovery</strong><br><br>Connect to any OpenAI-compatible provider with automatic model cataloging and free tier filtering.</td>
    <td align="left" valign="top"><strong>Deep Project Tools</strong><br><br>Search code with regex, find files by glob, read documents, inspect git diffs, and run approved shell commands.</td>
  </tr>
  <tr>
    <td align="left" valign="top"><strong>Non-blocking queue</strong><br><br>Queue follow-up prompts while Nio is generating answers or editing files.</td>
    <td align="left" valign="top"><strong>Voice & Multimodal Audio</strong><br><br>Speak directly into your microphone with <code>:voice</code> for instant audio transcription and multimodal querying.</td>
    <td align="left" valign="top"><strong>Atomic Undo Journaling</strong><br><br>Revert file edits made by the agent across current or previous runs with instant atomic rollbacks.</td>
  </tr>
</table>

## Queue messages while working

In the inline CLI, a persistent `queue>` input stays available during a response. The agent continues working while you type. Enter queues the message; unfinished drafts stay available when the response completes. Messages run in order after the current task finishes. Queued messages do not enter model context until they run.

- `F2` or `:queue` — open the queue panel; arrows select, Enter/e edits, Delete/d removes, and p pauses/resumes
- `:queue edit 2 New message` — replace a pending message
- `:queue remove 2` / `:queue clear` — remove pending work
- `:queue pause` / `:queue resume` — control automatic processing
- `:stop` — stop the response and preserve pending messages

Queue controls work while a response is running. Other commands entered in the inline queue prompt run after the response; the TUI also supports skill commands during a response. Errors and interruptions pause the queue. Pending messages are kept in memory for the running session.

## GitHub skills

Install a skill folder containing `SKILL.md` from a GitHub repository. Git is required. Local folders are not accepted as installation sources.

```sh
nio --skills
nio --skills add https://github.com/your-org/your-repo path/to/skill
nio skills add https://github.com/your-org/your-repo/tree/main/path/to/skill
nio --skills disable skill-name
nio --skills enable skill-name
nio --skills remove skill-name
```

`nio --skills` defaults to listing installed skills; `nio skills` remains available as an alias. The same operations are available through `:skills` or `/skills` in interactive mode. Installed packages and their enable/disable settings are stored beside the Nio configuration. New installations are enabled by default. The model receives a catalog of enabled skills and can read relevant instructions and supporting text files through `read_skill_file`. Changes affect subsequent requests; skill instructions retain the current mode and approval restrictions.

## Optional plugins

Plugins add executable file readers without bundling their dependencies into nio. PDF is the first available plugin; other readers can use the same package interface.

```sh
nio --plugins                         # installed and available plugins
nio plugins install pdf               # PDF text extraction, no OCR models
nio --plugins install pdf --languages eng,khm
nio --plugins install pdf --languages all
nio --plugins languages pdf            # language codes and download sizes
nio --plugins disable pdf
nio --plugins enable pdf
nio --plugins remove pdf
nio --plugins list --format json
```

`nio --plugins` opens a provider-style selection menu in an interactive terminal; `nio plugins` is an alias. Choose the PDF plugin to install, enable, disable, or remove it. For OCR, mark the language packs you need and select **Install PDF plugin with selected languages** (or **Install selected OCR languages** if PDF is already installed). **Install all languages** remains an explicit option. The TUI language list also supports text search. `nio --plugins list` prints a plain list; JSON and noninteractive invocations remain scriptable. `:plugins` and `/plugins` also work in the interactive prompt and TUI. Packages, enabled settings, and OCR models are stored beside the Nio configuration. Removing a plugin also removes its downloaded models. Installations are serialized and staged before registration. Reinstalling `pdf` adds missing languages and enables the plugin; it does not remove existing models.

The PDF plugin is a separate `nio-pdf` executable. The installer uses a worker alongside nio for local builds, or downloads the worker for the current platform and nio version from the official release and verifies its SHA-256 checksum. Released base nio archives do not contain the PDF worker. Plugin download installation requires a release publishing the matching worker asset; until then, build both binaries locally:

```sh
cargo build --release --locked --bin nio
cargo build --release --locked --features pdf-plugin --bin nio-pdf
./target/release/nio --plugins install pdf
```

PDF text extraction needs no other program. OCR is optional: install language models and provide `tesseract` and Poppler's `pdftoppm` on PATH. For example, macOS uses `brew install tesseract poppler`, and Debian/Ubuntu uses `apt install tesseract-ocr poppler-utils`; Windows users need compatible Tesseract and Poppler installations on PATH. Nio does not automatically run a system package manager. Models may run an approved setup command in Build mode when the user requests it.

Language packs come from a pinned official `tessdata_fast` revision and are verified against its Git blob checksums. Selected languages download only their models. `all` installs 126 models (about 339 MiB), including orientation/math models; it does not use all of them simultaneously for recognition. `read_file` accepts `ocr_languages`, for example `["eng", "khm"]`, to choose up to eight installed recognition languages. Without a selection, OCR uses English if installed, otherwise the first installed recognition language. `osd` is orientation data and cannot be selected as a recognition language.

The PDF reader extracts each page's existing text layer and uses OCR for pages with no extractable text. It keeps page labels and marks OCR results. Inputs are capped at 20 MiB, extracted text at 512 KiB, PDFs at 1,000 pages, and OCR at 50 pages per document. Each render/OCR process has a 60-second timeout; plugin reads have a 300-second total timeout. Split larger scans into smaller documents. Embedded images on a page that already has text are not separately OCR'd, and OCR can misread text.

Models can inspect `list_plugins` in every mode. `install_plugin` is available in Ask, Plan, and Build so file analysis can install a needed reader or OCR language packs without a mode switch. Ask and Plan always require explicit installation approval, even if automatic approval is enabled; noninteractive Ask and Plan runs deny installation. `manage_plugin` (enable/disable/remove) still requires Build mode and approval. Installation approval names the plugin, OCR selection, and estimated model download size. When a PDF is attached in an interactive session and the reader is missing, nio asks for installation approval before sending the document to the model. A filename containing “scan” suggests English OCR for this prompt. If installation is declined or OCR support is missing, the model sees the read error and can explain the next step. Reading never installs plugins or downloads languages implicitly. `--no-tools` disables model plugin management too.

The current plugin catalog contains the PDF reader. The plugin system can support other readers in the future. Installed plugin code runs locally with host access, and plugin output is bounded and treated as untrusted data.

## Full-screen interface

```sh
nio --tui
nio --tui --session SESSION_ID
```

The optional TUI uses a theme background, a scrollable conversation, and a persistent input area. Enter sends a message when idle and queues it while working. Shift+Enter or Alt+Enter inserts a newline when supported by the terminal. Long pastes use compact markers. Use the mouse wheel/trackpad or Page Up/Down to scroll, and click the input to position the cursor.

`:` and `/` open the command palette. `:setting`, `:mode`, `:model`, `:reasoning`, and `:theme` open selection panels. `:sessions` opens recent saved conversations; `:details` opens the latest diff. Approval prompts use Y/N and D for details. Ctrl+C stops the current work and pauses pending messages. `:quit` saves the session and restores the terminal.

In the model picker, press Ctrl+P to filter by provider, or choose All providers to reset the filter. Text search continues to work within the selected provider.

The model picker filters by model name, selector, or provider as you type; Backspace edits the search and Ctrl+U clears it. Arrow navigation updates only changed screen rows.

The theme panel previews each highlighted theme immediately. Enter saves it; Esc restores the saved theme. Available palettes include Tokyo Night and the muted light options Light, Paper, and Cloud.

TUI settings also accept explicit values, such as `:mode build`, `:theme ocean` or `:theme light`, and `:proxy off`. `:provider` displays saved providers; configure provider credentials with `nio provider` in the inline CLI.

## Key commands

Nio provides an interactive command palette in both the inline CLI and full-screen TUI (type `:` or `/`):

| Command | Action |
|---|---|
| `:voice` | Record microphone audio and send transcribed/multimodal audio to the model |
| `:mode` | Switch autonomy mode (`ask`, `plan`, `build`) |
| `:models` | Browse, search, and switch models and providers |
| `:undo` | Revert the last agent file change with atomic rollback |
| `:diff` | Review pending git modifications and changes |
| `:snippets` | Manage reusable code snippets and prompt templates |
| `:ide` | Manage background NioDE IDE language services daemon |
| `:plugins` | Install and configure optional file readers (PDF, SQLite, DuckDB, etc.) |
| `:skills` | Browse, add, and manage GitHub-based agent skills |
| `:queue` | Inspect and edit queued background messages |
| `:settings` | Configure theme, reasoning effort, auto-approval, and mouse |
| `:clear` | Clear the current conversation context |
| `:quit` | Save session and exit |

Preset providers include OpenRouter, Groq, Cerebras, Gemini, DeepSeek, Together AI, Fireworks, Mistral, SiliconFlow, Anthropic Claude, and OpenAI Codex.

File tools stay inside the current directory. Auto-discovery skips generated folders, secret filenames, and `.gitignore` paths. Text reads and writes are capped at 512 KiB; supported document inputs may be up to 20 MiB, with at most 512 KiB of extracted text. Search reads at most 16 MiB and returns at most 50 entries. `.gitignore` parsing is bounded to 256 KiB.

Undo history is stored privately beside the configuration, in `undo/`, and is shared by sessions for the same canonical project folder. It keeps up to 32 file edits within an 8 MiB journal budget, dropping the oldest entries when needed. Undo checks that the file still matches Nio's edit and refuses to overwrite later changes; failed undo attempts keep their recovery entry. Shell-command changes are not covered by this history.

## Agent tools

Nio offers bounded tools with small results:

- `list_plugins`, `install_plugin`, `manage_plugin`: inspect optional file readers, install plugins/add OCR languages, and enable/disable/remove plugins. Models can install readers and OCR languages in every mode with approval; enable/disable/remove requires Build.
- `find_files`: discover project files with path/glob filters and pagination. `path` defaults to `.` and stays within the active project; use `nio --dir /path/to/project` to work in another folder, or `:path` to inspect the current folder.
- `search_code`: literal or regex search with numbered lines, short context, and pagination.
- `web_fetch`: read an HTTP(S) page as text using the configured proxy, with a 20-second request timeout. HTML scripts/styles are removed; JavaScript execution, browser clicks, and forms are unsupported. Responses are capped at 1 MiB, excerpts at 8,000 characters, with `next_offset` for more.
- `ask_user`: ask one clarification question with up to three choices. In the regular prompt and full-screen TUI, select an answer with the arrow keys or type your own; nio continues the same turn. Noninteractive runs show the question for your next reply.
- `terminal_start`, `terminal_read`, `terminal_cancel`: start an approved command, read incremental output, and stop it. Starting/stopping commands requires Build mode; sessions live within one Nio process and stop when it exits. At most four commands run concurrently, with a one-hour maximum timeout and a 64 KiB output tail.

Ask and Plan allow research and project reads, while Build allows approved edits and commands. `--no-project-tools` allows web research, questions, and skill reading without project file or shell access. `--no-tools` disables every agent tool. `web_fetch` sends requests to the supplied website URL.

When implementation requires Build mode, Nio can offer a Yes/No switch with `request_build_mode`. Reply `yes` or `1` to switch to Build and continue the task, or `no` to keep the current mode. The switch saves Build as your default; file and command approval settings still apply.

## Sessions and output

Terminal responses render headings, bold (`**text**`), italics (`*text*`), lists, inline/fenced code, and streamed Markdown tables. JSON output preserves the original Markdown for host applications.

Persist and resume conversations with `-s`:

```sh
nio run -s my-chat -m kilo::kilo-auto/free "Check my project"
nio run -s my-chat -m kilo::kilo-auto/free "Now explain the config files"
```

Emit machine-readable events with `--format json`:

```sh
nio run --format json ...
```

See [STREAM.md](STREAM.md) for event shapes and [PROTOCOL.md](PROTOCOL.md) for CLI options, trust behavior, and host integration.

## Settings and config

Common settings:

```sh
nio config list
nio config get model
nio config set theme Monokai
```

Toggle automatic approval for a single run with `--auto`. Set reasoning effort with `:reasoning` or `--reasoning`. Toggle mouse input with `:mouse`. Route traffic through a proxy with `:proxy` or `NIO_PROXY`.

## Privacy

NioAI includes no telemetry, analytics, tracking, or background reporting. Network requests only go to features you use: model discovery, provider checks, responses, requested web pages, and requested skill/plugin/language downloads. Prompts, conversation context, project files, tool results, and approved command output can be sent to the model endpoint. Review your provider's terms before sending sensitive information. Credentials and sessions are stored locally; on Unix they are restricted to your user account. Avoid putting API keys directly in shell history.

## NioDE integration

NioAI is available as the `nio` agent through NioDE's direct subprocess route. Install a current native `nio` executable on the server's `PATH`; Nio does not require npm or a persistent agent server. Automatic installation awaits published, verified native release artifacts.

The Chat UI requests project access before enabling Nio tools. Build consent also grants file edits and shell execution for that conversation/project. Ask and Plan allow reads and approved plugin/language installation; project editing and shell execution require Build. Installed third-party plugin executables remain trusted host code. Studio and Canvas use `--mode ask --no-project-tools` with Nio 0.3.0 or later; it receives generated text and owns its file writes.

For another host, the explicit interface is:

```sh
nio models --format json
nio run -m kilo::kilo-auto/free --format json --mode ask --reasoning low \
  --trust-project --dir /path/to/project -s project-chat -- "Explain this project"
```

`--trust-project` grants project reads for this invocation. `--no-tools` disables all project discovery and tools, even for remembered trusted folders. `--auto` grants writes and shell commands for a single invocation; noninteractive runs ignore saved automatic approval preferences. Approved commands have the current user's host access; the project directory is their starting directory, not a shell sandbox. `NIO_API_KEY` overrides credentials for the selected chat provider and is not broadcast to model catalogs. Catalogs use provider-specific saved or environment credentials. `--reasoning` accepts low, medium, high, or default. `--file PATH` attaches a supported document, UTF-8/UTF-16 text file, or a PNG, JPEG, GIF, or WebP image. In prompts, use `@path` or `@{path with spaces}` to attach an existing file; in the regular interactive prompt and `--tui`, an existing absolute path pasted or dropped into the prompt is also attached automatically. `read_file` can read a specific absolute local path when you ask about it, including image files; project-relative reads remain project-scoped. Dropping a supported file into the TUI composer inserts a path reference. Attachment excerpts share a 24 KiB prompt limit; longer files include a truncation notice and can be continued through `read_file`; image files may be up to 10 MiB each and 20 MiB total. Images require a vision-capable provider model. Built-in document reading supports Word (`.docx`, `.docm`), Excel (`.xlsx`, `.xls`, `.xlsb`, `.xlsm`, `.xlam`), PowerPoint (`.pptx`, `.pptm`), and OpenDocument (`.odt`, `.ods`, `.odp`). Markdown, plain text, JSON/JSONL, CSV/TSV, YAML, TOML, XML, HTML, logs, and source code work as text. UTF-16 text requires a byte-order mark. Documents are extracted locally into text; formatting, embedded images, charts, and macros are not analyzed or executed. Spreadsheet output includes sheet names, row numbers, and tab-separated cell values; formulas are not recalculated. Scanned PDFs need OCR or page images; password-protected documents and legacy Word `.doc`/PowerPoint `.ppt` need conversion to an unlocked supported format. Document inputs are capped at 20 MiB, extracted text at 512 KiB, and ZIP-based documents at 4,096 entries and 32 MiB of declared expanded content. Parser working memory can exceed these file limits. PDF reading and optional OCR are provided by the separately installed `pdf` plugin. Document editing is not included.

JSON runs emit a `session` event with `sessionID` immediately, and persist the conversation on completion or handled interruption. Sessions are bound to the canonical project directory and project-access scope, with a lock preventing simultaneous use. Old array-only sessions have no project binding and require starting a new session; their files are preserved.

## CI/CD and automation

NioAI can run headlessly in CI/CD pipelines (GitHub Actions, GitLab CI, scripts) for automated code reviews, PR summaries, and task execution.

### Headless execution

Run prompts non-interactively using `nio run` with `--auto` and `--trust-project`:

```sh
# Run a one-shot query or task in headless mode
nio run -m kilo::kilo-auto/free --trust-project --auto "Review recent git diff and summarize changes"

# Stream structured JSON events for CI consumers
nio run --format json --trust-project --auto "Run checks and suggest fixes"
```

### Plugin handling in automated pipelines

Because plugin installation grants execution trust to host binaries, interactive Nio runs require explicit user confirmation. In automated, headless environments:

1. **Pre-install plugins (Recommended):** Install required plugins in your CI build steps before invoking the agent. This ensures deterministic builds and avoids network downloads during execution:
   ```sh
   nio --plugins install sqlite
   nio --plugins install pdf
   ```
2. **Unattended execution:** If an agent encounters a file requiring an uninstalled plugin during a headless run, the read tool returns a missing plugin error instead of blocking or hanging stdin on an approval prompt. The agent will gracefully continue with other files and tasks without crashing.

## Resource and reliability limits

- File tools reject excluded directories, parent traversal, and symlinks. Discovery visits at most 10,000 entries, to depth 8. Text reads/writes remain capped at 512 KiB (document inputs up to 20 MiB, extracted text up to 512 KiB); search reads at most 16 MiB and returns at most 50 bounded snippets. Discovery reads at most 256 KiB of `.gitignore` rules and applies common glob, directory, anchoring, and negation patterns.
- HTTP connections have a 10-second deadline; reads have a 60-second inactivity deadline; requests have a 300-second total deadline. Catalog requests have a 15-second deadline. Transient connection failures and HTTP 429/502/503/504 receive at most three retries with backoff and jitter before response delivery.
- A turn permits 128 model steps and at most 16 tool calls per response. When the step budget is reached, Nio asks the provider to summarize progress and tells you how to continue. Response text is capped at 2 MiB, individual stream events and tool arguments at 1 MiB, and some internal reads allow up to 8 MiB. Incomplete responses cannot execute tools.
- Context uses a 512 KiB serialized-message budget and drops whole older user turns, preserving tool-call/result groups. This is a byte budget, not an exact tokenizer or a guarantee for every provider's context window. A turn stops with a clear error when it reaches the budget.
- Files, config, and sessions use synced temporary files and atomic replacement. Writes preview changed lines and reject stale content. Config and session files are private on Unix. Locks prevent simultaneous Nio saves; external editors do not participate in these locks.
- Shell commands use a 120-second deadline and bounded captured output. On Unix, shell process groups are killed on completion, timeout, or handled cancellation, and the shell is reaped. Hosts should send SIGTERM and allow cleanup before forcing termination. Native Windows shell support remains pending.
- Follow-up suggestions are off by default to avoid an extra model request.

## Development checks

```sh
cargo fmt --check
cargo test --locked
npm test
```

The Rust integration tests run the real CLI against a local mock provider, covering streamed tools, approval and mode restrictions, session resume, interruption, and context compaction. Tests need permission to bind localhost ports. Launcher and installer tests use local fixtures without downloading releases. Running the JavaScript tests requires Node.js 20 or later; the npm launcher itself supports Node.js 16 or later.

## Repository

- Language: Rust
- Default provider: Kilo
- Repository: https://github.com/nio-labs/nio
- License: [MIT](LICENSE)
