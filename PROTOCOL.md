# NioAI CLI protocol

This document describes the command line interface for users and host applications. The JSON chat event shapes are documented in [STREAM.md](STREAM.md).

## Commands

```text
nio [OPTIONS]                         Start the interactive client
nio run [OPTIONS] <prompt>            Run one turn
nio models [--format json]             List available model selectors
nio provider                           Configure a provider interactively
nio --help
nio --version
```

Options may be placed before or after the prompt. Use `--` before a prompt that begins with `-`.

| Option | Meaning |
| --- | --- |
| `-m`, `--model SELECTOR` | Select a model; defaults to `NIO_MODEL` or the saved model. Use a selector from `nio models`, usually `gateway::model-id`. |
| `-s`, `--session ID` | Resume or create a persistent conversation. |
| `--base-url URL` | OpenAI-compatible API base URL for an unqualified model selector. Overrides `NIO_BASE_URL`. |
| `--api-key KEY` | API key for the selected endpoint. Prefer environment variables or provider settings to avoid shell history. |
| `--dir PATH` | Project working directory. |
| `--format json` | Emit newline-delimited JSON chat events on stdout. `text` and `human` select normal output. |
| `--trust-project` | Grant project access for this invocation without the interactive trust prompt. |
| `--no-tools` | Disable project discovery and all agent tools. |
| `--mode ask\|plan\|build` | Select the turn mode. Ask and Plan do not expose write or command tools. |
| `--reasoning low\|medium\|high\|default` | Set provider reasoning effort when supported. |
| `--file PATH` | Attach UTF-8 text to the prompt. Combined prompt and attachment size is limited to 24 KiB. |
| `--auto` | Approve agent file writes and shell commands for this invocation. |

When stdin is not a terminal, the prompt must be provided as an argument. An untrusted project gets no project access in this case unless `--trust-project` is set. The working directory itself is not a shell sandbox: approved commands run with the current user's host permissions.

## Model discovery

`nio models` prints a human-readable catalog. `nio models --format json` prints one JSON array:

```json
[
  {"id":"kilo::kilo-auto/free","label":"Kilo Auto Free · Kilo (free)"}
]
```

The `id` is the stable value to pass to `--model`. Providers can change model availability and labels over time. Catalog requests use credentials configured for the corresponding provider.

## Chat stream

With `nio run --format json`, stdout is NDJSON: one JSON object per line, flushed as events arrive. Progress events use the `reasoning` type as short status text; they are not private model reasoning. Text deltas, tool lifecycle events, session metadata, cancellation, and completion are described in [STREAM.md](STREAM.md).

Errors and human-facing diagnostics go to stderr. A successful completed turn exits with status 0. A request or provider failure, incomplete response, or handled cancellation exits nonzero. A partial text stream is not a completed answer. Hosts should wait for process exit and forward all remaining output before reporting completion.

## Project access and approvals

Interactive use asks before trusting a new project folder. Trust allows project reads; it does not approve writes or commands. Headless runs do not prompt for project trust. Hosts that have collected consent can use `--trust-project`, and should use `--no-tools` when project access is not part of the interaction.

Ask and Plan only expose list, search, and read tools. Build can also request file writes and shell commands. Writes and commands require approval unless `--auto` is supplied. Saved interactive approval settings do not enable automatic approval for headless runs.

Project reads, attachments, tool calls, and tool results may be included in requests to the selected model provider. See the privacy note in [README.md](README.md#privacy).

## Environment and local state

- `NIO_MODEL`: default model selector.
- `NIO_BASE_URL`: default OpenAI-compatible endpoint for an unqualified model selector.
- `NIO_API_KEY`: key for that endpoint or selected chat provider.
- Provider-specific key variables: for example, `OPENROUTER_API_KEY`, `KILO_API_KEY`, `ANTHROPIC_API_KEY`, and `OPENAI_API_KEY`.
- `NIO_CONFIG`: override the user configuration file path.
- `NIO_PROXY`: override the saved HTTP(S) proxy for model API requests.

Without `NIO_CONFIG`, configuration is stored under `$XDG_CONFIG_HOME/nio/config.json` or `$HOME/.config/nio/config.json`. Sessions are stored beside that configuration. On Unix, config and session files are created with user-only permissions. Session IDs are bound to the canonical project directory and project-access scope; simultaneous use of a session is rejected.

## Resource limits

Current safeguards include bounded project traversal, 512 KiB per file read/write, a 16 MiB search read budget, a 96 KiB serialized context budget, bounded provider responses, request deadlines, and capped retries. Limits and details are maintained in the [README resource section](README.md#resource-and-reliability-limits).
