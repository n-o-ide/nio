# NioAI Plan

**Product:** NioAI  
**Command:** `nio`  
**Purpose:** A lightweight, standalone AI coding agent for terminals, including small Linux systems and Android/Termux. NoIDE is one optional client of NioAI, not a runtime requirement.

## Product principles

- Keep the default installation and runtime small.
- Use hosted model APIs through configurable provider routes; do not promise that any model or provider will remain free.
- Keep the core agent independent of NoIDE and individual providers.
- Ask before applying file changes or running commands that need approval.
- Document the CLI and streaming interfaces so other tools can integrate with NioAI.

## Version 0.1: Standalone CLI foundation

- **Status:** Initial implementation working; provider behavior still needs validation against real endpoints and free model tool-call support.
- Provide `nio run`, `nio models`, and `nio --help`.
- Let users select a model entry labeled with its gateway, then route by the model selector; use Kilo as the default catalog and add other catalogs when configured.
- On the first interactive launch, require a model choice, save it as the default, then prompt for the user's first request. Allow `--model`/`NIO_MODEL` to select a model non-interactively.
- Use OpenAI-compatible streaming chat completions with function/tool calls.
- Allow anonymous calls only for documented free models on Kilo's public gateway.
- Provide a small line-based interactive UI with progress updates and a persistent in-process conversation; avoid a full-screen TUI dependency in the lightweight core.
- Let the agent list, search, and read project files automatically. Ask before writes and shell commands unless auto-approval is explicitly selected.
- Emit NDJSON events compatible with NoIDE's existing reasoning, text, and tool-use stream parser.
- Build a foundation that can later support tools, modes, sessions, and additional routes.

**Milestone:** From a project directory, a user can send a prompt to a configured model and see the streamed response in the terminal.

The current implementation provides `nio`, `nio run`, `nio models`, `nio --help`, and `nio --version`. Running `nio` starts a lightweight line UI. The agent automatically gets project overview, list/search/read tools; writes and shell commands require approval by default. OpenAI-compatible model responses stream text and tool calls. `nio run --format json` emits NDJSON events (`reasoning`, `text`, and `tool_use`) that match NoIDE's current chat parser. The NoIDE UI does not yet list Nio as a selectable agent. Model listing shows free entries by default (`--all` includes paid entries), labels each model with its gateway, and supplies route-qualified selectors such as `kilo::provider/model`. First launch asks for a model and saves it in Nio's config. Kilo's public free routes support keyless use; OpenRouter is listed when its key is configured. Nio currently does not import credentials from OpenCode or provide account sign-in.

## Version 0.2: Repository tools

- Respect `.gitignore` and repository guidance files throughout traversal.
- Replace whole-file writes with patch proposals and show the diff before applying changes.
- Add output limits, timeouts, cancellation, and clearer permissions for shell execution.
- Improve large-repository discovery and context selection.

## Version 0.3: Modes and sessions

- Add plan/read-only and build modes with clear permission boundaries.
- Save conversations locally and resume them.
- Add cancellation and predictable exit/error behavior.

## Version 0.4: Provider routes

- Add presets for OpenRouter, Kilo, and other compatible endpoints.
- Support model discovery where a route provides it.
- Keep provider-specific request and error handling behind adapters.
- Add fallback routing only after the single-provider flow is reliable.

## Version 0.5: Integration interfaces

- Version and publish the NDJSON stream contract; add cancellation and NoIDE session integration.
- Add an optional NoIDE adapter for prompts, model selection, attachments, streaming, and cancellation.
- Keep NoIDE-specific behavior outside the core CLI.

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
4. **1.4 — Integrations:** mature NoIDE support and optional agent protocols such as ACP where useful.
5. **1.5 — Optional tools:** opt-in MCP, language-server features, and user-defined tool packs.
6. **2.0 — Longer workflows:** optional subagents and multi-step task workflows, while preserving a small default install.

## Implementation choice

The initial implementation uses Rust because it is available in the workspace, NoIDE's server is Rust, and Rust can produce a standalone binary suitable for constrained devices. Keep dependencies focused and revisit the choice if Termux packaging or binary size becomes a problem.

## Decisions still needed before public release

- Confirm the package/repository namespace and check name availability.
- Set supported minimum OS versions and release targets.
- Version the NDJSON event schema before external integrations depend on it.
