//! Command-line entry: argument dispatch, the interactive `config` wizard,
//! and model selection.

use std::io::{self, Write};

use anyhow::{Context, Result};
use inquire::Select;

use crate::config::Config;
use crate::llm::openai::list_models;

const HELP: &str = "\
egg — a minimal OpenAI-compatible SWE agent in your terminal

USAGE:
    egg                         Start the interactive TUI agent
    egg --resume [session-id]   Resume a saved session (pick from list if no id given)
    egg config                  Configure API key / base URL / model (interactive)
    egg model           Fetch available models and switch (interactive)
    egg config path     Print the path to the config file
    egg config show     Print the current config (api key masked)
    egg help            Show this help
    egg version         Show the version

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
    /// Run the interactive config wizard.
    Config,
    /// Fetch models and switch the active one.
    Model,
    /// Print the config file path.
    ConfigPath,
    /// Print the current config with the key masked.
    ConfigShow,
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
        Some("config") => match args.next().as_deref() {
            None => Command::Config,
            Some("path") => Command::ConfigPath,
            Some("show") => Command::ConfigShow,
            _ => Command::Help,
        },
        Some("model") => Command::Model,
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

pub fn print_config_path() -> Result<()> {
    println!("{}", Config::path()?.display());
    Ok(())
}

pub fn print_config_show() -> Result<()> {
    let path = Config::path()?;
    if !path.exists() {
        println!("No config yet. Run `egg config` to create one.");
        println!("(would be written to {})", path.display());
        return Ok(());
    }
    let cfg = Config::load_file_or_default()?;
    println!("config file : {}", path.display());
    println!("api_key     : {}", mask(&cfg.api_key));
    println!("base_url    : {}", cfg.base_url);
    println!("model       : {}", cfg.model);
    Ok(())
}

/// Interactive setup: prompt for key/URL, then pick a model from the live list.
pub async fn run_wizard() -> Result<()> {
    let current = Config::load_file_or_default()?;

    println!("egg-agent configuration");
    println!("Press Enter to keep the current value shown in [brackets].\n");

    let api_key = prompt_secret_default("API key", &current.api_key)?
        .trim()
        .to_string();
    let base_url = prompt_default("Base URL (OpenAI-compatible)", &current.base_url)?
        .trim()
        .trim_end_matches('/')
        .to_string();

    if api_key.is_empty() {
        // Without a key we can't fetch models; fall back to manual entry.
        let model = prompt_default("Model", &current.model)?.trim().to_string();
        let config = Config {
            api_key,
            base_url,
            model,
            aux: None,
        };
        println!(
            "\nWarning: no API key set — `egg` won't be able to call the model until you add one."
        );
        let path = config.save()?;
        println!("Saved to {}", path.display());
        return Ok(());
    }

    // With a key + URL, offer live model selection (falls back to manual on failure).
    let model = select_model(&base_url, &api_key, &current.model).await?;

    let config = Config {
        api_key,
        base_url,
        model,
        aux: None,
    };
    let path = config.save()?;
    println!("\nSaved to {}", path.display());
    Ok(())
}

/// `egg model`: keep key/URL, fetch models, switch the active one.
pub async fn run_model_switch() -> Result<()> {
    let mut config = Config::load_file_or_default()?;

    if config.api_key.trim().is_empty() {
        println!("No API key configured yet. Run `egg config` first.");
        return Ok(());
    }

    let chosen = select_model(&config.base_url, &config.api_key, &config.model).await?;
    if chosen == config.model {
        println!("Model unchanged ({}).", config.model);
        return Ok(());
    }
    config.model = chosen;
    let path = config.save()?;
    println!("Switched model to {}. Saved to {}", config.model, path.display());
    Ok(())
}

/// Fetch the model list and let the user pick with arrow keys + type-to-filter.
///
/// On any fetch error (network, auth, non-OpenAI endpoint), falls back to a
/// plain text prompt so configuration never gets stuck.
async fn select_model(base_url: &str, api_key: &str, current: &str) -> Result<String> {
    println!("Fetching available models from {base_url} ...");
    let models = match list_models(base_url, api_key).await {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => {
            println!("The endpoint returned no models; enter one manually.");
            return prompt_default("Model", current).map(|s| s.trim().to_string());
        }
        Err(e) => {
            println!("Could not fetch models ({e:#}).");
            return prompt_default("Model", current).map(|s| s.trim().to_string());
        }
    };

    // Put the current model first so it's the default highlight, if present.
    let start = models.iter().position(|m| m == current).unwrap_or(0);

    let selection = Select::new("Select a model (type to filter):", models)
        .with_starting_cursor(start)
        .with_help_message("↑↓ move · type to filter · Enter to select · Esc to keep current")
        .prompt();

    match selection {
        Ok(model) => Ok(model),
        Err(inquire::InquireError::OperationCanceled)
        | Err(inquire::InquireError::OperationInterrupted) => {
            // Esc / Ctrl-C: keep whatever they had.
            println!("Keeping current model: {current}");
            Ok(current.to_string())
        }
        Err(inquire::InquireError::NotTTY) => {
            // Non-interactive (piped) environment: fall back to manual entry.
            prompt_default("Model", current).map(|s| s.trim().to_string())
        }
        Err(e) => Err(e).context("model selection failed"),
    }
}

/// Prompt with a default; empty input keeps the default.
fn prompt_default(label: &str, current: &str) -> Result<String> {
    if current.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{current}]: ");
    }
    io::stdout().flush().ok();

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("failed to read input")?;
    let trimmed = line.trim_end_matches(['\n', '\r']).to_string();
    Ok(if trimmed.is_empty() {
        current.to_string()
    } else {
        trimmed
    })
}

/// Like `prompt_default`, but shows the current value masked (for secrets).
fn prompt_secret_default(label: &str, current: &str) -> Result<String> {
    if current.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{}]: ", mask(current));
    }
    io::stdout().flush().ok();

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("failed to read input")?;
    let trimmed = line.trim_end_matches(['\n', '\r']).to_string();
    Ok(if trimmed.is_empty() {
        current.to_string()
    } else {
        trimmed
    })
}

/// Mask a secret so `show` and prompts don't leak the whole key.
fn mask(secret: &str) -> String {
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
