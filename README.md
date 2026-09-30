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

Running `nio` starts a lightweight, line-based coding agent. On first launch, it fetches available models, lists free models first, asks you to choose one, and saves it as the default. Nio scans the current project and gives the model tools to list, search, and read files automatically. The agent asks before writing files or running shell commands. Kilo's public gateway provides keyless access to its free routes:

```sh
nio
```

Use `:clear` to clear the interactive conversation and `:quit` to exit. Project tools stay inside the current directory. Automatic listing/search skips common generated folders and secret filenames; individual file reads and writes are limited to 512 KiB.

To see the model catalog at any time, run `nio models`. Free models appear first when the provider reports free status or zero pricing. Each choice shows its model name and gateway; Nio routes requests through the selected gateway automatically. You can override the saved model with `-m`:

```sh
nio run -m kilo::kilo-auto/free "Explain this project"
```

Use `:provider` in the interactive CLI, or run `nio provider`, to add or update a provider. Presets are available for OpenRouter, Orca, AIHubMix, Groq, Cerebras, Google Gemini, DeepSeek, Together AI, Fireworks AI, Mistral AI, SiliconFlow, Anthropic Claude, and OpenAI Codex. Claude uses `ANTHROPIC_API_KEY`; Codex uses `OPENAI_API_KEY`; Orca uses `ORCA_API_KEY`. Other providers accept their conventional `NIO_<PROVIDER>_API_KEY` variable or a key saved in provider settings. For another service, choose Custom and enter its OpenAI-compatible base URL. All configured catalogs are included in `:model` and `nio models`, with free models first:

```sh
nio
# then enter :provider
nio models
nio run -m aihubmix::provider/model "Summarize this repository"
```

In the interactive CLI, enter `:bash` or `:command` to switch to a direct shell prompt in the project directory; enter `:ai` to return to Nio.

Use `:proxy` to route model API requests through an HTTP or HTTPS proxy, or set `NIO_PROXY` to override the saved proxy. The connectivity check calls OpenRouter's public model endpoint without sending your provider key. Use only a proxy you own or are authorized to use; public proxies can observe prompts and authorization headers. Nio does not bundle or recommend a free public proxy.

In the interactive CLI, use `:mode` to choose Ask, Plan, or Build. Ask answers questions and can inspect files without changing them; Plan inspects the project and returns a plan; Build can edit files and run commands after approval. Approval prompts are on by default. Use `:approval` or `:setting` to toggle automatic approval for file writes and shell commands; this preference persists in Nio's user config. The `--auto` flag enables automatic approval for a single `nio run` invocation. Use `:reasoning` or `:setting` to set reasoning effort to low, medium, high, or the provider default.

Model catalogs include all available models, with free models listed first when the provider reports pricing or free status. Use `nio models` to see the catalog; in `:model`, press Left/Right to page through 25 choices and Up/Down to move between choices. Provider IDs appear as the left side of a selector, for example `openrouter::provider/model`. Provider access, free models, and quotas depend on the provider and may change. API keys saved through `:provider` are kept in Nio's user config; on Unix, the config file is restricted to the current user. Avoid putting API keys directly in shell history.

If OpenRouter models fail to load, check that your network allows HTTPS access to `openrouter.ai`. A network filter may return an HTTP 403 block page or interrupt TLS before Nio can reach the API; an API key cannot bypass that network block.

A one-off OpenAI-compatible endpoint can still be set with `NIO_BASE_URL` or `--base-url` for unqualified model IDs.

Pass `-s` or `--session` to persist and resume a conversation across separate `nio run` calls. Use the same session ID for each turn:

```sh
nio run -s my-chat -m kilo::kilo-auto/free "Check my project"
nio run -s my-chat -m kilo::kilo-auto/free "Now explain the config files"
```

Interactive sessions print a resume command when you exit. Start it with `nio --session <ID>`; one-shot runs print a `nio run -s <ID>` continuation command. Nio creates an ID automatically when needed.

Kilo may rate-limit anonymous use by IP, and its free model availability can change. Its Auto Free route can send prompts to upstream providers with their own data handling terms; avoid sending confidential material unless you have checked those terms.

## Current scope

The initial implementation supports streamed text and tool calls, project listing/search/read, approved file writes and shell commands, model listing, persistent `-s` sessions, and `nio run --format json` NDJSON events compatible with NoIDE's current chat parser. See [STREAM.md](STREAM.md) for event shapes. Direct NoIDE agent selection is still planned in [PLAN.md](PLAN.md).

## Name and command

The project is named NioAI and the command is `nio`. An older, separate NIO platform also provides a `nio` command. If both are installed, use the executable path or adjust `PATH` ordering.

Use `nio --version`, `nio -v`, or `nio --v` to print the installed version.

## License

NioAI is licensed under the [MIT License](LICENSE).
