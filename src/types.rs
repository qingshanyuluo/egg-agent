//! Shared domain types used across the crate: message model, slash commands,
//! overlay state, and the "intent" enums returned by overlay interactions.
//!
//! Extracted from `app.rs` so that module can focus on the App state machine
//! rather than also serving as the crate-wide type catalogue.

// ---- Message model ----

/// How a message is displayed in the transcript (distinct from the LLM role).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// A tool invocation line ("bash  ls src").
    Tool,
    /// The captured output of a tool, shown dimmed.
    ToolOutput,
    System,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Streamed chain-of-thought attached to an assistant message.
    pub reasoning: String,
    /// Whether the reasoning block is collapsed to a one-line summary.
    pub reasoning_collapsed: bool,
    /// Seconds the model spent reasoning (set when collapsed).
    pub reasoning_secs: Option<u64>,
    /// Chinese translation of reasoning, populated by TranslatePlugin.
    pub translation: Option<String>,
    /// Human-readable explanation of a tool call, populated by BashExplainPlugin.
    pub explanation: Option<String>,
    /// Full tool output (only for `Role::ToolOutput`), kept even when the
    /// on-screen preview is collapsed.
    pub full_content: Option<String>,
    /// Whether a long tool output is collapsed to a preview. Starts `true`;
    /// toggled by clicking the output's header row.
    pub output_collapsed: bool,
    /// Whether a tool call line is collapsed to a one-line summary.
    /// Tool calls default to expanded (false); bash commands with long args
    /// can be collapsed by the user.
    pub tool_collapsed: bool,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            reasoning: String::new(),
            reasoning_collapsed: false,
            reasoning_secs: None,
            translation: None,
            explanation: None,
            full_content: None,
            output_collapsed: true,
            tool_collapsed: false,
        }
    }
}

// ---- Slash commands ----

/// A slash command available from the input box.
#[derive(Debug, Clone, Copy)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

/// Built-in slash commands. Plugin-contributed commands are merged in at runtime.
pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "model",
        description: "Switch the active model",
    },
    SlashCommand {
        name: "connect",
        description: "Connect a provider: /connect <name> <api_key> [base_url]",
    },
    SlashCommand {
        name: "connect-remove",
        description: "Remove a provider: /connect-remove <name>",
    },
];

// ---- Modal overlays ----

/// A modal overlay drawn on top of the transcript. When present, it captures
/// keyboard and mouse input.
#[derive(Debug, Clone)]
pub enum Overlay {
    /// The `/`-triggered command palette. `filter` is the text typed after `/`.
    CommandMenu { filter: String, selected: usize },
    /// The model picker, fetched live from the provider.
    ModelPicker(ModelPicker),
    /// Interactive form to add a new API provider step by step.
    ConnectWizard(ConnectWizard),
}

/// State for the interactive provider-connection wizard.
#[derive(Debug, Clone)]
pub struct ConnectWizard {
    /// Which field is currently focused: 0 = name, 1 = api_key, 2 = base_url
    pub field: usize,
    pub name: String,
    pub api_key: String,
    pub base_url: String,
}

#[derive(Debug, Clone)]
pub enum ModelPicker {
    /// Fetching the model list over the network.
    Loading,
    /// Loaded; user is filtering/selecting.
    Ready {
        all: Vec<String>,
        filter: String,
        selected: usize,
    },
    /// Fetch failed.
    Error(String),
}

// ---- Overlay I/O ----

/// A normalized key delivered to an overlay widget.
#[derive(Debug, Clone, Copy)]
pub enum OverlayKey {
    Char(char),
    Backspace,
    Up,
    Down,
    Enter,
    Esc,
}

/// What the event loop should do after an overlay interaction.
#[derive(Debug, Clone)]
pub enum OverlayAction {
    None,
    /// Kick off the async model-list fetch.
    FetchModels,
    /// Apply and persist the chosen model.
    ApplyModel(String),
    /// Save the newly connected provider: (name, api_key, base_url).
    ConnectProvider { name: String, api_key: String, base_url: String },
    /// A plugin-registered command was selected; the main loop should
    /// dispatch it through the plugin registry.
    PluginCommand(String),
}
