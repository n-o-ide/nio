# NioAI 0.2.0

NioAI 0.2.0 adds a full-screen terminal interface, queued prompts, GitHub skills, and file/image attachments, alongside improvements to session history and terminal rendering.

## Highlights

- **Full-screen TUI:** launch with `nio --tui`; use themed panels for settings, sessions, models, skills, and queued messages. Theme selection previews live, with Tokyo Night and muted light palettes.
- **Prompt queue:** queue messages while Nio is working, then review, edit, remove, pause, or resume them.
- **GitHub skills:** install skills from GitHub repositories and list, enable, disable, or remove them with `nio --skills` or interactive commands.
- **File and image input:** attach UTF-8 text, PNG, JPEG, GIF, or WebP files with `--file`, `@path`, dropped paths, or absolute paths in interactive prompts. `read_file` can read explicitly requested absolute local paths and send images to vision-capable models.
- **Session history:** switch among saved conversations, resume recent context, and identify sessions by their first user prompt.
- **Terminal interaction:** improve prompt editing, mouse and trackpad scrolling, Markdown formatting, tool change summaries, and progress feedback.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/nio-labs/nio/main/install.sh | bash
```

Or install the npm launcher:

```sh
npm install -g nio-ai
```
