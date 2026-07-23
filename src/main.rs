use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
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
    env_logger::Builder::from_env("EGG_LOG")
        .format_timestamp_millis()
        .target(env_logger::Target::Stderr)
        .init();
    match cli::parse_args() {
        Command::Run => run_tui().await,
        Command::Resume(id) => run_tui_resume(id).await,
        Command::Config => cli::run_wizard().await,
        Command::Model => cli::run_model_switch().await,
        Command::ConfigPath => cli::print_config_path(),
        Command::ConfigShow => cli::print_config_show(),
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
    let provider = provider_label(&config.base_url);
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
                        } else {
                            app.clear_input();
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
                        // Shift+Enter or Alt+Enter (Option+Enter on macOS) inserts
                        // a newline. Shift works in iTerm2/Ghostty/kitty; Alt works
                        // in the default macOS Terminal.app and everywhere else.
                        let mods = key.modifiers;
                        let newline = (mods.contains(KeyModifiers::SHIFT)
                            || mods.contains(KeyModifiers::ALT))
                            && !app.running;
                        if newline {
                            app.input_newline();
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
                        app.history_up();
                    }
                    KeyCode::Down if !app.running => {
                        app.history_down();
                    }
                    KeyCode::Char(c) if !app.running => app.input.push(c),
                    KeyCode::Backspace if !app.running => {
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
                            handle_overlay_action(action, app, &client, &ctl_tx, plugins);
                        } else {
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
            let base = client.base_url().to_string();
            let key = client.api_key().to_string();
            let tx = ctl_tx.clone();
            tokio::spawn(async move {
                let result = egg_agent::llm::openai::list_models(&base, &key)
                    .await
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(CtlEvent::ModelList(result));
            });
        }
        OverlayAction::ApplyModel(model) => {
            // Switch the live client, update the UI, and persist to config.
            client.set_model(&model);
            app.apply_chosen_model(model.clone());
            if let Ok(mut cfg) = Config::load_file_or_default() {
                cfg.model = model;
                let _ = cfg.save();
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

/// Derive a short provider name from the base URL host, for the status line.
fn provider_label(base_url: &str) -> String {
    let host = base_url
        .split("://")
        .nth(1)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or("")
        .trim_start_matches("api.");
    // Take the second-to-last label ("novita" from "novita.ai"), else the host.
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
