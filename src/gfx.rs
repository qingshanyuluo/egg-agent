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

/// Render tool-call arguments compactly: unwrap a single-key JSON object to just
/// its value (e.g. `{"command":"ls"}` -> `ls`), otherwise show a one-line JSON.
pub fn compact_args(args: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(args) else {
        return args.trim().to_string();
    };
    if let Some(obj) = value.as_object() {
        if obj.len() == 1 {
            if let Some(v) = obj.values().next() {
                if let Some(s) = v.as_str() {
                    return s.to_string();
                }
                return v.to_string();
            }
        }
    }
    value.to_string()
}
