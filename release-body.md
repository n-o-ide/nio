# NioAI 0.3.1

## Fix

- Delay the assistant label until visible text arrives. Tool-only responses containing whitespace no longer show empty `nio:` replies.
- Keep working status visible while Markdown tables and other formatting are buffered.
- Preserve leading styles and render tables that finish at the end of a response.

Includes the bounded project search, URL reading, clarification, terminal sessions, Markdown improvements, and Studio/Canvas integration introduced in v0.3.0. `web_fetch` needs no search API key; web search remains removed.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/nio-labs/nio/main/install.sh | NIO_VERSION=v0.3.1 bash
```

Or, once the npm package is published:

```sh
npm install -g @nio-labs/nio-ai@0.3.1
```
