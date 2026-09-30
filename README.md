# NioAI

A lightweight AI coding agent for the terminal. The product is called **NioAI**; the executable is **`nio`**.

NioAI is being built as a standalone public CLI. It sends requests to configurable OpenAI-compatible model endpoints. Provider access, model availability, and free quotas are controlled by the provider and may change.

## Requirements

- Rust toolchain to build from source
- Any OpenAI-compatible provider you configure; Kilo Gateway is available by default

## Build

```sh
cargo build --release
```

The executable is at `target/release/nio`.

## Configure and run

Running `nio` starts a lightweight, line-based coding agent. Before accessing a project folder, Nio asks whether to trust it. Trusted folders are remembered; choosing no trust keeps project context and agent tools disabled for that run. Untrusted non-interactive projects have no project access unless the host explicitly passes `--trust-project`. In an interactive TTY, Nio still asks whether to trust the project folder when it first needs access. On first launch, Nio fetches available models, lists free models first, asks you to choose one, and saves it as the default. For trusted projects, Nio can list, search, and read files automatically. The agent asks before writing files or running shell commands. Kilo's public gateway provides keyless access to its free routes:

```sh
nio
```

Use `:clear` to clear the interactive conversation, `:diff` to review git changes, `:undo` to revert file mutations made by Nio, and `:quit` to exit. File tools stay inside the current directory. Automatic listing/search skips common generated folders, secret filenames, and paths matched by `.gitignore`; individual file reads and writes are limited to 512 KiB. Each search reads at most 16 MiB and returns at most 50 entries. Ignore parsing is bounded to 256 KiB across the scan.

To see the model catalog at any time, run `nio models`. Free models appear first when the provider reports free status or zero pricing. Each choice shows its model name and gateway; Nio routes requests through the selected gateway automatically. You can override the saved model with `-m`:

```sh
nio run -m kilo::kilo-auto/free "Explain this project"
```

Use `:provider` in the interactive CLI, or run `nio provider`, to add or update a provider. Presets are available for OpenRouter, OrcaRouter (free), AIHubMix, Groq, Cerebras, Google Gemini, DeepSeek, Together AI, Fireworks AI, Mistral AI, SiliconFlow, Anthropic Claude, and OpenAI Codex. Claude uses `ANTHROPIC_API_KEY`; Codex uses `OPENAI_API_KEY`; OrcaRouter uses `ORCAROUTER_API_KEY`. Other providers accept their conventional `NIO_<PROVIDER>_API_KEY` variable or a key saved in provider settings. For another service, choose Custom and enter its OpenAI-compatible base URL. All configured catalogs are included in `:model` and `nio models`, with free models first:

```sh
nio
# then enter :provider
nio models
nio run -m aihubmix::provider/model "Summarize this repository"
```

In the interactive CLI, enter `:bash` or `:command` to switch to a direct shell prompt in the project directory; enter `:ai` to return to Nio. `:` and `/` both work for interactive commands; for example, `:approval` and `/approval`, or `:setting` and `/setting`, are equivalent.

Shell commands use `sh` on Unix-like systems and `cmd.exe` on Windows. Command execution uses bounded capture, a 120-second timeout, and clean process group termination on cancellation.

Enter `:path` (alias `:workingpath`) to show the current project directory. Enter `:undo` to roll back the last agent file mutation. Enter `:diff` to see the current git diff.

Use `:proxy` to route all model API requests through an HTTP or HTTPS proxy, or set `NIO_PROXY` to override the saved proxy. Its local presets are Tinyproxy at `http://127.0.0.1:8888` and Squid at `http://127.0.0.1:3128`; install and start the selected service first. The connectivity check probes Kilo and each configured provider's `/models` endpoint without sending API keys. Use a standard HTTP(S) proxy that supports CONNECT tunnels for HTTPS traffic; a CORS relay is not a general-purpose API proxy. Use only a proxy you own or are authorized to use; public proxies can observe prompts and authorization headers. Nio does not bundle or recommend a free public proxy.

In the interactive CLI, use `:mode` to choose Ask, Plan, or Build. Ask answers questions and can inspect files without changing them; Plan inspects the project and returns a plan; Build can edit files and run commands after approval. Approval prompts are on by default. Use `:approval` or `:setting` to toggle automatic approval for file writes and shell commands; this preference persists in Nio's user config. The `--auto` flag enables automatic approval for a single `nio run` invocation. Use `:reasoning` or `:setting` to set reasoning effort to low, medium, high, or the provider default. Use `:theme` to preview and select a persistent terminal palette: Default, Ocean, Forest, Sunset, Dracula, Nord, Solarized, or Monokai. The same choice is available in `:setting` and via `nio config get/set theme`.

Model catalogs include all available models, with free models listed first when the provider reports pricing or free status. Use `nio models` to see the catalog; in `:model`, press Left/Right to page through 25 choices and Up/Down to move between choices. Provider IDs appear as the left side of a selector, for example `openrouter::provider/model`. Provider access, free models, and quotas depend on the provider and may change. API keys saved through `:provider` are kept in Nio's user config; on Unix, the config file is restricted to the current user. Avoid putting API keys directly in shell history.

Interactive model selection pages through 25 entries at a time; use Left/Right to page and Up/Down to move between entries. Search results also paginate at 25 entries.

If OpenRouter models fail to load, check that your network allows HTTPS access to `openrouter.ai`. A network filter may return an HTTP 403 block page or interrupt TLS before Nio can reach the API; an API key cannot bypass that network block.

A one-off OpenAI-compatible endpoint can still be set with `NIO_BASE_URL` or `--base-url` for unqualified model IDs.

Pass `-s` or `--session` to persist and resume a conversation across separate `nio run` calls. Use the same session ID for each turn:

```sh
nio run -s my-chat -m kilo::kilo-auto/free "Check my project"
nio run -s my-chat -m kilo::kilo-auto/free "Now explain the config files"
```

Interactive sessions print a resume command when you exit. Start it with `nio --session <ID>`; one-shot runs print a `nio run -s <ID>` continuation command. Nio creates an ID automatically when needed.

Pass `--format json` with `nio run` to emit NDJSON events, such as `reasoning`, `text`, and `tool_use`, rather than only normal terminal output. See [STREAM.md](STREAM.md) for event shapes and [PROTOCOL.md](PROTOCOL.md) for CLI options, environment variables, trust behavior, and host integration. NoIDE provides NioAI selection through its native subprocess adapter; see the integration section below.

Kilo may rate-limit anonymous use by IP, and its free model availability can change. Its Auto Free route can send prompts to upstream providers with their own data handling terms; avoid sending confidential material unless you have checked those terms.

## Privacy

NioAI includes no telemetry, analytics, tracking, or background usage reporting. Network requests are made to provide features you use, such as model discovery, provider connectivity checks, and model responses.

When you use a model, NioAI sends the prompt and relevant conversation context to the selected model endpoint. If you enable project access, that can include project files read by the agent; text attachments and tool results, including output from approved shell commands, can also be included. The selected provider processes this data under its own terms and may route requests to upstream providers. Review those terms before sending sensitive information.

Provider credentials, configuration, and saved conversation sessions are stored locally. On Unix, configuration and session files are restricted to your user account; they are not encrypted by NioAI. API keys are sent to the configured provider as request credentials. NioAI does not send prompts or project data to an NioAI-operated analytics service.

## Name and command

The project is named NioAI and the command is `nio`. An older, separate NIO platform also provides a `nio` command. If both are installed, use the executable path or adjust `PATH` ordering.

Use `nio --version`, `nio -v`, or `nio --v` to print the installed version.

## License

NioAI is licensed under the [MIT License](LICENSE).

## NoTerm / NoIDE integration

NioAI is available as the `nio` agent through NoIDE's direct subprocess route.
Install a current native `nio` executable on the server's `PATH`; Nio does not
require npm or a persistent agent server. Automatic installation awaits
published, verified native release artifacts.

The Chat UI requests project access before enabling Nio tools. Build consent
also grants file edits and shell execution for that conversation/project.
Ask and Plan are enforced as read-only. Studio uses `--mode ask --no-tools`;
it receives generated text and owns its file writes.

For another host, the explicit interface is:

```sh
nio models --format json
nio run -m kilo::kilo-auto/free --format json --mode ask --reasoning low \
  --trust-project --dir /path/to/project -s project-chat -- "Explain this project"
```

`--trust-project` grants project reads for this invocation. `--no-tools` disables
all project discovery and tools, even for remembered trusted folders. `--auto`
grants writes and shell commands for a single invocation; noninteractive runs
ignore saved automatic approval preferences. Approved commands have the current
user's host access; the project directory is their starting directory, not a
shell sandbox. `NIO_API_KEY` overrides credentials for the selected chat provider
and is not broadcast to model catalogs. Catalogs use provider-specific saved or
environment credentials. `--reasoning` accepts low, medium, high, or default.
`--file` accepts UTF-8 text attachments, with a combined prompt/attachment limit
of 24 KiB. Binary and image attachments are currently unsupported.

JSON runs emit a `session` event with `sessionID` immediately, and persist the
conversation on completion or handled interruption. Sessions are bound to the
canonical project directory and project-access scope, with a lock preventing
simultaneous use. Old array-only sessions have no project binding and require
starting a new session; their files are preserved.

## Resource and reliability limits

- File tools reject excluded directories, parent traversal, and symlinks.
  Discovery visits at most 10,000 entries, to depth 8. Individual reads/writes
  remain capped at 512 KiB; search reads at most 16 MiB and returns at most 50
  bounded snippets. Discovery reads at most 256 KiB of `.gitignore` rules and
  applies common glob, directory, anchoring, and negation patterns.
- HTTP connections have a 10-second deadline; reads have a 60-second inactivity
  deadline; requests have a 300-second total deadline. Catalog requests have a
  15-second deadline. Transient connection failures and HTTP 429/502/503/504
  receive at most three retries with backoff and jitter before response delivery.
- A turn permits 24 model steps and at most 16 tool calls per response. Response
  text is capped at 2 MiB, individual stream events/tool arguments at 1 MiB,
  and wire responses at 8 MiB. Incomplete responses cannot execute tools.
- Context uses a 96 KiB serialized-message budget and drops whole older user
  turns, preserving tool-call/result groups. This is a byte budget, not an exact
  tokenizer or a guarantee for every provider's context window. A turn stops
  with a clear error when it reaches the budget.
- Files, config, and sessions use synced temporary files and atomic replacement.
  Writes preview changed lines and reject stale content. Config and session
  files are private on Unix. Locks prevent simultaneous Nio saves; external
  editors do not participate in these locks.
- Shell commands use a 120-second deadline and bounded captured output. On Unix,
  shell process groups are killed on completion, timeout, or handled cancellation,
  and the shell is reaped. Hosts should send SIGTERM and allow cleanup before
  forcing termination. Native Windows shell support remains pending.
- Follow-up suggestions are off by default to avoid an extra model request.
