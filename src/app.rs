//! Application state and the agent-event reducer.
//!
//! `App` owns every piece of mutable UI state: the transcript, input box,
//! scroll position, overlays, clipboard selection, and the handle that lets
//! the user cancel a running agent turn.
//!
//! Domain types (Message, Role, Overlay, …) live in [`crate::types`]; pure
//! helper functions live in [`crate::gfx`]. Both are re-exported from here so
//! the rest of the crate can keep a single `use crate::app::*` import.

use std::time::Instant;

use crate::agent::AgentEvent;
use crate::gfx;
use crate::llm::ChatMessage;

// ---- Re-export domain types for zero-diff downstream imports ----
pub use crate::types::{
    COMMANDS, ConnectWizard, Message, ModelPicker, Overlay, OverlayAction, OverlayKey, Role,
    SlashCommand,
};

pub struct App {
    pub input: String,
    /// What the user sees in the transcript.
    pub messages: Vec<Message>,
    /// Full LLM-format conversation history, carried across turns for context.
    pub history: Vec<ChatMessage>,
    /// Show the splash card: starts true, cleared when a conversation exists.
    pub show_splash: bool,
    /// Whether the splash is visible at this very moment (drives the color
    /// animation tick). Set by the UI each frame, read by the event loop.
    pub splash_visible: std::cell::Cell<bool>,
    /// Instant the app booted; drives the splash color animation.
    pub booted_at: Instant,
    /// True while a background agent turn is in flight.
    pub running: bool,
    /// True after a turn starts but before its first token — drives the spinner.
    pub waiting_first_token: bool,
    /// When the current wait began (for the spinner frame + elapsed display).
    pub wait_started: Option<Instant>,
    /// When reasoning started this turn (to compute "thought for Ns").
    reasoning_started: Option<Instant>,
    /// Index into `messages` of the assistant message the current turn streams
    /// into, if one exists yet.
    streaming_idx: Option<usize>,
    pub should_quit: bool,
    /// Model id shown in the status line.
    pub model: String,
    /// Provider label inferred from the base URL, shown in the status line.
    pub provider: String,
    /// Screen rows (y) that show a collapsible "thought" line, mapped to the
    /// message index they belong to. Rebuilt by the UI on every frame; consulted
    /// when a mouse click arrives. Uses interior-mutability-free RefCell so the
    /// render (which takes `&App`) can record hit regions.
    pub thought_hitboxes: std::cell::RefCell<Vec<(u16, usize)>>,
    /// Screen rows (y) of collapsible tool-output header lines → message index.
    /// Rebuilt by the UI each frame, just like `thought_hitboxes`.
    pub tool_hitboxes: std::cell::RefCell<Vec<(u16, usize)>>,
    /// Total rendered height of the transcript in screen rows, including the
    /// off-screen portion, accounting for line wrapping at the current width.
    /// Rebuilt by the UI each frame; used to clamp `scroll_back` so the view
    /// can't scroll past the first line.
    pub total_rows: std::cell::Cell<usize>,
    /// Height (rows) of the transcript viewport in the last rendered frame.
    pub view_height: std::cell::Cell<usize>,
    /// Active modal overlay (command palette or model picker), if any.
    pub overlay: Option<Overlay>,
    /// Screen rows (y) of clickable overlay items -> item index. Rebuilt each
    /// frame by the UI, consulted on click.
    pub overlay_hitboxes: std::cell::RefCell<Vec<(u16, usize)>>,
    /// The plain text rendered on each transcript screen row, keyed by absolute
    /// screen y. Rebuilt by the UI each frame; used to build the clipboard string
    /// when the user drag-selects transcript rows.
    pub row_text: std::cell::RefCell<std::collections::HashMap<u16, String>>,
    /// Active drag selection over transcript rows: (anchor_y, cursor_y). Present
    /// only while dragging or after a completed selection (until cleared).
    pub selection: Option<(u16, u16)>,
    /// When a "copied" toast was triggered; drives the fade-out.
    pub copied_at: Option<Instant>,
    /// Slash commands contributed by plugins, refreshed at startup.
    pub plugin_commands: Vec<SlashCommand>,
    /// Manual scroll offset from bottom: 0 = auto-follow, >0 = scrolled up.
    /// A `Cell` so the UI can re-clamp it against the freshly measured layout
    /// each frame (via `clamp_scroll`) without needing `&mut App`.
    pub scroll_back: std::cell::Cell<usize>,
    /// Handle to abort a running agent task (Esc during a turn).
    pub abort_handle: Option<tokio::task::AbortHandle>,
    /// When the last Ctrl+C was pressed (for two-stage quit).
    last_ctrl_c: Option<Instant>,
    /// History of submitted inputs for Up/Down navigation.
    input_history: Vec<String>,
    /// Current position in history navigation (None = editing a fresh input).
    history_cursor: Option<usize>,
    /// The input being composed before the user navigated into history;
    /// restored when they scroll back past the newest entry.
    draft_input: String,
    /// All configured providers: (name, api_key, base_url). Refreshed after
    /// /connect or /connect-remove so the model picker stays in sync.
    pub all_providers: Vec<(String, String, String)>,
    /// The `@`-file completion popup, when open (feature D). Kept separate from
    /// `overlay` so it doesn't disturb the modal-overlay input/hitbox contract:
    /// it's an inline typeahead over the input box, not a modal.
    pub file_popup: Option<FilePopup>,
}

/// State for the `@`-file completion popup. `query` is the text typed after the
/// `@` that opened it; `matches` is the current ranked candidate list (filled
/// asynchronously off the UI thread); `selected` indexes into `matches`.
#[derive(Debug, Clone, Default)]
pub struct FilePopup {
    /// Byte offset of the `@` in `app.input` that opened this popup. Used to
    /// splice the chosen path back in and to know how much to replace.
    pub at: usize,
    /// The query typed after `@` (does not include the `@` itself).
    pub query: String,
    /// Ranked candidate paths, newest search wins.
    pub matches: Vec<String>,
    /// Index of the highlighted candidate within `matches`.
    pub selected: usize,
}

impl App {
    pub fn new(model: String, provider: String) -> Self {
        Self {
            input: String::new(),
            messages: Vec::new(),
            history: Vec::new(),
            show_splash: true,
            splash_visible: std::cell::Cell::new(false),
            booted_at: Instant::now(),
            running: false,
            waiting_first_token: false,
            wait_started: None,
            reasoning_started: None,
            streaming_idx: None,
            should_quit: false,
            model,
            provider,
            thought_hitboxes: std::cell::RefCell::new(Vec::new()),
            tool_hitboxes: std::cell::RefCell::new(Vec::new()),
            total_rows: std::cell::Cell::new(0),
            view_height: std::cell::Cell::new(0),
            overlay: None,
            overlay_hitboxes: std::cell::RefCell::new(Vec::new()),
            row_text: std::cell::RefCell::new(std::collections::HashMap::new()),
            selection: None,
            copied_at: None,
            plugin_commands: Vec::new(),
            scroll_back: std::cell::Cell::new(0),
            abort_handle: None,
            last_ctrl_c: None,
            input_history: Vec::new(),
            history_cursor: None,
            draft_input: String::new(),
            all_providers: Vec::new(),
            file_popup: None,
        }
    }

    // ---- Submit & agent events ----

    /// Take the current input as a submitted user message, if non-empty and idle.
    /// Returns the full history (including the new user turn) to hand to the agent.
    pub fn take_submission(&mut self) -> Option<Vec<ChatMessage>> {
        if self.running {
            return None;
        }
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }

        self.messages.push(Message::new(Role::User, text.clone()));
        self.history.push(ChatMessage::user(text.clone()));
        self.input_history.push(text);
        self.history_cursor = None;
        self.draft_input.clear();
        self.input.clear();
        self.show_splash = false;
        self.running = true;

        Some(self.history.clone())
    }

    /// Apply one event streamed from the background agent task.
    pub fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart => {
                self.waiting_first_token = true;
                self.wait_started = Some(Instant::now());
                self.reasoning_started = None;
                self.streaming_idx = None;
                self.scroll_back.set(0);
                // Auto-collapse all tool outputs from previous turns (like reasoning).
                for msg in &mut self.messages {
                    if msg.role == Role::ToolOutput && msg.full_content.is_some() {
                        msg.output_collapsed = true;
                    }
                    if msg.role == Role::Tool {
                        msg.tool_collapsed = true;
                    }
                }
            }
            AgentEvent::ReasoningDelta(delta) => {
                self.first_token_arrived();
                if self.reasoning_started.is_none() {
                    self.reasoning_started = Some(Instant::now());
                }
                let idx = self.ensure_stream_message();
                self.messages[idx].reasoning.push_str(&delta);
            }
            AgentEvent::ContentDelta(delta) => {
                self.first_token_arrived();
                let idx = self.ensure_stream_message();
                if self.messages[idx].content.is_empty()
                    && !self.messages[idx].reasoning.is_empty()
                {
                    self.messages[idx].reasoning_collapsed = true;
                    self.messages[idx].reasoning_secs =
                        Some(self.reasoning_started.map_or(0, |t| t.elapsed().as_secs()));
                }
                self.messages[idx].content.push_str(&delta);
            }
            AgentEvent::ToolCall { name, args } => {
                if let Some(i) = self.streaming_idx {
                    if !self.messages[i].reasoning.is_empty() {
                        self.messages[i].reasoning_collapsed = true;
                        self.messages[i].reasoning_secs =
                            Some(self.reasoning_started.map_or(0, |t| t.elapsed().as_secs()));
                    }
                }
                self.finish_stream_message();
                let mut m = Message::new(Role::Tool, gfx::tool_call_label(&name, &args));
                m.args = Some(args);
                self.messages.push(m);
            }
            AgentEvent::ToolResult { output } => {
                let trimmed = output.trim_end().to_string();
                let mut msg = Message::new(Role::ToolOutput, gfx::first_lines(&trimmed, 12));
                // Always keep full content so every output is collapsible.
                msg.full_content = Some(trimmed);
                msg.output_collapsed = false; // show expanded initially, auto-collapse next turn
                self.messages.push(msg);
            }
            AgentEvent::Error {
                message,
                partial_history,
            } => {
                // Save partial progress to history so resume doesn't lose it.
                if let Some(h) = partial_history {
                    // Agent accumulated a full history (e.g. max-iterations).
                    self.history = h;
                } else if let Some(i) = self.streaming_idx {
                    // Stream-level error: save whatever content was shown in the UI.
                    let msg = &self.messages[i];
                    if !msg.content.is_empty() || !msg.reasoning.is_empty() {
                        let mut partial = ChatMessage::text("assistant", &msg.content);
                        if !msg.reasoning.is_empty() {
                            partial.reasoning_content = Some(msg.reasoning.clone());
                        }
                        self.history.push(partial);
                    }
                }
                self.finish_stream_message();
                self.messages
                    .push(Message::new(Role::System, format!("error: {message}")));
                self.reset_run_state();
            }
            AgentEvent::Done(history) => {
                self.finish_stream_message();
                self.history = history;
                self.reset_run_state();
            }
        }
    }

    // ---- Streaming helpers ----

    pub fn spinner_frame(&self) -> usize {
        self.wait_started
            .map(|t| (t.elapsed().as_millis() / 80) as usize)
            .unwrap_or(0)
    }

    /// The message index the current turn is streaming into, or the index
    /// where the next message will be created. Used by plugins.
    pub fn streaming_idx(&self) -> usize {
        self.streaming_idx.unwrap_or(self.messages.len())
    }

    fn first_token_arrived(&mut self) {
        self.waiting_first_token = false;
    }

    fn ensure_stream_message(&mut self) -> usize {
        if let Some(i) = self.streaming_idx {
            return i;
        }
        self.messages.push(Message::new(Role::Assistant, ""));
        let i = self.messages.len() - 1;
        self.streaming_idx = Some(i);
        i
    }

    fn finish_stream_message(&mut self) {
        if let Some(i) = self.streaming_idx {
            if self.messages[i].content.is_empty() && self.messages[i].reasoning.is_empty() {
                self.messages.remove(i);
            }
        }
        self.streaming_idx = None;
    }

    fn reset_run_state(&mut self) {
        self.running = false;
        self.waiting_first_token = false;
        self.wait_started = None;
        self.reasoning_started = None;
        self.streaming_idx = None;
    }

    // ---- Input & quit ----

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.history_cursor = None;
        self.draft_input.clear();
    }

    /// Replace the input buffer wholesale (e.g. after an external-editor edit),
    /// resetting history navigation so Up/Down start fresh from this text.
    pub fn set_input(&mut self, text: String) {
        self.input = text;
        self.history_cursor = None;
    }

    // ---- `@`-file completion popup (feature D) ----

    /// True while the file-completion popup is open (captures ↑↓/Enter/Tab/Esc).
    pub fn file_popup_active(&self) -> bool {
        self.file_popup.is_some()
    }

    /// Open the popup, recording the byte offset of the `@` that triggers it
    /// (the `@` is assumed to be the last char pushed to `input`).
    pub fn open_file_popup(&mut self) {
        let at = self.input.len().saturating_sub('@'.len_utf8());
        self.file_popup = Some(FilePopup {
            at,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
        });
    }

    /// The current query text after `@`, if the popup is open.
    pub fn file_popup_query(&self) -> Option<String> {
        self.file_popup.as_ref().map(|p| p.query.clone())
    }

    /// Extend the query by one char (a keystroke while the popup is open) and
    /// mirror it into the input buffer so the `@query` stays visible.
    pub fn file_popup_push(&mut self, c: char) {
        if let Some(p) = self.file_popup.as_mut() {
            p.query.push(c);
            p.selected = 0;
        }
        self.input.push(c);
    }

    /// Backspace within the query. Returns `false` (and closes the popup) when
    /// the `@` itself is deleted, so the caller can treat it as a normal edit.
    pub fn file_popup_backspace(&mut self) -> bool {
        let close = match self.file_popup.as_mut() {
            Some(p) if p.query.is_empty() => true, // backspacing the `@`
            Some(p) => {
                p.query.pop();
                p.selected = 0;
                false
            }
            None => return false,
        };
        self.input.pop();
        if close {
            self.file_popup = None;
        }
        true
    }

    /// Replace the popup's candidate list (from an async search result). Ignored
    /// if the popup was closed meanwhile.
    pub fn set_file_matches(&mut self, matches: Vec<String>) {
        if let Some(p) = self.file_popup.as_mut() {
            p.selected = p.selected.min(matches.len().saturating_sub(1));
            p.matches = matches;
        }
    }

    pub fn file_popup_up(&mut self) {
        if let Some(p) = self.file_popup.as_mut() {
            p.selected = p.selected.saturating_sub(1);
        }
    }

    pub fn file_popup_down(&mut self) {
        if let Some(p) = self.file_popup.as_mut()
            && !p.matches.is_empty()
        {
            p.selected = (p.selected + 1).min(p.matches.len() - 1);
        }
    }

    /// Accept the highlighted candidate: replace `@query` in the input with the
    /// chosen path (plus a trailing space) and close the popup. No-op with an
    /// empty candidate list.
    pub fn file_popup_accept(&mut self) {
        let Some(p) = self.file_popup.take() else {
            return;
        };
        let Some(choice) = p.matches.get(p.selected).cloned() else {
            // Nothing to insert; leave the typed `@query` as-is.
            self.file_popup = None;
            return;
        };
        // Replace from the `@` to the end of input (the query is always the tail
        // while the popup is open) with "path ".
        self.input.truncate(p.at);
        self.input.push_str(&choice);
        self.input.push(' ');
        self.history_cursor = None;
    }

    /// Close the popup without inserting, leaving the typed `@query` in place.
    pub fn close_file_popup(&mut self) {
        self.file_popup = None;
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    // ---- Scroll ----

    /// The largest valid `scroll_back`: everything above the viewport. Derived
    /// from the last rendered frame's totals.
    fn max_scroll_back(&self) -> usize {
        self.total_rows.get().saturating_sub(self.view_height.get())
    }

    /// Clamp `scroll_back` into `[0, max_scroll_back]`. Called by the UI once per
    /// frame (through a `Cell`, hence `&self`) after it has recomputed the
    /// wrapped totals, so a resize — which changes wrapping and thus the max —
    /// can't leave us scrolled past the top.
    pub fn clamp_scroll(&self) {
        let max = self.max_scroll_back();
        if self.scroll_back.get() > max {
            self.scroll_back.set(max);
        }
    }

    pub fn scroll_up(&mut self) {
        let max = self.max_scroll_back();
        self.scroll_back
            .set(self.scroll_back.get().saturating_add(3).min(max));
    }

    pub fn scroll_down(&mut self) {
        self.scroll_back
            .set(self.scroll_back.get().saturating_sub(3));
    }

    pub fn is_scrolled_back(&self) -> bool {
        self.scroll_back.get() > 0
    }

    // ---- Input history ----

    pub fn input_newline(&mut self) {
        self.input.push('\n');
        self.history_cursor = None;
        self.draft_input.clear();
    }

    pub fn history_up(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        if self.history_cursor.is_none() {
            self.draft_input = self.input.clone();
        }
        let cursor = self
            .history_cursor
            .map_or(0, |c| (c + 1).min(self.input_history.len() - 1));
        let idx = self.input_history.len() - 1 - cursor;
        self.input = self.input_history[idx].clone();
        self.history_cursor = Some(cursor);
    }

    pub fn history_down(&mut self) {
        match self.history_cursor {
            None => return,
            Some(0) => {
                self.input = self.draft_input.clone();
                self.history_cursor = None;
            }
            Some(c) => {
                let cursor = c - 1;
                let idx = self.input_history.len() - 1 - cursor;
                self.input = self.input_history[idx].clone();
                self.history_cursor = Some(cursor);
            }
        }
    }

    // ---- Session management ----

    pub fn cancel_session(&mut self) {
        if let Some(h) = self.abort_handle.take() {
            h.abort();
        }
        self.messages
            .push(Message::new(Role::System, "session cancelled"));
        self.running = false;
        self.waiting_first_token = false;
    }

    pub fn apply_resume(&mut self, loaded: Vec<ChatMessage>) {
        self.show_splash = false;
        self.history = loaded.clone();
        self.messages.clear();
        self.messages.push(Message::new(
            Role::System,
            "session resumed — continuing from previous conversation",
        ));
        for msg in &loaded {
            match msg.role.as_str() {
                "user" => {
                    if let Some(ref c) = msg.content {
                        self.messages.push(Message::new(Role::User, c.clone()));
                    }
                }
                "assistant" => {
                    // Reasoning (chain-of-thought) attached to this turn.
                    let reasoning = msg.reasoning_content.clone().unwrap_or_default();
                    let content = msg.content.clone().unwrap_or_default();

                    // Assistant text / reasoning first (the model "thinks" then
                    // acts, so reasoning/content appears before tool calls).
                    if !content.is_empty() || !reasoning.is_empty() {
                        let mut m = Message::new(Role::Assistant, content);
                        if !reasoning.is_empty() {
                            m.reasoning = reasoning;
                            m.reasoning_collapsed = true;
                            m.reasoning_secs = Some(0);
                        }
                        self.messages.push(m);
                    }

                    // Tool calls embedded in the assistant turn.
                    if let Some(ref calls) = msg.tool_calls {
                        for tc in calls {
                            let mut m = Message::new(
                                Role::Tool,
                                gfx::tool_call_label(&tc.function.name, &tc.function.arguments),
                            );
                            m.args = Some(tc.function.arguments.clone());
                            self.messages.push(m);
                        }
                    }
                }
                "tool" => {
                    if let Some(ref c) = msg.content {
                        let trimmed = c.trim_end().to_string();
                        let mut m =
                            Message::new(Role::ToolOutput, gfx::first_lines(&trimmed, 12));
                        // Always keep full content for collapsible output.
                        m.full_content = Some(trimmed);
                        m.output_collapsed = true; // resumed sessions start collapsed
                        self.messages.push(m);
                    }
                }
                _ => {}
            }
        }
    }

    // ---- Ctrl+C ----

    pub fn handle_ctrl_c(&mut self) -> bool {
        if !self.input.is_empty() {
            self.clear_input();
            self.last_ctrl_c = Some(Instant::now());
            return false;
        }
        let now = Instant::now();
        let should_quit = self
            .last_ctrl_c
            .map_or(false, |t| now.duration_since(t).as_millis() < 1500);
        self.last_ctrl_c = Some(now);
        should_quit
    }

    // ---- Paste & clipboard ----

    pub fn paste(&mut self, text: &str) {
        if self.running {
            return;
        }
        // When an overlay is open, inject the pasted text into its active field.
        if self.overlay.is_some() {
            self.paste_overlay(text);
            return;
        }
        self.input.push_str(text);
        self.history_cursor = None;
        self.draft_input.clear();
    }

    /// Inject pasted text into the active overlay's text field (filter or form field).
    fn paste_overlay(&mut self, text: &str) {
        let Some(overlay) = self.overlay.take() else { return };
        let sanitized = text.replace('\n', " ").replace('\r', " ");
        match overlay {
            Overlay::CommandMenu { mut filter, selected } => {
                filter.push_str(&sanitized);
                self.overlay = Some(Overlay::CommandMenu { filter, selected });
            }
            Overlay::ModelPicker(picker) => match picker {
                ModelPicker::Ready { all, mut filter, selected } => {
                    filter.push_str(&sanitized);
                    self.overlay = Some(Overlay::ModelPicker(ModelPicker::Ready { all, filter, selected }));
                }
                other => {
                    self.overlay = Some(Overlay::ModelPicker(other));
                }
            },
            Overlay::ConnectWizard(mut wiz) => {
                match wiz.field {
                    0 => wiz.name.push_str(&sanitized),
                    1 => wiz.api_key.push_str(&sanitized),
                    2 => wiz.base_url.push_str(&sanitized),
                    _ => {}
                }
                self.overlay = Some(Overlay::ConnectWizard(wiz));
            }
        }
    }

    pub fn selection_rows(&self) -> Option<(u16, u16)> {
        self.selection.map(|(a, b)| (a.min(b), a.max(b)))
    }

    pub fn take_selected_text(&self) -> String {
        let Some((top, bottom)) = self.selection_rows() else {
            return String::new();
        };
        let rows = self.row_text.borrow();
        let mut out: Vec<String> = Vec::new();
        for y in top..=bottom {
            if let Some(text) = rows.get(&y) {
                out.push(text.clone());
            }
        }
        while out.last().map(|s| s.trim().is_empty()).unwrap_or(false) {
            out.pop();
        }
        out.join("\n")
    }

    pub fn toast_active(&self) -> bool {
        self.copied_at
            .map(|t| t.elapsed().as_millis() < 1500)
            .unwrap_or(false)
    }

    // ---- Overlays ----

    pub fn overlay_active(&self) -> bool {
        self.overlay.is_some()
    }

    pub fn open_command_menu(&mut self) {
        self.overlay = Some(Overlay::CommandMenu {
            filter: String::new(),
            selected: 0,
        });
    }

    /// Open the interactive connect-wizard overlay to add a new provider.
    pub fn open_connect_wizard(&mut self) {
        self.overlay = Some(Overlay::ConnectWizard(ConnectWizard {
            field: 0,
            name: String::new(),
            api_key: String::new(),
            base_url: String::from("https://api.openai.com/v1"),
        }));
    }

    /// Commands matching the current command-menu filter (prefix match).
    /// Merges built-in commands with plugin-contributed commands.
    pub fn filtered_commands(&self, filter: &str) -> Vec<SlashCommand> {
        let mut results: Vec<SlashCommand> = COMMANDS
            .iter()
            .filter(|c| c.name.starts_with(filter))
            .copied()
            .collect();
        for cmd in &self.plugin_commands {
            if cmd.name.starts_with(filter) {
                results.push(*cmd);
            }
        }
        results
    }

    /// Models matching the picker filter (case-insensitive substring).
    pub fn filtered_models<'a>(all: &'a [String], filter: &str) -> Vec<&'a String> {
        let f = filter.to_lowercase();
        all.iter().filter(|m| m.to_lowercase().contains(&f)).collect()
    }

    /// Feed a key to the active overlay. Returns an action for the event loop.
    pub fn overlay_key(&mut self, key: OverlayKey) -> OverlayAction {
        let Some(overlay) = self.overlay.take() else {
            return OverlayAction::None;
        };
        match overlay {
            Overlay::ConnectWizard(wiz) => self.connect_wizard_key(wiz, key),
            Overlay::CommandMenu {
                mut filter,
                mut selected,
            } => {
                match key {
                    OverlayKey::Esc => return OverlayAction::None,
                    OverlayKey::Char(c) => {
                        filter.push(c);
                        selected = 0;
                    }
                    OverlayKey::Backspace => {
                        if filter.is_empty() {
                            return OverlayAction::None;
                        }
                        filter.pop();
                        selected = 0;
                    }
                    OverlayKey::Up => {
                        let n = self.filtered_commands(&filter).len().max(1);
                        selected = (selected + n - 1) % n;
                    }
                    OverlayKey::Down => {
                        let n = self.filtered_commands(&filter).len().max(1);
                        selected = (selected + 1) % n;
                    }
                    OverlayKey::Enter => {
                        let matches = self.filtered_commands(&filter);
                        if let Some(cmd) = matches.get(selected) {
                            return self.run_command(cmd.name);
                        }
                        return OverlayAction::None;
                    }
                }
                self.overlay = Some(Overlay::CommandMenu { filter, selected });
                OverlayAction::None
            }
            Overlay::ModelPicker(picker) => self.model_picker_key(picker, key),
        }
    }

    fn model_picker_key(&mut self, picker: ModelPicker, key: OverlayKey) -> OverlayAction {
        match picker {
            ModelPicker::Ready {
                all,
                mut filter,
                mut selected,
            } => {
                match key {
                    OverlayKey::Esc => return OverlayAction::None,
                    OverlayKey::Char(c) => {
                        filter.push(c);
                        selected = 0;
                    }
                    OverlayKey::Backspace => {
                        filter.pop();
                        selected = 0;
                    }
                    OverlayKey::Up => {
                        let n = Self::filtered_models(&all, &filter).len().max(1);
                        selected = (selected + n - 1) % n;
                    }
                    OverlayKey::Down => {
                        let n = Self::filtered_models(&all, &filter).len().max(1);
                        selected = (selected + 1) % n;
                    }
                    OverlayKey::Enter => {
                        let matches = Self::filtered_models(&all, &filter);
                        if let Some(model) = matches.get(selected) {
                            return OverlayAction::ApplyModel((*model).clone());
                        }
                        return OverlayAction::None;
                    }
                }
                self.overlay = Some(Overlay::ModelPicker(ModelPicker::Ready {
                    all,
                    filter,
                    selected,
                }));
                OverlayAction::None
            }
            other => {
                if !matches!(key, OverlayKey::Esc) {
                    self.overlay = Some(Overlay::ModelPicker(other));
                }
                OverlayAction::None
            }
        }
    }

    fn connect_wizard_key(
        &mut self,
        mut wiz: ConnectWizard,
        key: OverlayKey,
    ) -> OverlayAction {
        match key {
            OverlayKey::Esc => return OverlayAction::None,
            OverlayKey::Enter => {
                // Enter on the last field (base_url) submits the form.
                if wiz.field == 2 && !wiz.name.trim().is_empty() && !wiz.api_key.trim().is_empty() {
                    let name = wiz.name.trim().to_lowercase();
                    let api_key = wiz.api_key.trim().to_string();
                    let base_url = wiz.base_url.trim().trim_end_matches('/').to_string();
                    let base_url = if base_url.is_empty() {
                        "https://api.openai.com/v1".to_string()
                    } else {
                        base_url
                    };
                    return OverlayAction::ConnectProvider {
                        name,
                        api_key,
                        base_url,
                    };
                }
                // Otherwise, advance to next field.
                wiz.field = (wiz.field + 1).min(2);
            }
            OverlayKey::Char('\t') => {
                // Tab cycles forward through fields.
                wiz.field = (wiz.field + 1) % 3;
            }
            OverlayKey::Up => {
                // Shift focus to previous field.
                wiz.field = if wiz.field == 0 { 2 } else { wiz.field - 1 };
            }
            OverlayKey::Down => {
                // Shift focus to next field.
                wiz.field = (wiz.field + 1) % 3;
            }
            OverlayKey::Backspace => {
                match wiz.field {
                    0 => { wiz.name.pop(); }
                    1 => { wiz.api_key.pop(); }
                    2 => { wiz.base_url.pop(); }
                    _ => {}
                }
            }
            OverlayKey::Char(c) => {
                match wiz.field {
                    0 => wiz.name.push(c),
                    1 => wiz.api_key.push(c),
                    2 => wiz.base_url.push(c),
                    _ => {}
                }
            }
        }
        self.overlay = Some(Overlay::ConnectWizard(wiz));
        OverlayAction::None
    }

    fn run_command(&mut self, name: &str) -> OverlayAction {
        match name {
            "model" => {
                self.overlay = Some(Overlay::ModelPicker(ModelPicker::Loading));
                OverlayAction::FetchModels
            }
            "connect" => {
                self.open_connect_wizard();
                OverlayAction::None
            }
            "connect-remove" => {
                // This command needs an argument; pre-fill the input and let the
                // user type the rest, then handle on Enter.
                self.overlay = None;
                self.input = format!("/{name} ");
                self.history_cursor = None;
                self.draft_input.clear();
                OverlayAction::None
            }
            other => {
                if self.plugin_commands.iter().any(|c| c.name == other) {
                    OverlayAction::PluginCommand(other.to_string())
                } else {
                    OverlayAction::None
                }
            }
        }
    }

    pub fn set_model_list(&mut self, result: Result<Vec<String>, String>) {
        if !matches!(self.overlay, Some(Overlay::ModelPicker(_))) {
            return;
        }
        self.overlay = Some(Overlay::ModelPicker(match result {
            Ok(all) => {
                let selected = all.iter().position(|m| *m == self.model).unwrap_or(0);
                ModelPicker::Ready {
                    all,
                    filter: String::new(),
                    selected,
                }
            }
            Err(e) => ModelPicker::Error(e),
        }));
    }

    pub fn apply_chosen_model(&mut self, model: String) {
        self.model = model.clone();
        self.overlay = None;
        self.messages
            .push(Message::new(Role::System, format!("switched model to {model}")));
    }

    /// Refresh the cached provider list from config (called after /connect etc.).
    pub fn refresh_providers(&mut self, config: &crate::config::Config) {
        self.all_providers = config
            .all_providers()
            .into_iter()
            .map(|(n, k, u)| (n.to_string(), k.to_string(), u.to_string()))
            .collect();
    }

    /// Handle a click inside an overlay at row `y`. Returns an action.
    pub fn overlay_click(&mut self, y: u16) -> OverlayAction {
        let hit = self
            .overlay_hitboxes
            .borrow()
            .iter()
            .find(|(row, _)| *row == y)
            .map(|(_, idx)| *idx);
        let Some(idx) = hit else {
            return OverlayAction::None;
        };
        match self.overlay.take() {
            Some(Overlay::CommandMenu { filter, .. }) => {
                let matches = self.filtered_commands(&filter);
                if let Some(cmd) = matches.get(idx) {
                    return self.run_command(cmd.name);
                }
                OverlayAction::None
            }
            Some(Overlay::ModelPicker(ModelPicker::Ready { all, filter, .. })) => {
                let matches = Self::filtered_models(&all, &filter);
                if let Some(model) = matches.get(idx) {
                    return OverlayAction::ApplyModel((*model).clone());
                }
                self.overlay = Some(Overlay::ModelPicker(ModelPicker::Ready {
                    all,
                    filter,
                    selected: idx,
                }));
                OverlayAction::None
            }
            other => {
                self.overlay = other;
                OverlayAction::None
            }
        }
    }
}
