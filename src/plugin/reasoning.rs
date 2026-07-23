//! Reasoning plugin: click-to-toggle thought chain and tool collapse/expand.
//!
//! When the user clicks a "▸ thought for Ns" or "▾ thought for Ns" line,
//! this plugin toggles [`Message::reasoning_collapsed`] for the corresponding
//! message. It also handles clicks on tool-call and tool-output headers
//! via [`Message::tool_collapsed`] and [`Message::output_collapsed`].
//! The hit-test data (`thought_hitboxes` and `tool_hitboxes`) is built by the
//! UI each frame and stored on [`App`].

use crate::app::App;
use super::Plugin;

pub struct ReasoningPlugin;

impl Plugin for ReasoningPlugin {
    fn name(&self) -> &'static str {
        "reasoning"
    }

    fn on_mouse_up(&self, row: u16, app: &mut App) -> bool {
        // Try thought hitboxes first.
        {
            let hitboxes = app.thought_hitboxes.borrow();
            let hit = hitboxes
                .iter()
                .find(|(r, _)| *r == row)
                .or_else(|| hitboxes.iter().find(|(r, _)| r.abs_diff(row) <= 1))
                .map(|(_, idx)| *idx);
            let hitboxes_snapshot = hitboxes.clone();
            drop(hitboxes);

            log::debug!(
                "reasoning click: row={row} hit={:?} hitboxes={:?} msg_count={}",
                hit,
                hitboxes_snapshot,
                app.messages.len(),
            );

            if let Some(idx) = hit
                && let Some(msg) = app.messages.get_mut(idx)
            {
                let was = msg.reasoning_collapsed;
                msg.reasoning_collapsed = !was;
                log::debug!(
                    "reasoning toggle msg_idx={idx} {was}->{} reasoning_len={}",
                    msg.reasoning_collapsed,
                    msg.reasoning.len(),
                );
                return true;
            }
        }

        // Try tool hitboxes.
        {
            let tool_hitboxes = app.tool_hitboxes.borrow();
            let hit = tool_hitboxes
                .iter()
                .find(|(r, _)| *r == row)
                .or_else(|| tool_hitboxes.iter().find(|(r, _)| r.abs_diff(row) <= 1))
                .map(|(_, idx)| *idx);
            let snapshot = tool_hitboxes.clone();
            drop(tool_hitboxes);

            log::debug!(
                "tool click: row={row} hit={:?} tool_hitboxes={:?}",
                hit,
                snapshot,
            );

            if let Some(idx) = hit
                && let Some(msg) = app.messages.get_mut(idx)
            {
                match msg.role {
                    crate::app::Role::Tool => {
                        let was = msg.tool_collapsed;
                        msg.tool_collapsed = !was;
                        log::debug!(
                            "tool toggle msg_idx={idx} {was}->{}",
                            msg.tool_collapsed,
                        );
                        return true;
                    }
                    crate::app::Role::ToolOutput => {
                        let was = msg.output_collapsed;
                        msg.output_collapsed = !was;
                        log::debug!(
                            "tool-output toggle msg_idx={idx} {was}->{}",
                            msg.output_collapsed,
                        );
                        return true;
                    }
                    _ => {}
                }
            }
        }

        log::debug!("reasoning click miss at row={row}");
        false
    }
}
