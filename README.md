# egg-agent

A terminal-based AI coding agent TUI written in Rust, inspired by [opencode](https://opencode.ai).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/qingshanyuluo/egg-agent/main/install.sh | sh
```

Or download a binary from the [Releases](https://github.com/qingshanyuluo/egg-agent/releases) page.

Supports:
- macOS (Apple Silicon / Intel)
- Linux (x86_64 / ARM64)

## Setup

Start the TUI and connect your API provider:

```sh
egg                   # Launch the TUI
```

Then type `/connect` and follow the wizard, or pass credentials directly:

```
/connect deepseek sk-xxxxxxxx https://api.deepseek.com/v1
```

Or set environment variables:

```sh
export EGG_API_KEY="sk-..."
export EGG_BASE_URL="https://api.deepseek.com/v1"
export EGG_MODEL="deepseek-chat"
```

## Usage

```sh
egg                  # Start a new session
egg --resume         # Resume a saved session (interactive pick)
egg --resume <id>    # Resume by id (e.g. 20260723-1530)
```

Slash commands inside the TUI:

- `/model` — open the model picker (fetches live model list from all connected providers)
- `/connect` — open the connect-wizard to add a provider
- `/connect <name> <api_key> [base_url]` — connect a provider directly
- `/connect-remove <name>` — remove a named provider
- `/memory` — toggle auto experience-memory archival on/off

Keybindings in the TUI:

- `Enter` — send message
- `Shift+Enter` / `Alt+Enter` — newline in input
- `Esc` — cancel running turn / clear input
- `Ctrl+C` — clear input (first press) / quit (second press)
- `Up/Down` — navigate input history
- `PgUp/PgDn` — scroll transcript
- `/` on empty input — command palette
- Mouse — select/copy text, click reasoning to expand/collapse

## Features

- [x] ratatui-based TUI with streaming conversation view
- [x] LLM provider integration (OpenAI-compatible, with retry)
- [x] Tool calling: bash, read_file, write_file, edit_file, search
- [x] Session persistence (save/resume)
- [x] Plugin system: translation, bash explanation, clipboard copy
- [x] Auto experience memory: detects "struggle then success" turns and distills them into `~/.egg-agent/memory/`
- [x] Model picker with live model list
- [x] Interactive config wizard
- [x] Streaming reasoning (chain-of-thought) with expand/collapse

## Experience Memory

egg archives **experience notes** from hard-won turns via a three-stage
funnel, each stage cheaper than the next:

1. **Heuristic** — a finished turn with ≥ `min_tool_calls` tool calls
   (default 6) counts as a "complex exploration".
2. **Screening** — the cheap `[aux]` model judges whether the turn was
   genuine trial-and-error that ended in success ("反复尝试摸清环境后成功").
   Smooth or mechanical turns are dropped here. Skipped when no `[aux]`
   is configured.
3. **Summarizing** — a (ideally stronger) model distills the trajectory
   into a Markdown note under `~/.egg-agent/memory/<scope>/<category>/`:
   what went wrong and why, the final working path, generalized lessons,
   tags. `<scope>` is `global` for project-agnostic lessons or the current
   repo name for project-specific ones; `<category>` is a 1-2 level topic
   path (e.g. `rust/cargo`) the summarizer picks, preferring categories
   that already exist — notes form a skill-style topic tree, not a
   chronological log.

Archival is fully asynchronous and never touches the conversation. Tune via
`[memory]` in `~/.egg-agent/config.toml`:

```toml
[memory]
# Summarizer model — point this at your smartest model. Empty = main model.
model = "deepseek-reasoner"
# base_url = "https://api.deepseek.com/v1"   # falls back to main provider
# api_key = "sk-..."                          # falls back to main key
min_tool_calls = 6
enabled = true
```

## Build from Source

```sh
git clone https://github.com/qingshanyuluo/egg-agent.git
cd egg-agent
cargo build --release
```

## Stack

- [ratatui](https://github.com/ratatui/ratatui) + [crossterm](https://github.com/crossterm-rs/crossterm) for the TUI
- [tokio](https://tokio.rs) for async runtime
- [reqwest](https://github.com/seanmonstar/reqwest) for HTTP/streaming
