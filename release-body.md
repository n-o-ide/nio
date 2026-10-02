# NioAI 0.3.2

## Fix

- Wrap long fenced code lines within the frame instead of allowing terminal wrapping to break the layout.
- Account for response indentation, frame prefixes, wide Unicode characters, and tabs.
- Preserve the full code text on continuation lines, including unfinished streamed code blocks.

Includes the blank assistant label fix from v0.3.1 and all v0.3.0 tools. `web_fetch` needs no search API key; web search remains removed.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/nio-labs/nio/main/install.sh | NIO_VERSION=v0.3.2 bash
```

Or, once published to npm:

```sh
npm install -g @nio-labs/nio-ai@0.3.2
```
