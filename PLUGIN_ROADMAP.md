# NioAI Extensions & Plugin Architecture Specification

This document provides the complete, consolidated architectural specification for NioAI's plugin ecosystem, bundle tooling, and custom snippets.

---

## Table of Contents
1. [Core Philosophy: Beyond Read-Only Plugins](#1-core-philosophy-beyond-read-only-plugins)
2. [Interactive Search in `:plugins` and `nio --plugins`](#2-interactive-search-in-plugins-and-nio---plugins)
3. [The 40 Open-Source Vetted Plugins](#3-the-40-open-source-vetted-plugins)
   - [Databases, Stores & Query Engines](#1-databases-stores--query-engines)
   - [Developer Tooling, Code Intelligence & Testing](#2-developer-tooling-code-intelligence--testing)
   - [DevOps, Containers & Infrastructure](#3-devops-containers--infrastructure)
   - [Rich Documents & Perception](#4-rich-documents--perception)
   - [Packaging, Formats & Web Archives](#5-packaging-formats--web-archives)
   - [Systems, Protocols, Media & Utilities](#6-systems-protocols-media--utilities)
4. [Custom Snippets & Functions Subsystem (`:snippets`)](#4-custom-snippets--functions-subsystem-snippets)
5. [The `:ide` Command (`nio-de` Server Bundle Installer)](#5-the-ide-command-nio-de-server-bundle-installer)
6. [Five Strategic System Enhancements](#6-five-strategic-system-enhancements)
   - [1. Post-Edit Formatting Hooks (`fmt-hook`)](#1-post-edit-formatting-hooks-fmt-hook)
   - [2. Git-Based Snippet Sharing](#2-git-based-snippet-sharing)
   - [3. Environment & Health Diagnostic (`nio doctor`)](#3-environment--health-diagnostic-nio-doctor)
   - [4. Model Context Protocol (MCP) Bridge (`nio-mcp`)](#4-model-context-protocol-mcp-bridge-nio-mcp)
   - [5. Interactive TUI Status Dashboard (`:status`)](#5-interactive-tui-status-dashboard-status)
7. [Security, Approval & CI/CD Guarantees](#7-security-approval--cicd-guarantees)
8. [Subprocess Communication Protocol (Protocol 2 Specification)](#8-subprocess-communication-protocol-protocol-2-specification)

---

## 1. Core Philosophy: Beyond Read-Only Plugins

In NioAI, plugins are not passive decoders or file readers. They represent **first-class executable capabilities** running in isolated subprocesses that give the terminal agent distinct powers:

```
┌────────────────────────────────────────────────────────────────────────┐
│                          NioAI Extension Triad                         │
├──────────────────────────┬──────────────────────────┬──────────────────┤
│ 1. Skills (SKILL.md)     │ 2. Plugins (nio-*)       │ 3. Snippets      │
│ How to think & reason    │ Superpowers & Tools      │ Executable code  │
│ (Prompts, Guidelines)    │ (Binaries, Decoders)     │ & templates      │
└──────────────────────────┴──────────────────────────┴──────────────────┘
```

Plugins provide four distinct operational capabilities:
* **Active Agent Tools (Function Calling):** Expose tools with JSON Schemas (e.g. `sqlite_query`, `docker_ps`, `http_request`) that the agent can execute during its reasoning loop.
* **Specialized File Ingestion:** Safe, bounded text and metadata extraction from complex or binary files (PDF, Parquet, OCR, Jupyter Notebooks).
* **Code Intelligence & Diagnostics:** Structured bridges for linters and compilers (Clippy, ESLint, Ruff, AST-grep) that return exact file/line spans for deterministic auto-fixing.
* **Lifecycle Hooks:** Automated actions that run immediately after file modifications (e.g., auto-formatting modified files).

---

## 2. Interactive Search in `:plugins` and `nio --plugins`

With 40 plugins available, scrolling through a linear list is inefficient. The interactive picker in both the inline CLI and full-screen TUI includes **real-time fuzzy and keyword filtering**.

### Interactive Picker UI Layout
```text
┌ Plugins ──────────────────────────────────────────────────────────────────┐
│ / Search: sql_                                             (3 matches)    │
├───────────────────────────────────────────────────────────────────────────┤
│ › 1. SQLite        · Inspect schema & run bounded queries (.db, .sqlite)  │
│   2. DuckDB        · In-process analytical SQL for CSV and Parquet        │
│   3. Postgres Lite · Read-only dev DB schema and relationship explorer   │
└───────────────────────────────────────────────────────────────────────────┘
↑/↓ move · Enter select · Backspace delete · Esc clear/exit
```

### Search Mechanics
* **Trigger:** Pressing `/` (or directly typing characters) opens the search bar.
* **Indexed Fields:** Matches against plugin name, description, supported file extensions, and category search tags.
* **Navigation:** `Up`/`Down` (or `k`/`j`) navigates filtered results; `Enter` selects; `Esc` clears the search or exits.
* **CLI Search:** `nio --plugins search <query>` provides quick non-interactive terminal search.

---

## 3. The 40 Open-Source Vetted Plugins

All 40 plugins are built on **100% commercially permissive open-source libraries** (**MIT**, **Apache-2.0**, **BSD**, or **Public Domain**). None use copyleft (GPL/AGPL) or restrictive commercial licenses (SSPL/BSL).

### 1. Databases, Stores & Query Engines
| # | Plugin Name | Capabilities | Open Source Engine / Crate | License | Search Tags |
|:--|:---|:---|:---|:---|:---|
| 1 | `sqlite` | Schema inspect & bounded read-only queries (`SELECT ... LIMIT 20`) | `rusqlite` / SQLite | MIT / Public Domain | `db, sql, sqlite, sqlite3, query` |
| 2 | `duckdb` | Fast analytical SQL on local CSV, Parquet, and JSON | `duckdb-rs` / DuckDB | MIT | `duckdb, analytics, olap, parquet, sql` |
| 3 | `parquet` | Arrow/Parquet schema, column stats (nulls, min/max), row sampling | `arrow-rs` / `parquet` | Apache-2.0 | `parquet, arrow, data, analytics, schema` |
| 4 | `postgres-lite` | Read-only connection to dev PostgreSQL; schema & FK relationship mapper | `tokio-postgres` | MIT / Apache-2.0 | `postgres, psql, database, sql, query` |
| 5 | `redis-tool` | Read-only Redis/Valkey key explorer, TTL inspector, type analyzer | `redis-rs` / Valkey | MIT / BSD-3-Clause | `redis, cache, key-value, ttl, memory` |
| 6 | `data-profiler` | High-speed tabular statistics profiler (types, nulls, distributions) | `csv` / `serde_json` | MIT / Unlicense | `csv, tsv, jsonl, statistics, profile` |

### 2. Developer Tooling, Code Intelligence & Testing
| # | Plugin Name | Capabilities | Open Source Engine / Crate | License | Search Tags |
|:--|:---|:---|:---|:---|:---|
| 7 | `linter-bridge` | Captures compiler & linter diagnostics (Clippy, ESLint, Ruff) for auto-patching | CLI Bridge / JSON parser | MIT / Apache-2.0 | `linter, clippy, eslint, ruff, fix` |
| 8 | `test-reporter` | Runs tests (`cargo test`, `pytest`, `vitest`) and isolates failed assertions | `quick-xml` / JSON parser | MIT / Apache-2.0 | `test, pytest, cargo-test, failures` |
| 9 | `ast-grep` | Structural code search using concrete syntax trees | `tree-sitter` | MIT | `ast, syntax, tree-sitter, search` |
| 10 | `http-client` | Structured REST client to test dev servers (status, headers, latency) | `reqwest` | MIT / Apache-2.0 | `http, curl, api, rest, test, request` |
| 11 | `openapi` | OpenAPI v3 / Swagger spec parser, route lister, payload validator | `openapiv3` | MIT / Apache-2.0 | `openapi, swagger, yaml, api-schema` |
| 12 | `git-advanced` | Semantic blame, commit divergence graphing, merge conflict helper | `gix` (Gitoxide) | MIT / Apache-2.0 | `git, blame, diff, branches, merge` |
| 13 | `benchmark-runner`| Runs micro-benchmarks and summarizes performance regressions | `criterion` / `hyperfine` | Apache-2.0 / MIT | `bench, benchmark, perf, latency` |

### 3. DevOps, Containers & Infrastructure
| # | Plugin Name | Capabilities | Open Source Engine / Crate | License | Search Tags |
|:--|:---|:---|:---|:---|:---|
| 14 | `docker-inspector`| Safe container status inspect, log tailing, port mappings | `bollard` (Docker API) | Apache-2.0 | `docker, containers, compose, logs, ps` |
| 15 | `k8s-view` | Read-only pod logs, events, and deployment configurations | `kube-rs` | Apache-2.0 | `k8s, kubernetes, pods, helm, cluster` |
| 16 | `env-guard` | Scans staged code for leaked API keys, tokens, and secrets | Pure Rust regex scanner | MIT / Apache-2.0 | `env, secrets, dotenv, security, leak` |
| 17 | `terraform-check`| Validates HCL syntax and summarizes planned resource additions | `hcl-rs` | MIT / Apache-2.0 | `terraform, hcl, iac, plan, validate` |
| 18 | `pcap-inspector` | Summarizes packet capture files (DNS queries, TLS handshakes, HTTP) | `pcap-file` / `etherparse` | MIT / Apache-2.0 | `pcap, network, packet, tcpdump, traffic` |
| 19 | `system-info` | Reports system architecture, open ports, memory pressure, and CPU | `sysinfo` | MIT | `os, cpu, memory, disk, ports, hardware` |

### 4. Rich Documents & Perception
| # | Plugin Name | Capabilities | Open Source Engine / Crate | License | Search Tags |
|:--|:---|:---|:---|:---|:---|
| 20 | `pdf` *(Existing)* | PDF text extraction with optional multi-language Tesseract OCR | `pdf-extract` + Tesseract | MIT / Apache-2.0 | `pdf, ocr, text, document` |
| 21 | `image-ocr` | Text extraction from screenshots, error popups, and diagrams | Tesseract OCR engine | Apache-2.0 | `ocr, image, png, jpg, screenshot` |
| 22 | `diagram-renderer`| Validates Mermaid/PlantUML syntax and outputs ASCII or SVG previews | Pure Rust AST / `kroki` | MIT | `mermaid, plantuml, diagram, ascii` |
| 23 | `epub` | Technical documentation and ebook reader with TOC and chapter pagination | `epub` crate | MIT | `epub, books, docs, chapters` |
| 24 | `legacy-office` | Text extraction from older binary Word (`.doc`) and PowerPoint (`.ppt`) | `ole` / `dotext` | MIT | `doc, ppt, rtf, office, legacy` |
| 25 | `rtf-reader` | Decodes Rich Text Format files with Unicode escapes | `rtf-parser` | MIT | `rtf, formatting, rich-text` |
| 26 | `font-inspector`| Inspects `.ttf`, `.otf`, `.woff2` font tables, glyph coverage, metadata | `ttf-parser` | MIT / Apache-2.0 | `font, ttf, otf, woff2, typography` |

### 5. Packaging, Formats & Web Archives
| # | Plugin Name | Capabilities | Open Source Engine / Crate | License | Search Tags |
|:--|:---|:---|:---|:---|:---|
| 27 | `archive-explorer`| Safe directory tree preview and bounded extraction (`.zip`, `.tar`, `.7z`) | `zip`, `tar`, `sevenz-rust` | MIT / Apache-2.0 | `zip, tar, gzip, 7z, archive, extract` |
| 28 | `notebook-engine`| Clean Jupyter `.ipynb` extractor (strips base64 image bloat, keeps cells) | `serde_json` | MIT / Apache-2.0 | `ipynb, jupyter, notebook, python, cells` |
| 29 | `web-archive` | Extracts title, text, headers, and asset lists from `.mhtml` and `.warc` | `mail-parser` / `warc` | Apache-2.0 / MIT | `mhtml, warc, web, snapshot, offline` |
| 30 | `email-reader` | Parses `.eml`, `.msg`, `.mbox` headers, message bodies, and attachments | `mail-parser` | Apache-2.0 | `eml, msg, mbox, email, headers, mail` |
| 31 | `calendar-contacts`| Decodes iCalendar `.ics` events and vCard `.vcf` contact cards | `icalendar` | MIT / Apache-2.0 | `ics, vcf, calendar, contacts, vcard` |
| 32 | `geospatial` | Extracts features, bounding coordinates, and geometries from GeoJSON/GPX | `geojson` / `gpx` | MIT / Apache-2.0 | `geojson, gpx, kml, gis, map` |
| 33 | `wasm-inspector`| Inspects WebAssembly `.wasm`/`.wat` exports, imports, sections | `wasmparser` | Apache-2.0 w/ LLVM | `wasm, wat, webassembly, binary` |

### 6. Systems, Protocols, Media & Utilities
| # | Plugin Name | Capabilities | Open Source Engine / Crate | License | Search Tags |
|:--|:---|:---|:---|:---|:---|
| 34 | `log-analyzer` | Streaming log parser: groups error patterns and graphs event timestamps | `regex` / `memmap2` | MIT / Apache-2.0 | `log, logs, syslog, trace, error` |
| 35 | `audio-transcribe`| Local speech-to-text for audio bug reports, meeting notes (`whisper.cpp`) | `whisper.cpp` | MIT | `audio, speech, whisper, mp3, wav` |
| 36 | `protobuf-inspector`| Decodes `.proto` schemas and inspects binary `.pb` protobuf payloads | `prost-reflect` | Apache-2.0 | `protobuf, proto, pb, grpc, schema` |
| 37 | `graphql-schema` | Validates `.graphql` SDL schemas, queries, mutations, deprecations | `async-graphql-parser` | MIT / Apache-2.0 | `graphql, schema, sdl, query, api` |
| 38 | `hex-inspector` | Binary file hex dump, magic byte detection, Shannon entropy for corrupt files | Pure Rust (`byteorder`) | MIT / Apache-2.0 | `hex, binary, entropy, bytes, debug` |
| 39 | `diff-visualizer`| Syntactic side-by-side diff summaries between files or git revisions | `similar` | Apache-2.0 | `diff, patch, compare, syntax` |
| 40 | `markdown-linter`| Structural Markdown validator, frontmatter checker, broken relative link check | `pulldown-cmark` | MIT | `markdown, md, lint, frontmatter, links` |

---

## 4. Custom Snippets & Functions Subsystem (`:snippets`)

Snippets provide a **custom, language-agnostic code cookbook** that the agent uses during task execution:

### Storage Locations
* **Project Toolbox:** `<project-root>/.nio/snippets/` (committed to git, shared across the team).
* **Global Toolbox:** `~/.nio/snippets/` (personal developer recipes across all repositories).

### File Format: Self-Documenting Header Comments
Snippets are stored as ordinary code files in any language (`.py`, `.sh`, `.ts`, `.rs`, `.sql`, `.go`):

```python
# snippet: seed-sqlite
# description: Populates local development database with mock customers and orders
# tags: db, sqlite, seed, mock
# runner: python3

import sqlite3
# ... executable implementation ...
```

### Agent Integration & Two Execution Modes
1. **Code Template Adaptation:** When tasked with writing new code (e.g. an API endpoint or auth handler), Nio searches snippets to reuse your team's vetted patterns rather than generating generic boilerplate.
2. **Callable Utility Scripts:** When tasked with operational steps (e.g. `reset_cache.sh`, `gen_proto.sh`), the agent discovers the script and executes it with approved permissions.

### Tools & Commands
* `:snippets` — Opens the interactive snippet explorer.
* `nio --snippets list` / `nio --snippets search <query>` — CLI discovery.
* Model tools: `search_snippets(query)` and `run_snippet(name, args)`.

---

## 5. The `:ide` Command (`nio-de` Server Bundle Installer)

The `:ide` command is a host bundle installer (not an LLM model tool) that provisions and manages the `nio-de` development environment daemon on local machines or cloud VMs.

### Command Capabilities
* `nio ide` or `:ide`: Checks server status, process ID (PID), listening port, and access token.
* `nio ide install`: Detects host OS (`macos`, `linux`) and architecture (`x86_64`, `aarch64`), downloads the pre-built `nio-de` binary, verifies its SHA-256 checksum, and installs it to `~/.nio/bin/nio-de` with `0o755` permissions.
* `nio ide start [--port 8080]`: Launches the IDE server in the background as a detached daemon.
* `nio ide stop`: Safely terminates the background daemon.
* `nio ide update`: Replaces the current binary with the latest verified release.

---

## 6. Five Strategic System Enhancements

### 1. Post-Edit Formatting Hooks (`fmt-hook`)
* **Mechanism:** When `write_file` or `patch_file` modifies code, Nio automatically triggers any installed formatting plugin (such as `rustfmt`, `prettier`, or `ruff`) on the modified file.
* **Result:** Eliminates whitespace regressions, trailing commas, and formatting drift across git diffs.

### 2. Git-Based Snippet Sharing
* **Command:** `nio --snippets add https://github.com/my-org/nio-snippets`
* **Mechanism:** Reuses Nio's proven skill cloning logic to pull shared snippet repositories into `.nio/snippets/`, keeping team toolboxes in sync.

### 3. Environment & Health Diagnostic (`nio doctor`)
* **Output:**
  ```text
  $ nio doctor
  ✓ Model Provider: Kilo API reachable (latency: 142ms)
  ✓ PDF Plugin: Installed (v0.3.2)
    ! OCR: Tesseract found; Poppler (pdftoppm) missing from PATH (brew install poppler)
  ✓ NioDE Server: Installed (~/.nio/bin/nio-de), stopped
  ✓ Snippets: 14 project snippets, 6 global snippets loaded
  ```

### 4. Model Context Protocol (MCP) Bridge (`nio-mcp`)
* A lightweight adapter plugin allowing any standard community MCP server (over stdio) to run natively as a Nio plugin without custom code.

### 5. Interactive TUI Status Dashboard (`:status`)
* A TUI panel reporting active session tokens, context window budget usage %, `nio-de` server port/PID, and active plugin memory bounds.

---

## 7. Security, Approval & CI/CD Guarantees

### Strict Installation Boundaries
* **Privilege Escalation Protection:** Installing a plugin downloads external executable code to the host. Therefore, **plugin installation always requires explicit user confirmation across Ask, Plan, and Build modes**, even when `--auto` is enabled.
* **Graceful Decline:** If a user rejects installation, Nio receives `Tool error: user denied plugin installation`. The model gracefully skips reading that file and continues the turn without crashing or looping.

### CI/CD Headless Automation Best Practices
In automated pipelines (GitHub Actions, GitLab CI):
```bash
# 1. Pre-install required plugins during setup
nio --plugins install sqlite
nio --plugins install pdf

# 2. Run one-shot headless tasks non-interactively
nio run -m kilo::kilo-auto/free --trust-project --auto "Run lint and review diff"
```

---

## 8. Subprocess Communication Protocol (Protocol 2 Specification)

Plugins communicate with Nio through **bounded, timed JSON-RPC over stdin/stdout**:

### 1. Capability Discovery (`describe`)
```json
// Request from Nio to Plugin via stdin:
{ "protocol": 2, "operation": "describe" }

// Response from Plugin to Nio via stdout:
{
  "protocol": 2,
  "kind": "tool_provider", // "file_reader" | "tool_provider" | "analyzer"
  "file_extensions": ["sqlite", "db", "sqlite3"],
  "tools": [
    {
      "name": "sqlite_query",
      "description": "Execute bounded read-only SQL query on a SQLite database",
      "parameters": {
        "type": "object",
        "properties": {
          "path": { "type": "string" },
          "sql": { "type": "string" },
          "limit": { "type": "integer", "default": 20 }
        },
        "required": ["path", "sql"]
      }
    }
  ]
}
```

### 2. Tool Execution (`call_tool`)
```json
// Request:
{
  "protocol": 2,
  "operation": "call_tool",
  "tool": "sqlite_query",
  "arguments": {
    "path": "dev.db",
    "sql": "SELECT id, email, created_at FROM users LIMIT 5"
  }
}

// Response:
{
  "protocol": 2,
  "result": "[{\"id\": 1, \"email\": \"alice@example.com\"}]"
}
```

### 3. File Ingestion (`read`)
```json
// Request:
{
  "protocol": 2,
  "operation": "read",
  "path": "/project/data/events.parquet",
  "data_dir": "/Users/user/.nio/plugins/parquet"
}
```

### Response:
```json
{
  "protocol": 2,
  "text": "Schema: user_id (int64), timestamp (datetime64), payload (string)\nRows: 1,420,000 across 12 row groups.\nSample rows:\n..."
}
```

---

## 9. Provider Failover & Auto-Recovery (`:provider` Fallback)

When the active provider (e.g. Kilo Gateway) encounters an outage, timeout, or HTTP 500/502/503/504 error during an interactive turn, Nio presents a clean, focused two-choice recovery picker instead of stranding the user with an unhandled error:

### Interactive Fallback Menu
```text
┌ Provider Unavailable ──────────────────────────────────────────┐
│ › 1. Select a provider (:provider)                            │
│   2. Retry connection                                         │
└───────────────────────────────────────────────────────────────┘
↑/↓ move · Enter select · Esc cancel
```

### Action Behaviors
1. **Option 1: Select a provider (`:provider`):**
   - Directly launches the standard `:provider` configuration screen (`configure_provider()`).
   - Allows selecting another provider (OpenRouter, Groq, Gemini, etc.).
   - Upon selection, Nio updates the active session and automatically retries the pending turn with the new provider.
2. **Option 2: Retry connection:**
   - Retries the current request against the active provider immediately.
3. **Esc / Cancel:**
   - Cancels the pending turn, pauses the message queue, and returns cleanly to the `nio>` prompt.

