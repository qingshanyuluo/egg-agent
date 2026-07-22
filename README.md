# egg-agent

A terminal-based AI coding agent TUI written in Rust, inspired by [opencode](https://opencode.ai).

## Status

Early scaffold — the TUI shell (message list + input box) works; no model backend is wired up yet.

## Features

- [x] ratatui-based TUI: conversation view + input box
- [ ] LLM provider integration
- [ ] Tool calling (file edit, shell, ...)
- [ ] Session persistence

## Build & Run

```sh
cargo run
```

- `Enter` — send message
- `Esc` / `Ctrl-C` — quit

## Stack

- [ratatui](https://github.com/ratatui/ratatui) + [crossterm](https://github.com/crossterm-rs/crossterm) for the TUI
- [tokio](https://tokio.rs) for async runtime
