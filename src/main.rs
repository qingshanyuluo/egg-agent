use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use egg_agent::agent::{AgentEvent, run_agent};
use egg_agent::app::{App, OverlayAction, OverlayKey};
use egg_agent::cli::{self, Command};
use egg_agent::config::Config;
use egg_agent::llm::{ChatMessage, LlmClient};
use egg_agent::llm::openai::OpenAiClient;
use egg_agent::plugin::{self, PluginEvent};
use egg_agent::session;
use egg_agent::tools::ToolRegistry;
use egg_agent::types::{Message, Role};
use egg_agent::ui;

/// Restore the terminal to its pre-TUI state. Best-effort: every step ignores
/// errors so a partial failure still runs the remaining cleanup. Safe to call
/// more than once.
fn restore_terminal() {
    use std::io::Write;
    let mut out = io::stdout();
    let _ = disable_raw_mode();
    let _ = execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
    let _ = out.flush();
}

/// RAII guard that owns the terminal's raw/alternate-screen state.
///
/// Entering the alternate screen on construction and leaving it on `Drop`
/// guarantees the terminal is restored on EVERY exit path — normal return, an
/// early `?` error, or a panic unwinding through the stack — so the user's
/// shell is never left in a corrupted (raw, alt-screen, hidden-cursor) state.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            Hide
        )?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
        let _ = self.terminal.show_cursor();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Persistent log file so we can diagnose hangs after the fact.
    let log_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".egg-agent");
    std::fs::create_dir_all(&log_dir).ok();
    let log_file = std::fs::File::create(log_dir.join("egg.log"))
        .context("cannot create egg.log")?;
    let mut log_builder = env_logger::Builder::from_env("EGG_LOG");
    if std::env::var("EGG_LOG").is_err() {
        log_builder.filter_level(log::LevelFilter::Debug); // capture everything by default
    }
    log_builder
        .format_timestamp_millis()
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .init();
    match cli::parse_args() {
        Command::Run => run_tui().await,
        Command::Resume(id) => run_tui_resume(id).await,
        Command::Help => {
            cli::print_help();
            Ok(())
        }
        Command::Version => {
            cli::print_version();
            Ok(())
        }
    }
}

async fn run_tui_resume(id: Option<String>) -> Result<()> {
    let sessions = session::list().unwrap_or_default();
    if sessions.is_empty() {
        println!("No saved sessions found.");
        println!("Run `egg` to start a new session.");
        return Ok(());
    }

    // If an id was specified, find the matching session.
    let target = if let Some(ref id) = id {
        sessions
            .iter()
            .find(|s| s.id == *id || s.path.to_string_lossy().contains(id.as_str()))
    } else {
        None
    };

    let info = match target {
        Some(s) => s.clone(),
        None => {
            // Interactive picker.
            println!("Saved sessions:\n");
            for (i, s) in sessions.iter().enumerate() {
                let preview = if s.preview.is_empty() {
                    "(empty)"
                } else {
                    &s.preview
                };
                println!("  [{}] {}  {}", i, s.timestamp, preview);
            }
            println!();
            print!("Pick a session (0-{}) or Enter for latest: ", sessions.len().saturating_sub(1));
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let input = input.trim();
            if input.is_empty() {
                sessions[0].clone()
            } else {
                match input.parse::<usize>() {
                    Ok(n) if n < sessions.len() => sessions[n].clone(),
                    _ => {
                        println!("Invalid choice.");
                        return Ok(());
                    }
                }
            }
        }
    };

    match session::load(&info.path) {
        Ok(history) => run_tui_with(Some(history)).await,
        Err(e) => {
            eprintln!("Failed to load session: {e}");
            Ok(())
        }
    }
}

async fn run_tui() -> Result<()> {
    run_tui_with(None).await
}

async fn run_tui_with(pending_history: Option<Vec<ChatMessage>>) -> Result<()> {
    // Build the backend before touching the terminal, so config errors print cleanly.
    let config = Config::load()?;
    let provider = cli::provider_label(&config.base_url);
    let model = config.model.clone();
    // Keep a concrete handle so `/model` can switch the live model and we can
    // fetch the model list; the agent uses the same instance via the trait object.
    let client = Arc::new(OpenAiClient::new(&config));
    let llm: Arc<dyn LlmClient> = client.clone();
    let tools = Arc::new(ToolRegistry::default_set());

    // Optional aux client for plugins (translation, etc.).
    let aux_client: Option<Arc<dyn LlmClient>> = config
        .aux
        .as_ref()
        .filter(|a| !a.model.trim().is_empty())
        .map(|aux| {
            let base = aux.base_url.as_deref().unwrap_or(&config.base_url);
            let key = aux.api_key.as_deref().unwrap_or(&config.api_key);
            log::info!("aux model configured: {} @ {base}", aux.model);
            Arc::new(OpenAiClient::with_params(base, key, &aux.model)) as Arc<dyn LlmClient>
        });
    if aux_client.is_none() {
        log::info!("no aux model configured — translation/explain plugins will be silent");
    }

    let plugins = plugin::Registry::builtin();
    let (plugin_tx, mut plugin_rx) = mpsc::unbounded_channel::<PluginEvent>();

    // On panic, restore the terminal first so the backtrace is readable and the
    // shell isn't left corrupted, then run the default hook to print the panic.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    // The guard restores the terminal on every exit path via its Drop impl.
    let mut guard = TerminalGuard::new()?;

    let mut app = App::new(model, provider);
    app.plugin_commands = plugins.all_commands();
    app.refresh_providers(&config);
    if let Some(history) = pending_history {
        app.apply_resume(history);
    }
    let result = run(
        &mut guard.terminal,
        &mut app,
        llm,
        client,
        tools,
        &plugins,
        &aux_client,
        plugin_tx,
        &mut plugin_rx,
    )
    .await;

    // Save session while we still have app state, then restore terminal.
    let session_id = if app.history.len() > 1 {
        session::save(&app.history).ok().and_then(|p| {
            // Extract short id from filename: "session-2026-07-23T15-30-00.json" → "20260723-1530"
            p.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_prefix("session-"))
                .map(|s| {
                    s.replace('-', "")
                        .replace('T', "-")
                        .chars()
                        .take(13)
                        .collect::<String>()
                })
        })
    } else {
        None
    };
    drop(guard); // restore terminal
    if let Some(id) = session_id {
        println!("Resume:  egg --resume {id}");
    }
    result
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    llm: Arc<dyn LlmClient>,
    client: Arc<OpenAiClient>,
    tools: Arc<ToolRegistry>,
    plugins: &plugin::Registry,
    aux_client: &Option<Arc<dyn LlmClient>>,
    plugin_tx: mpsc::UnboundedSender<PluginEvent>,
    plugin_rx: &mut mpsc::UnboundedReceiver<PluginEvent>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    // Separate channel for slash-command results (e.g. the fetched model list).
    let (ctl_tx, mut ctl_rx) = mpsc::unbounded_channel::<CtlEvent>();

    // Restore the terminal if we're killed by a signal mid-render (the main loop
    // blocks in event::poll and can't react in time). Runs on a tokio worker
    // thread, so it's scheduled even while this task is blocked.
    spawn_signal_watcher();

    // Redraw only when something actually changed, so the loop doesn't spin the
    // CPU (and flood the terminal) by redrawing on every idle poll timeout.
    let mut dirty = true;

    // Track the last Esc press time to detect Alt+Enter / Option+Enter (Esc
    // followed quickly by Enter) for inserting newlines without the kitty
    // keyboard protocol (which interferes with bracketed paste).
    let mut last_esc: Option<Instant> = None;

    while !app.should_quit {
        if dirty {
            terminal.draw(|frame| ui::draw(frame, app))?;
            dirty = false;
        }

        // Drain any events the background agent has produced.
        while let Ok(event) = rx.try_recv() {
            log::debug!("agent event: {event:?}");
            app.apply_event(event.clone());
            // Let plugins react to the applied state change.
            plugins.on_agent_event(&event, app, aux_client.as_ref(), &plugin_tx);
            dirty = true;
        }
        // Drain control events (model list arriving, etc.).
        while let Ok(event) = ctl_rx.try_recv() {
            match event {
                CtlEvent::ModelList(result) => app.set_model_list(result),
            }
            dirty = true;
        }
        // Drain plugin events (translation results, bash explanations, etc.).
        while let Ok(pe) = plugin_rx.try_recv() {
            match pe {
                PluginEvent::Custom {
                    msg_idx,
                    field,
                    ref text,
                } => {
                    log::debug!("plugin event: msg_idx={msg_idx} field={field} len={}", text.len());
                    if let Some(msg) = app.messages.get_mut(msg_idx) {
                        match field {
                            "translation" => msg.translation = Some(text.clone()),
                            "explanation" => msg.explanation = Some(text.clone()),
                            other => log::warn!("unknown plugin message field: {other}"),
                        }
                    } else {
                        log::warn!("plugin event for unknown msg_idx={msg_idx}");
                    }
                }
                PluginEvent::Redraw => {}
            }
            dirty = true;
        }

        // Tick fast while a turn runs (spinner/stream), an overlay is animating
        // (the model picker's loading spinner), or a toast is fading; otherwise
        // wait longer and stay quiet.
        let animating = app.running || app.overlay_active() || app.toast_active();
        let poll_ms = if animating { 80 } else { 200 };

        if !event::poll(Duration::from_millis(poll_ms))? {
            if animating {
                dirty = true;
            }
            // If Esc was pressed but no key followed in time, clear input.
            if let Some(t) = last_esc {
                if t.elapsed() >= Duration::from_millis(200) {
                    app.clear_input();
                    last_esc = None;
                    dirty = true;
                }
            }
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                dirty = true;

                // While an overlay is open, it captures all keys.
                if app.overlay_active() {
                    if let Some(ok) = to_overlay_key(key.code) {
                        let action = app.overlay_key(ok);
                        handle_overlay_action(action, app, &client, &ctl_tx, plugins);
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Esc => {
                        if app.running {
                            app.cancel_session();
                        } else if app.overlay_active() {
                            // Esc in overlay: let the overlay handle it via the
                            // overlay key path below.
                        } else {
                            // Record the time so we can detect Alt+Enter (Esc
                            // followed quickly by Enter) for inserting newlines.
                            last_esc = Some(Instant::now());
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.handle_ctrl_c() {
                            app.quit();
                        }
                    }
                    // Typing "/" on an empty input opens the command palette.
                    KeyCode::Char('/') if app.input.is_empty() && !app.running => {
                        app.open_command_menu();
                    }
                    KeyCode::Enter => {
                        // Detect Alt+Enter / Option+Enter: Esc followed quickly
                        // by Enter inserts a newline. Shift+Enter still works
                        // natively in most terminals without the kitty protocol.
                        let esc_newline = last_esc
                            .map(|t| t.elapsed() < Duration::from_millis(200))
                            .unwrap_or(false);
                        last_esc = None;

                        let shift_newline =
                            key.modifiers.contains(KeyModifiers::SHIFT) && !app.running;

                        if (esc_newline || shift_newline) && !app.running && !app.overlay_active() {
                            app.input_newline();
                        } else if app.input.trim().starts_with('/') && !app.running {
                            // Slash command: handle it directly.
                            let input = app.input.clone();
                            app.input.clear();

                            // /model: open the model picker and start fetching.
                            if input.trim() == "/model" {
                                app.overlay = Some(egg_agent::app::Overlay::ModelPicker(
                                    egg_agent::app::ModelPicker::Loading,
                                ));
                                let tx = ctl_tx.clone();
                                let providers = app.all_providers.clone();
                                tokio::spawn(async move {
                                    let result = fetch_all_models(&providers).await;
                                    let _ = tx.send(CtlEvent::ModelList(result));
                                });
                            } else if let Some(msg) = cli::handle_slash_command(&input, app) {
                                app.messages.push(
                                    egg_agent::app::Message::new(
                                        egg_agent::app::Role::System, msg,
                                    ),
                                );
                            }
                        } else if let Some(history) = app.take_submission() {
                            let llm = Arc::clone(&llm);
                            let tools = Arc::clone(&tools);
                            let tx = tx.clone();
                            let handle = tokio::spawn(async move {
                                run_agent(history, llm, tools, tx).await;
                            });
                            app.abort_handle = Some(handle.abort_handle());
                        }
                    }
                    // Input history: Up/Down to recall previous submissions.
                    KeyCode::Up if !app.running => {
                        last_esc = None;
                        app.history_up();
                    }
                    KeyCode::Down if !app.running => {
                        last_esc = None;
                        app.history_down();
                    }
                    KeyCode::Char(c) if !app.running => {
                        // If Esc was just pressed (without Enter), clear input first.
                        if last_esc.take().is_some() {
                            app.clear_input();
                        }
                        app.input.push(c)
                    }
                    KeyCode::Backspace if !app.running => {
                        if last_esc.take().is_some() {
                            app.clear_input();
                        }
                        app.input.pop();
                    }
                    _ => {}
                }
            }
            Event::Mouse(m) => {
                dirty = true;
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if app.overlay_active() {
                            let action = app.overlay_click(m.row);
                            handle_overlay_action(action, app, &client, &ctl_tx, plugins);                        } else {
                            log::debug!("mouse down row={}", m.row);
                            plugins.on_mouse_down(m.row, app);
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if !app.overlay_active() {
                            plugins.on_mouse_drag(m.row, app);
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if !app.overlay_active() {
                            log::debug!("mouse up row={} hitboxes={:?}", m.row, app.thought_hitboxes.borrow());
                            plugins.on_mouse_up(m.row, app);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        app.scroll_down();
                    }
                    MouseEventKind::ScrollUp => {
                        app.scroll_up();
                    }
                    _ => {}
                }
            }
            Event::Paste(pasted) => {
                // Bracketed paste: insert as-is (newlines kept), never auto-send.
                app.paste(&pasted);
                dirty = true;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Control-channel events for slash-command async results.
enum CtlEvent {
    ModelList(Result<Vec<String>, String>),
}

/// Map a crossterm key into the overlay's normalized key, if relevant.
fn to_overlay_key(code: KeyCode) -> Option<OverlayKey> {
    match code {
        KeyCode::Esc => Some(OverlayKey::Esc),
        KeyCode::Enter => Some(OverlayKey::Enter),
        KeyCode::Up => Some(OverlayKey::Up),
        KeyCode::Down => Some(OverlayKey::Down),
        KeyCode::Backspace => Some(OverlayKey::Backspace),
        KeyCode::Tab => Some(OverlayKey::Char('\t')),
        KeyCode::Char(c) => Some(OverlayKey::Char(c)),
        _ => None,
    }
}

/// Perform the side effect an overlay interaction requested.
fn handle_overlay_action(
    action: OverlayAction,
    app: &mut App,
    client: &Arc<OpenAiClient>,
    ctl_tx: &mpsc::UnboundedSender<CtlEvent>,
    plugins: &plugin::Registry,
) {
    match action {
        OverlayAction::None => {}
        OverlayAction::FetchModels => {
            let tx = ctl_tx.clone();
            let providers = app.all_providers.clone();
            tokio::spawn(async move {
                let result = fetch_all_models(&providers).await;
                let _ = tx.send(CtlEvent::ModelList(result));
            });
        }
        OverlayAction::ApplyModel(model) => {
            // The model string may be "model_name (provider_name)".
            let (model_name, provider_name) = parse_model_choice(&model);
            client.set_model(model_name);
            app.apply_chosen_model(model_name.to_string());

            // If a named provider was selected, switch credentials.
            if let Some(prov) = provider_name {
                // Match by display label: for "default" the label comes from
                // provider_label(url); for named providers it's the name itself.
                let found = app.all_providers.iter().find(|(n, _k, url)| {
                    if *n == "default" {
                        cli::provider_label(url) == prov
                    } else {
                        n == prov
                    }
                });
                if let Some((_name, _key, url)) = found {
                    client.set_provider(url, _key);
                    app.provider = prov.to_string();
                }
            }

            if let Ok(mut cfg) = Config::load_file_or_default() {
                cfg.model = model_name.to_string();
                let _ = cfg.save();
            }
        }
        OverlayAction::ConnectProvider {
            name,
            api_key,
            base_url,
        } => {
            let result = cli::handle_slash_command(
                &format!("/connect {name} {api_key} {base_url}"),
                app,
            );
            if let Some(msg) = result {
                app.messages
                    .push(Message::new(Role::System, msg));
            }
        }
        OverlayAction::PluginCommand(name) => {
            log::debug!("plugin command: {name}");
            if plugins.dispatch_command(&name, app) {
                app.overlay = None;
            }
        }
    }
}

/// Fetch models from all configured providers, returning a combined list
/// where each model is labeled with its provider: "model_name (provider)".
async fn fetch_all_models(providers: &[(String, String, String)]) -> Result<Vec<String>, String> {
    let mut all: Vec<String> = Vec::new();
    let multi = providers.len() > 1;
    for (name, key, url) in providers {
        let label = if name == "default" {
            cli::provider_label(url)
        } else {
            name.clone()
        };
        match egg_agent::llm::openai::list_models(url, key).await {
            Ok(models) => {
                for m in models {
                    if multi {
                        all.push(format!("{m} ({label})"));
                    } else {
                        all.push(m);
                    }
                }
            }
            Err(e) => {
                log::warn!("failed to fetch models from {label}: {e:#}");
            }
        }
    }
    if all.is_empty() {
        Err("no models found from any provider".to_string())
    } else {
        Ok(all)
    }
}

/// Parse a "model_name (provider_name)" string back to its parts.
fn parse_model_choice(choice: &str) -> (&str, Option<&str>) {
    if let Some(rest) = choice.strip_suffix(')') {
        if let Some((model, provider)) = rest.split_once(" (") {
            return (model, Some(provider));
        }
    }
    (choice, None)
}

/// Watch for termination signals and restore the terminal before the process
/// dies. A SIGTERM (or SIGINT delivered as a real signal) would otherwise kill
/// us mid-alternate-screen — and the main loop can't react in time because it's
/// blocked in crossterm's `event::poll`. So the handler restores the terminal
/// itself and exits, rather than signalling the loop.
fn spawn_signal_watcher() {
    #[cfg(unix)]
    {
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut int = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(_) => return,
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            restore_terminal();
            // 130 = terminated by SIGINT, the conventional shell exit code.
            std::process::exit(130);
        });
    }
    #[cfg(not(unix))]
    {
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                restore_terminal();
                std::process::exit(130);
            }
        });
    }
}
