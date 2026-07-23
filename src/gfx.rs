//! Pure utility functions shared by `app` and `ui`: terminal-color math,
//! tool-output truncation, and tool-argument display helpers.
//!
//! These have no business living on `App` — they're stateless functions that
//! happen to be called during event reduction or rendering.

use ratatui::style::Color;

/// Convert HSV (hue 0-360, saturation 0-1, value 0-1) to 8-bit RGB.
#[allow(dead_code)]
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// Produce a color that cycles through a soft rainbow over `period_ms`
/// milliseconds. Used by the splash screen.
#[allow(dead_code)]
pub fn splash_accent(elapsed_ms: u128, period_ms: u128, phase: f32) -> Color {
    let t = (elapsed_ms % period_ms) as f32 / period_ms as f32;
    let h = (t + phase).fract() * 360.0;
    let (r, g, b) = hsv_to_rgb(h, 0.55, 1.0);
    Color::Rgb(r, g, b)
}

/// Keep tool result previews from flooding the transcript.
pub fn first_lines(s: &str, max: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max {
        s.to_string()
    } else {
        format!("{}\n… {} more lines", lines[..max].join("\n"), lines.len() - max)
    }
}

/// Render a tool call as a compact one-liner: `bash  ls -la`, `read_file  src/main.rs`,
/// `edit_file  app.rs  (±12 lines)`, etc.  Content-heavy arguments are never dumped
/// into the transcript — only paths, sizes, and line counts.
pub fn tool_call_label(name: &str, args: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(args) else {
        return format!("{name}  {}", args.trim());
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return format!("{name}  {value}"),
    };

    match name {
        "bash" => {
            if let Some(cmd) = obj.get("command").and_then(|v| v.as_str()) {
                format!("bash  {cmd}")
            } else {
                format!("bash  {value}")
            }
        }
        "read_file" => {
            if let Some(p) = obj.get("path").and_then(|v| v.as_str()) {
                format!("read_file  {p}")
            } else {
                format!("read_file  {value}")
            }
        }
        "write_file" => {
            if let Some(p) = obj.get("path").and_then(|v| v.as_str()) {
                let n = obj
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|c| c.len())
                    .unwrap_or(0);
                format!("write_file  {p}  ({n} bytes)")
            } else {
                format!("write_file  {value}")
            }
        }
        "edit_file" => {
            if let Some(p) = obj.get("path").and_then(|v| v.as_str()) {
                let old = obj
                    .get("old_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new = obj
                    .get("new_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let old_lines = old.lines().count();
                let new_lines = new.lines().count();
                if old_lines == new_lines {
                    format!("edit_file  {p}  ({old_lines} lines)")
                } else {
                    let added = new_lines.saturating_sub(old_lines);
                    let removed = old_lines.saturating_sub(new_lines);
                    format!("edit_file  {p}  (+{added} / -{removed} lines)")
                }
            } else {
                format!("edit_file  {value}")
            }
        }
        "search" => {
            let pat = obj
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            if let Some(p) = obj.get("path").and_then(|v| v.as_str()) {
                if p != "." {
                    return format!("search  \"{pat}\"  in {p}");
                }
            }
            format!("search  \"{pat}\"")
        }
        _ => {
            // Unknown / future tool: single-key unwrap, else compact JSON.
            if obj.len() == 1 {
                if let Some(v) = obj.values().next().and_then(|v| v.as_str()) {
                    return format!("{name}  {v}");
                }
            }
            format!("{name}  {value}")
        }
    }
}
