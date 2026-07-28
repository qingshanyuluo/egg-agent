//! Plugin system for egg-agent.
//!
//! Plugins handle "outside the LLM loop" logic: mouse interactions, clipboard,
//! reasoning display, translation, bash explanation, etc. Each plugin is a
//! self-contained module implementing the [`Plugin`] trait.
//!
//! Plugins are called in registration order. A plugin returning `true` from
//! [`Plugin::on_mouse_up`] signals that the event was consumed and no further
//! plugins should process it.
//!
//! ## Slash commands
//!
//! Plugins contribute commands to the `/` palette via [`Plugin::commands`].
//! When the user selects one, [`Plugin::handle_command`] is called. This is
//! how toggle switches like `/translate` and `/explain` work.
//!
//! ## Plugin-to-main-loop events
//!
//! A plugin that produces async work (e.g. calling the aux LLM for translation)
//! sends a [`PluginEvent::Custom`] back to the main loop. The `field` string
//! tells the loop which [`Message`] field to write — new plugins can pick a
//! new `field` value without editing this module.

pub mod bash_explain;
pub mod clipboard;
pub mod memory;
pub mod reasoning;
pub mod translate;

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::agent::AgentEvent;
use crate::app::{App, SlashCommand};
use crate::llm::LlmClient;

/// Events plugins can send back to the main loop.
#[derive(Debug, Clone)]
pub enum PluginEvent {
    /// A plugin changed state; re-render needed.
    #[allow(dead_code)]
    Redraw,
    /// Generic plugin→app message: set `Message.field` to `text` at the given
    /// index.  The field name matches a field on [`crate::app::Message`].
    /// Examples: `"translation"`, `"explanation"`.
    Custom {
        msg_idx: usize,
        field: &'static str,
        text: String,
    },
}

/// A plugin hooks into the agent UI lifecycle to provide cross-cutting
/// features without tangling into the core loop.
///
/// All hooks have default no-op implementations; a plugin only implements
/// what it needs.
pub trait Plugin: Send + Sync {
    /// Human-readable name for debugging.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;

    /// Called after an [`AgentEvent`] has been applied to [`App`] state.
    /// Plugins can read the new state and react (e.g. accumulate reasoning
    /// for translation, detect patterns).
    fn on_agent_event(
        &self,
        _event: &AgentEvent,
        _app: &mut App,
        _aux: Option<&Arc<dyn LlmClient>>,
        _events: &UnboundedSender<PluginEvent>,
    ) {
    }

    /// Mouse button pressed on a transcript row.
    fn on_mouse_down(&self, _row: u16, _app: &mut App) {}

    /// Mouse dragged over a transcript row.
    fn on_mouse_drag(&self, _row: u16, _app: &mut App) {}

    /// Mouse button released on a transcript row.
    ///
    /// Return `true` if the plugin consumed this event (no further plugins
    /// will see it).
    fn on_mouse_up(&self, _row: u16, _app: &mut App) -> bool {
        false
    }

    // --- Slash command support ---

    /// Slash commands this plugin contributes to the `/` palette.
    fn commands(&self) -> Vec<(&'static str, &'static str)> {
        vec![]
    }

    /// Handle a command selected from the `/` palette.
    /// Return `true` if this plugin handled the command.
    fn handle_command(&self, _name: &str, _app: &mut App) -> bool {
        false
    }

    /// Whether the plugin is currently active. Used by toggle commands.
    fn is_enabled(&self) -> bool {
        true
    }
}

/// A registry of plugins called in insertion order.
pub struct Registry {
    plugins: Vec<Box<dyn Plugin>>,
}

impl Registry {
    /// Create a registry with the built-in plugin set.
    ///
    /// Order matters: clipboard goes first so it can consume multi-row
    /// selections before the reasoning plugin interprets them as clicks.
    pub fn builtin() -> Self {
        Self {
            plugins: vec![
                Box::new(clipboard::ClipboardPlugin),
                Box::new(reasoning::ReasoningPlugin),
                Box::new(translate::TranslatePlugin::new()),
                Box::new(bash_explain::BashExplainPlugin::new()),
            ],
        }
    }

    /// Register an extra plugin after the built-ins. Used for plugins that
    /// are constructed with their own resources (e.g. the memory plugin,
    /// which holds its own summarizer LLM client).
    pub fn add(&mut self, plugin: Box<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    // --- Agent / mouse dispatch ---

    pub fn on_agent_event(
        &self,
        event: &AgentEvent,
        app: &mut App,
        aux: Option<&Arc<dyn LlmClient>>,
        events: &UnboundedSender<PluginEvent>,
    ) {
        for p in &self.plugins {
            p.on_agent_event(event, app, aux, events);
        }
    }

    pub fn on_mouse_down(&self, row: u16, app: &mut App) {
        for p in &self.plugins {
            p.on_mouse_down(row, app);
        }
    }

    pub fn on_mouse_drag(&self, row: u16, app: &mut App) {
        for p in &self.plugins {
            p.on_mouse_drag(row, app);
        }
    }

    /// Dispatch `on_mouse_up` to plugins in registration order, stopping at
    /// the first plugin that returns `true`.
    pub fn on_mouse_up(&self, row: u16, app: &mut App) {
        for p in &self.plugins {
            if p.on_mouse_up(row, app) {
                break;
            }
        }
    }

    // --- Slash command dispatch ---

    /// Collect all slash commands from every plugin.
    pub fn all_commands(&self) -> Vec<SlashCommand> {
        let mut cmds = Vec::new();
        for p in &self.plugins {
            for (name, description) in p.commands() {
                cmds.push(SlashCommand { name, description });
            }
        }
        cmds
    }

    /// Try each plugin's [`Plugin::handle_command`]; return `true` if one handled it.
    pub fn dispatch_command(&self, name: &str, app: &mut App) -> bool {
        for p in &self.plugins {
            if p.handle_command(name, app) {
                return true;
            }
        }
        false
    }
}
