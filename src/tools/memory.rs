//! Memory retrieval tools: let the model recall past experience notes.
//!
//! The memory plugin (`crate::plugin::memory`) files hard-won lessons under
//! `~/.egg-agent/memory/<scope>/<category>/`. These two tools are the *read*
//! side — the model pulls a relevant note back into the conversation on
//! demand, so it can avoid repeating a mistake it (or a past session) already
//! worked through.
//!
//! - `memory_search` — keyword search; returns the best-matching notes.
//! - `skill_tree` — browse the note tree, or open one note by path.
//!
//! Both are read-only and never mutate memory.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::Tool;
use crate::memory;

/// Keyword search over the experience-memory notes.
pub struct MemorySearch;

#[async_trait]
impl Tool for MemorySearch {
    fn name(&self) -> &'static str {
        "memory_search"
    }

    fn description(&self) -> &'static str {
        "Search your own experience-memory: past lessons distilled from hard-won \
         debugging/exploration (pitfalls hit, workarounds found, reusable procedures). \
         Call this BEFORE tackling a task that smells familiar — investigating logs, \
         a build/dependency error, an unfamiliar API or tool — to check whether a past \
         session already figured out the tricky part. Returns the best-matching notes \
         (title + path + preview). Set `full: true` to inline the top note's full text. \
         Then open a specific note with `skill_tree` using its path."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords describing the problem or topic. Examples: 'aliyun SLS tag search', 'cargo dependency conflict', 'ratatui render'. Space-separated tokens; matched against note titles, tags, and body."
                },
                "full": {
                    "type": "boolean",
                    "description": "When true, inline the full text of the single best match (if there is a clear top hit). Default false: return just the ranked list."
                }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, args: Value) -> Result<String> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return Ok("error: 'query' must be a non-empty string".to_string());
        }
        let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);

        let hits = memory::search(query, 5);
        if hits.is_empty() {
            return Ok(format!(
                "(no experience notes match '{query}' — nothing archived on this topic yet)"
            ));
        }

        let mut out = format!("Found {} matching experience note(s):\n\n", hits.len());
        for (i, h) in hits.iter().enumerate() {
            let rel = display_path(&h.path);
            out.push_str(&format!(
                "{}. [{}]  (score {})\n   path: {}\n   {}\n\n",
                i + 1,
                if h.title.is_empty() { "(untitled)" } else { &h.title },
                h.score,
                rel,
                h.snippet
            ));
        }

        // Inline the top note when asked and there's a decisive winner (its
        // score strictly beats the runner-up), so `full` doesn't dump an
        // arbitrary note when several tie.
        if full {
            let decisive = hits.len() == 1 || hits[0].score > hits[1].score;
            if decisive {
                match memory::read_note(&hits[0].path) {
                    Ok(body) => {
                        out.push_str("--- top match, full text ---\n\n");
                        out.push_str(&body);
                    }
                    Err(e) => out.push_str(&format!("(could not read top note: {e:#})")),
                }
            } else {
                out.push_str(
                    "(several notes tie for top score — open one explicitly with skill_tree)",
                );
            }
        }

        Ok(out)
    }
}

/// Browse the memory note tree, or open a single note by path.
pub struct SkillTree;

#[async_trait]
impl Tool for SkillTree {
    fn name(&self) -> &'static str {
        "skill_tree"
    }

    fn description(&self) -> &'static str {
        "Browse your experience-memory as a topic tree, or open one note. \
         Call with no `path` to see every archived lesson organized by scope \
         (global vs. per-project) and topic — a map of 'what I've learned'. \
         Pass a directory path (e.g. 'global/aliyun') to list a subtree, or a \
         note path ending in '.md' to read that note's full text. Pair with \
         memory_search when you know keywords; use this to explore when you don't."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Optional. A subtree like 'global' or 'global/aliyun' to list, or a note path like 'global/aliyun/sls-tag.md' to read in full. Omit to list the whole tree."
                }
            }
        })
    }

    async fn call(&self, args: Value) -> Result<String> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty());

        // A note path resolves to its full text; a directory (or None) lists.
        // `memory::list_tree` already reads a file path as its contents, so we
        // just forward — but for a clearly-a-file path, prefer read_note so a
        // read error surfaces plainly.
        match path {
            Some(p) if p.ends_with(".md") => {
                let root = memory::dir()?;
                let mut file = root.clone();
                for seg in p.split(['/', '\\']) {
                    let seg = seg.trim();
                    if seg.is_empty() || seg == "." || seg == ".." {
                        continue;
                    }
                    file = file.join(seg);
                }
                if file.is_file() {
                    memory::read_note(&file)
                } else {
                    Ok(format!("(no such note: {p})"))
                }
            }
            other => memory::list_tree(other),
        }
    }
}

/// Note path relative to the memory root, forward-slashed — the value the
/// tools accept back as `path`.
fn display_path(file: &std::path::Path) -> String {
    let root = memory::dir().ok();
    let rel = root
        .as_ref()
        .and_then(|r| file.strip_prefix(r).ok())
        .unwrap_or(file);
    rel.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_rejects_empty_query() {
        let out = MemorySearch
            .call(serde_json::json!({"query": "   "}))
            .await
            .unwrap();
        assert!(out.starts_with("error:"), "got: {out}");
    }

    #[tokio::test]
    async fn search_missing_topic_is_friendly_not_panic() {
        // A query that won't match anything real → graceful "nothing archived".
        let out = MemorySearch
            .call(serde_json::json!({"query": "zzz_nonexistent_topic_qwxz"}))
            .await
            .unwrap();
        assert!(out.contains("no experience notes match"), "got: {out}");
    }

    #[tokio::test]
    async fn skill_tree_nonexistent_note_is_friendly() {
        let out = SkillTree
            .call(serde_json::json!({"path": "global/nope/does-not-exist.md"}))
            .await
            .unwrap();
        assert!(out.contains("no such note"), "got: {out}");
    }

    #[tokio::test]
    async fn skill_tree_lists_without_panicking() {
        // With or without notes, listing the whole tree must return a string.
        let out = SkillTree.call(serde_json::json!({})).await.unwrap();
        assert!(!out.is_empty());
    }
}
