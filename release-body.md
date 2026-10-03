# NioAI 0.3.3

## What's New

- **Voice input & audio LLM support**: Record microphone audio and send voice input to LLMs with `:voice` and `nio voice`, featuring native macOS AVFoundation recording, live recording timer, Whisper speech-to-text, and multimodal audio input.
- **Dedicated visible search input in menus**: Added visible search input box with live multi-term filtering and navigation to `:plugins` and picker menus.
- **Provider failover & recovery**: Prompt to switch providers or retry when connection drops or providers become unavailable.
- **Snippets & NioDE daemon**: Manage reusable code snippets with `:snippets` and background IDE language services with `:ide`.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/nio-labs/nio/main/install.sh | NIO_VERSION=v0.3.3 bash
```

Or, once published to npm:

```sh
npm install -g @nio-labs/nio-ai@0.3.3
```
