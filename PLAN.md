# NioAI Plan

**Product:** NioAI
**Command:** `nio`
**Purpose:** A lightweight, standalone AI coding agent for terminals, including small Linux systems and Android/Termux. NioDE is one optional client of NioAI, not a runtime requirement.

## Product principles

- Keep the default installation and runtime small.
- Use hosted model APIs through configurable provider routes; do not promise that any model or provider will remain free.
- Keep the core agent independent of NioDE and individual providers.
- Ask before applying file changes or running commands that need approval.
- Document the CLI and streaming interfaces so other tools can integrate with NioAI.

## Version 0.1: Standalone CLI foundation

- **Status:** In progress. The CLI is functional: provider/model selection, project trust, streaming chat, tool calls, file/shell approvals, sessions, JSON output, proxy support, config commands, themes, reasoning effort, and basic error handling are implemented.
- Provide `nio run`, `nio models`, `nio --help`, and `nio --version`.
- On first interactive launch, require a model choice, save it as the default, then prompt for the user's first request. Allow `--model`/`NIO_MODEL` to select a model non-interactively.
- Use OpenAI-compatible streaming chat completions with function/tool calls.
- Use anonymous calls only when the provider permits them.
- Provide a small line-based interactive UI with progress updates and a persistent in-process conversation; avoid a full-screen TUI dependency in the lightweight core.
- Let the agent list, search, and read project files automatically. Ask before writes and shell commands unless auto-approval is explicitly selected.
- Emit NDJSON events compatible with NioDE's existing reasoning, text, and tool-use stream parser.
- Build a foundation that can later support tools, modes, sessions, and additional routes.

**Milestone:** From a project directory, a user can send a prompt to a configured model and see the streamed response in the terminal.

The current implementation provides `nio`, `nio run`, `nio models`, `nio --help`, and `nio --version`. Running `nio` starts a lightweight line UI. The agent automatically gets project overview, list/search/read tools; writes and shell commands require approval by default. OpenAI-compatible model responses stream text and tool calls. `nio run --format json` emits NDJSON events (`reasoning`, `text`, and `tool_use`) that match NioDE's current chat parser. The NioDE UI now lists NioAI through a native subprocess adapter. Model listing includes available entries with free models first, labels each model with its gateway, and supplies route-qualified selectors such as `kilo::provider/model`. First launch asks for a model and saves it in Nio's config. Kilo's public free routes support keyless use; OpenRouter is listed when its key is configured. Nio currently does not import credentials from OpenCode or provide account sign-in.

Interactive users can run `:provider`, `:models`, `:mode`, `:reasoning`, `:theme`, `:approval`, `:proxy`, `:bash` or `:command`, `:path` / `:workingpath`, `:diff`, `:undo`, and `:clear`. Headless and scripted users can use `NIO_BASE_URL`, `NIO_API_KEY`, `NIO_PROXY`, `NIO_CONFIG`, `--format json`, `--session`, `--trust-project`, `--no-tools`, `--mode`, `--reasoning`, `--file`, and `--auto`. `nio config` supports `list`, `get <KEY>`, and `set <KEY> <VALUE>`.

## Version 0.2: Robustness and shell safety

- Add lightweight retry with jitter for transient provider/network errors, especially rate limits and timeouts.
- Add token-aware context management so long sessions do not exceed model windows silently.
- Replace `Result<T, String>` with small structured error types to distinguish auth failures, timeouts, tool errors, and provider issues.
- Add a configurable HTTP request timeout in addition to the existing shell command timeout.
- Add lightweight shell-safety checks for obviously destructive commands, with an explicit override path.
- Add diff-style previews for file writes and show concise approval context.

## Version 0.3: Repository-aware editing

- Respect `.gitignore` and common generated/secret paths during automatic project discovery.
- Add a lightweight patch/line-replace tool so the agent can make small edits without rewriting whole files.
- Add optional Git context: `git status` and `git diff` summaries when the project is a repository.
- Improve large-repository discovery with depth limits and skip rules.

## Version 0.4: Modes and sessions

- Enforce read-only boundaries in Ask/Plan modes.
- Improve session resume UX and conversation management.
- Improve interruption and cancellation behavior with predictable exit codes.

## Version 0.5: Provider routes

- Maintain existing provider presets and add validation on provider save.
- Add provider adapters for request/error handling and clearer failure messages.
- Reserve fallback routing for later once single-provider flow is stable.

## Version 0.6: Integration interfaces

- Version and publish the NDJSON stream contract in `STREAM.md`.
- Add cancellation and NioDE session integration.
- Keep NioDE-specific behavior outside the core CLI.

## Version 1.0: Public release

- Document installation, configuration, security behavior, and supported platforms.
- Provide reproducible release builds for desktop Linux, macOS, and Android/Termux where practical.
- Publish contribution guidance, license, release notes, and a support policy.
- Stabilize core command behavior and the non-interactive output contract.

## Roadmap after 1.0

These are candidate directions, prioritized from user feedback; they are not release commitments.

1. **1.1 — Reliability:** session recovery, clearer tool output, interruption handling, and configuration diagnostics.
2. **1.2 — Repository awareness:** improved context selection, Git status/diff summaries, and large-repository handling.
3. **1.3 — Provider flexibility:** provider presets, model discovery, per-task model selection, and configurable fallback.
4. **1.4 — Integrations:** mature NioDE support and optional agent protocols such as ACP where useful.
5. **1.5 — Optional tools:** opt-in MCP, language-server features, and user-defined tool packs.
6. **2.0 — Longer workflows:** optional subagents and multi-step task workflows, while preserving a small default install.

## Implementation choice

The initial implementation uses Rust because it is available in the workspace, NioDE's server is Rust, and Rust can produce a standalone binary suitable for constrained devices. Keep dependencies focused and revisit the choice if Termux packaging or binary size becomes a problem.

## Decisions still needed before public release

- Confirm the package/repository namespace and check name availability.
- Set supported minimum OS versions and release targets.
- Version the NDJSON event schema before external integrations depend on it.

## Current enhancement milestone: native NioDE integration

Implemented in the working tree:

- Shared project containment/exclusion checks, symlink rejection, and bounded traversal.
- HTTP deadlines and bounded retries before response delivery.
- Complete-turn context trimming and explicit context/resource limits.
- Stream completion validation before executing tools; step/call/response limits.
- Atomic, private persistence, session locks and project/access binding, write previews and stale-write checks.
- Unix shell group cleanup and handled noninteractive cancellation.
- Per-run mode, reasoning, trust, text attachments, JSON model discovery and session metadata.
- NoTerm native subprocess adapter, Chat consent and mode mapping, tools-disabled Studio, and ordered output completion.
- Interactive configuration commands and persisted settings for model, approval, reasoning, theme, progress style, proxy, and provider credentials.
- Shell-safety warnings for obviously destructive commands, with an explicit override path.

Validation scope: compilation and frontend type checking. Live provider,
interactive terminal, cancellation, and cross-platform behavior still need
runtime verification. No new tests are included in this milestone.

Remaining release work: provider adapters, tokenizer/model-window-aware
context budgeting, native Windows commands, checksummed release artifacts,
automatic native installation, and published platform validation.
