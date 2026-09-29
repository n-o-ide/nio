# NioAI

A lightweight AI coding agent for the terminal. The product is called **NioAI**; the executable is **`nio`**.

NioAI is being built as a standalone public CLI. It sends requests to configurable OpenAI-compatible model endpoints. Provider access, model availability, and free quotas are controlled by the provider and may change.

## Requirements

- Rust toolchain to build from source
- Kilo gateway access for free models; API keys are needed for paid/provider-account routes

## Build

```sh
cargo build --release
```

The executable is at `target/release/nio`.

## Configure and run

Running `nio` starts a lightweight, line-based coding agent. On first launch, it fetches available free models, asks you to choose one, and saves it as the default. Nio scans the current project and gives the model tools to list, search, and read files automatically. The agent asks before writing files or running shell commands. Kilo's public gateway provides keyless access to its free routes:

```sh
nio
```

Use `:clear` to clear the interactive conversation and `:quit` to exit. Project tools stay inside the current directory. Automatic listing/search skips common generated folders and secret filenames; individual file reads and writes are limited to 512 KiB.

To see free models at any time, run `nio models`. Each choice shows its model name and gateway; Nio routes requests through the selected gateway automatically. You can override the saved model with `-m`:

```sh
nio run -m kilo::kilo-auto/free "Explain this project"
```

OpenRouter models appear when an OpenRouter key is available:

```sh
export OPENROUTER_API_KEY="your-provider-key"
nio models
nio run -m openrouter::provider/model "Summarize this repository"
```

Use `nio models --all` to include paid models. A custom OpenAI-compatible endpoint can be set with `NIO_BASE_URL` or `--base-url` for unqualified model IDs.

OpenCode stores provider credentials in its own local auth file, so users do not need to export a key for every run. NioAI currently does not read that file or provide OpenCode-style sign-in; for providers that require credentials, configure the provider key in the environment. OpenCode Zen's current setup flow asks users to sign in, add billing details, and paste a Zen API key, even when selecting a free model.

Kilo may rate-limit anonymous use by IP, and its free model availability can change. Its Auto Free route can send prompts to upstream providers with their own data handling terms; avoid sending confidential material unless you have checked those terms.

Avoid putting API keys directly in shell history.

## Current scope

The initial implementation supports streamed text and tool calls, project listing/search/read, approved file writes and shell commands, model listing, and `nio run --format json` NDJSON events compatible with NoIDE's current chat parser. See [STREAM.md](STREAM.md) for event shapes. Persistent sessions and direct NoIDE agent selection are still planned in [PLAN.md](PLAN.md).

## Name and command

The project is named NioAI and the command is `nio`. An older, separate NIO platform also provides a `nio` command. If both are installed, use the executable path or adjust `PATH` ordering.

## License

NioAI is licensed under the [MIT License](LICENSE).
