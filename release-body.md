# NioAI 0.3.0

## Highlights

- **Project search:** `find_files` supports glob filters; `search_code` adds literal/regex queries, numbered context, and pagination with bounded results.
- **Read web pages:** `web_fetch` reads supplied HTTP(S) URLs without a search API key. HTML is converted to text; browser interaction and JavaScript execution are unsupported.
- **Clarification:** `ask_user` asks a focused question and waits for your next message.
- **Terminal sessions:** start approved commands in Build mode, read incremental output, and cancel sessions. Commands stop when Nio exits.
- **Terminal Markdown:** render `*italics*` and streamed tables with bold/italic cell formatting while preserving code literals.
- **Host integration:** `--no-project-tools` enables page reading, questions, and skills without project file or shell tools. Studio and Canvas use this mode.
- **Simpler setup:** web search and its Brave/SearXNG configuration have been removed.

- **Clearer path errors:** project search reports the requested path and active project, with guidance for selecting another project.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/nio-labs/nio/main/install.sh | NIO_VERSION=v0.3.0 bash
```

Or install the npm launcher once published:

```sh
npm install -g @nio-labs/nio-ai@0.3.0
```

Release assets include Linux (x64/ARM64), macOS (Intel/Apple silicon), Windows (x64), and SHA256 checksums.
