//! Command-line entry: argument dispatch and helper utilities.
//!
//! The CLI is intentionally minimal — most actions (`/model`, `/connect`) are
//! slash commands inside the TUI. The only CLI verbs are `egg` (launch),
//! `egg --resume` (restore session), `egg help`, and `egg version`.

use crate::config::{Config, ProviderConfig};

const HELP: &str = "\
egg — a minimal OpenAI-compatible SWE agent in your terminal

USAGE:
    egg                         Start the interactive TUI agent
    egg --resume [session-id]   Resume a saved session (pick from list if no id given)
    egg help                    Show this help
    egg version                 Show the version

INSIDE THE TUI:
    /model              Fetch available models from all providers and switch
    /connect            Connect a named API provider: /connect <name> <api_key> [base_url]
    /connect-remove     Remove a named provider: /connect-remove <name>

CONFIG:
    Stored at ~/.egg-agent/config.toml
    Environment overrides: EGG_API_KEY, EGG_BASE_URL, EGG_MODEL
";

/// What the parsed command line asks us to do.
pub enum Command {
    /// Launch the TUI.
    Run,
    /// Launch the TUI resuming a saved session (optionally by id).
    Resume(Option<String>),
    /// Print help.
    Help,
    /// Print version.
    Version,
}

pub fn parse_args() -> Command {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None => Command::Run,
        Some("--resume" | "resume") => Command::Resume(args.next()),
        Some("help" | "-h" | "--help") => Command::Help,
        Some("version" | "-V" | "--version") => Command::Version,
        _ => Command::Help,
    }
}

pub fn print_help() {
    print!("{HELP}");
}

pub fn print_version() {
    println!("egg {}", env!("CARGO_PKG_VERSION"));
}

// ---- Slash-command handlers (called from the TUI loop) ----

/// Try to handle a slash command typed into the input box.
/// Returns `Some(message)` if the command was handled (the message is shown
/// in the transcript), or `None` if the input isn't a recognized slash command.
pub fn handle_slash_command(input: &str, app: &mut crate::app::App) -> Option<String> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    if trimmed == "/connect" {
        app.open_connect_wizard();
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix("/connect-remove ") {
        return Some(do_connect_remove(rest.trim(), app));
    }

    if let Some(rest) = trimmed.strip_prefix("/connect ") {
        // Guard: don't treat "/connect-remove" as a connect command.
        if rest.starts_with("-remove") {
            return None;
        }
        let result = do_connect(rest.trim(), app);
        if result.is_empty() {
            return None;
        }
        return Some(result);
    }

    None
}

fn do_connect(args: &str, app: &mut crate::app::App) -> String {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        app.open_connect_wizard();
        return String::new();
    }
    let name = parts[0].to_lowercase();
    if name == "default" {
        return "provider name cannot be 'default'".to_string();
    }
    if parts.len() < 2 {
        app.open_connect_wizard();
        return String::new();
    }
    let api_key = parts[1].to_string();
    let base_url = if parts.len() >= 3 {
        parts[2].trim_end_matches('/').to_string()
    } else {
        "https://api.openai.com/v1".to_string()
    };

    let mut config = match Config::load_file_or_default() {
        Ok(c) => c,
        Err(e) => return format!("failed to load config: {e}"),
    };

    if config.providers.contains_key(&name) {
        return format!(
            "provider '{name}' already exists — use /connect-remove {name} first"
        );
    }

    config.providers.insert(
        name.clone(),
        ProviderConfig { api_key, base_url },
    );

    match config.save() {
        Ok(path) => {
            // Refresh the app's provider list for the model picker.
            app.refresh_providers(&config);
            format!(
                "connected provider '{name}' — saved to {}\n  use /model to pick a model from this provider",
                path.display()
            )
        }
        Err(e) => format!("failed to save config: {e}"),
    }
}

fn do_connect_remove(name: &str, app: &mut crate::app::App) -> String {
    if name.is_empty() || name == "default" {
        return "cannot remove the default provider".to_string();
    }
    let mut config = match Config::load_file_or_default() {
        Ok(c) => c,
        Err(e) => return format!("failed to load config: {e}"),
    };
    if config.providers.remove(name).is_none() {
        return format!("provider '{name}' not found");
    }
    match config.save() {
        Ok(path) => {
            app.refresh_providers(&config);
            format!("removed provider '{name}' — saved to {}", path.display())
        }
        Err(e) => format!("failed to save config: {e}"),
    }
}

// ---- helpers, shared with main.rs ----

/// Derive a short provider name from the base URL host, for display.
pub fn provider_label(base_url: &str) -> String {
    let host = base_url
        .split("://")
        .nth(1)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or("")
        .trim_start_matches("api.");
    let parts: Vec<&str> = host.split('.').collect();
    let name = if parts.len() >= 2 {
        parts[parts.len() - 2]
    } else {
        host
    };
    if name.is_empty() {
        "local".to_string()
    } else {
        name.to_string()
    }
}

/// Mask a secret so we don't leak the whole key.
pub fn mask(secret: &str) -> String {
    let n = secret.chars().count();
    if n == 0 {
        "(not set)".to_string()
    } else if n <= 8 {
        "*".repeat(n)
    } else {
        let head: String = secret.chars().take(4).collect();
        let tail: String = secret.chars().skip(n - 4).collect();
        format!("{head}...{tail}")
    }
}
