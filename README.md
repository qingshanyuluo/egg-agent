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
- [x] Model picker with live model list
- [x] Interactive config wizard
- [x] Streaming reasoning (chain-of-thought) with expand/collapse

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
