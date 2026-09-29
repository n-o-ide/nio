# NioAI stream events

`nio run --format json` writes one JSON object per line to stdout. Provider text is streamed as it arrives. Status and tool events use the event shapes already handled by NoIDE's chat stream parser.

## Progress

```json
{"type":"reasoning","part":{"type":"reasoning","text":"exploring: Scanning project files"}}
{"type":"reasoning","part":{"type":"reasoning","text":"thinking: Thinking"}}
{"type":"reasoning","part":{"type":"reasoning","text":"working: write_file src/main.rs"}}
```

These are short progress summaries, not private model reasoning tokens.

## Text deltas

```json
{"type":"text","part":{"type":"text","text":"The project starts in "}}
{"type":"text","part":{"type":"text","text":"src/main.rs."}}
```

## Tool state

```json
{"type":"tool_use","part":{"type":"tool","callID":"call_1","tool":"read_file","state":{"status":"running","input":{"path":"src/main.rs"},"title":"read_file src/main.rs"}}}
{"type":"tool_use","part":{"type":"tool","callID":"call_1","tool":"read_file","state":{"status":"completed","input":{"path":"src/main.rs"},"output":"...file contents...","title":"read_file src/main.rs"}}}
```

Tool states use `running`, `completed`, or `error`. Writes and shell commands are denied unless the user approves them or `--auto` was explicitly selected. Errors are returned to the model as tool results so it can explain or recover.

`step_finish` marks the end of a response. Errors that prevent a response are printed to stderr and cause a non-zero exit status.
