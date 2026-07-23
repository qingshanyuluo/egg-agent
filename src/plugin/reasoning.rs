//! Reasoning plugin: click-to-toggle thought chain collapse/expand.
//!
//! When the user clicks a "▸ thought for Ns" or "▾ thought for Ns" line,
//! this plugin toggles [`Message::reasoning_collapsed`] for the corresponding
//! message. The hit-test data (`thought_hitboxes`) is built by the UI each
//! frame and stored on [`App`].

use crate::app::App;
use super::Plugin;

pub struct ReasoningPlugin;

impl Plugin for ReasoningPlugin {
    fn name(&self) -> &'static str {
        "reasoning"
    }

    fn on_mouse_up(&self, row: u16, app: &mut App) -> bool {
        let hitboxes = app.thought_hitboxes.borrow();
        log::debug!(
            "reasoning click: row={row} hitboxes={:?} msg_count={}",
            hitboxes,
            app.messages.len(),
        );
        let hit = hitboxes
            .iter()
            .find(|(r, _)| *r == row)
            .map(|(_, idx)| *idx);
        drop(hitboxes);

        match hit {
            Some(idx) => match app.messages.get_mut(idx) {
                Some(msg) => {
                    let was = msg.reasoning_collapsed;
                    msg.reasoning_collapsed = !was;
                    log::debug!(
                        "reasoning toggle msg_idx={idx} {was}->{} reasoning_len={}",
                        msg.reasoning_collapsed,
                        msg.reasoning.len(),
                    );
                    true
                }
                None => {
                    log::warn!(
                        "reasoning hitbox -> missing msg_idx={idx} (have {} messages)",
                        app.messages.len()
                    );
                    false
                }
            },
            None => {
                log::debug!("reasoning click miss at row={row}");
                false
            }
        }
    }
}
