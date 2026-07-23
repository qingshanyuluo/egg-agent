//! Clipboard plugin: drag-select transcript rows → copy to system clipboard.
//!
//! Manages the selection state on [`App`] (anchor, range) and triggers
//! copy + toast on multi-row selections. Single clicks are passed through
//! so the reasoning plugin can handle thought-chain toggles.

use std::time::Instant;

use crate::app::App;
use crate::clipboard;
use super::Plugin;

pub struct ClipboardPlugin;

impl Plugin for ClipboardPlugin {
    fn name(&self) -> &'static str {
        "clipboard"
    }

    fn on_mouse_down(&self, row: u16, app: &mut App) {
        app.selection = Some((row, row));
    }

    fn on_mouse_drag(&self, row: u16, app: &mut App) {
        if let Some((anchor, _)) = app.selection {
            app.selection = Some((anchor, row));
        }
    }

    fn on_mouse_up(&self, _row: u16, app: &mut App) -> bool {
        let (top, bottom) = match app.selection_rows() {
            Some(range) => range,
            None => {
                log::debug!("clipboard: mouse up but no selection");
                return false;
            }
        };

        if top == bottom {
            log::debug!("clipboard: single click at row {top}, passing through");
            app.selection = None;
            return false;
        }

        // Multi-row selection: copy to system clipboard and show toast.
        let text = app.take_selected_text();
        log::debug!(
            "clipboard: selection rows={top}..{bottom} text_len={} text={:?}",
            text.len(),
            text.chars().take(80).collect::<String>(),
        );

        if text.trim().is_empty() {
            log::debug!("clipboard: selected text is empty, skipping copy");
            app.selection = None;
            return true;
        }

        let ok = clipboard::copy(&text);
        log::debug!("clipboard: copy result={ok}");
        if ok {
            app.copied_at = Some(Instant::now());
        }
        app.selection = None;
        true // consumed
    }
}
