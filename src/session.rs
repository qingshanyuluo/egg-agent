//! Session persistence: save/load conversation history so the user can
//! resume a previous session on next launch.
//!
//! Sessions are stored as JSON files under `~/.egg-agent/sessions/`, one
//! file per session. Each file contains the full `Vec<ChatMessage>` history
//! that the agent loop uses as context.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::llm::ChatMessage;

/// Directory holding session files: `~/.egg-agent/sessions/`.
fn dir() -> Result<PathBuf> {
    let d = Config::dir()?.join("sessions");
    std::fs::create_dir_all(&d)
        .with_context(|| format!("could not create sessions dir {}", d.display()))?;
    Ok(d)
}

/// A saved session, listed for resume.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub path: PathBuf,
    /// Short id for CLI use (e.g. "20260723-1530").
    pub id: String,
    /// When the session was saved (ISO timestamp from filename).
    pub timestamp: String,
    /// First user message as a preview.
    pub preview: String,
}

/// Save the conversation history. Returns the file path.
pub fn save(history: &[ChatMessage]) -> Result<PathBuf> {
    if history.len() <= 1 {
        // Nothing meaningful to save (just system prompt or nothing).
        anyhow::bail!("nothing to save");
    }

    let now = chrono_now();
    let filename = format!("session-{now}.json");
    let path = dir()?.join(&filename);

    let json = serde_json::to_string_pretty(history).context("could not serialize session")?;
    std::fs::write(&path, json)
        .with_context(|| format!("could not write session to {}", path.display()))?;

    Ok(path)
}

/// Load a session from a file path.
pub fn load(path: &std::path::Path) -> Result<Vec<ChatMessage>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read session {}", path.display()))?;
    let history: Vec<ChatMessage> =
        serde_json::from_str(&text).context("could not parse session file")?;
    Ok(history)
}

/// List available sessions, newest first.
pub fn list() -> Result<Vec<SessionInfo>> {
    let d = dir()?;
    let mut entries: Vec<SessionInfo> = Vec::new();

    let read_dir = match std::fs::read_dir(&d) {
        Ok(rd) => rd,
        Err(_) => return Ok(entries),
    };

    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().map_or(true, |e| e != "json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // "session-2026-07-23T15-30-00" → "2026-07-23 15:30:00"
        let timestamp = stem
            .strip_prefix("session-")
            .unwrap_or(stem)
            .replace('T', " ");
        // Short id: "20260723-1530" for CLI use.
        let id = timestamp
            .replace('-', "")
            .replace(' ', "-")
            .chars()
            .take(13)
            .collect::<String>();

        // Quick preview: first user message (up to 80 chars).
        let preview = match std::fs::read_to_string(&path) {
            Ok(text) => {
                match serde_json::from_str::<Vec<ChatMessage>>(&text) {
                    Ok(msgs) => msgs
                        .iter()
                        .find(|m| m.role == "user")
                        .and_then(|m| m.content.as_deref())
                        .map(|c| {
                            // Use char_indices to find a safe UTF-8 boundary
                            let mut end = 80;
                            if c.len() > 80 {
                                // Find the last char boundary at or before byte 80
                                if let Some((idx, _)) = c.char_indices().take_while(|(i, _)| *i <= 80).last() {
                                    end = idx;
                                    // Add the char's byte length to include it
                                    if let Some(ch) = c[idx..].chars().next() {
                                        end += ch.len_utf8();
                                    }
                                }
                                format!("{}…", &c[..end.min(c.len())])
                            } else {
                                c.to_string()
                            }
                        })
                        .unwrap_or_default(),
                    Err(_) => String::new(),
                }
            }
            Err(_) => String::new(),
        };

        entries.push(SessionInfo {
            path,
            id,
            timestamp,
            preview,
        });
    }

    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp)); // newest first
    Ok(entries)
}

/// Timestamp string safe for filenames.
pub(crate) fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let days_since_epoch = secs / 86400;
    let secs_in_day = secs % 86400;
    let hours = secs_in_day / 3600;
    let mins = (secs_in_day % 3600) / 60;
    let secs_rem = secs_in_day % 60;

    let (y, m, d) = civil_from_days(days_since_epoch as i64);

    format!("{y:04}-{m:02}-{d:02}T{hours:02}-{mins:02}-{secs_rem:02}-{millis:03}")
}

/// Convert days since Unix epoch to (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Algorithm from Howard Hinnant
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
