//! File editing tool: precise old_string → new_string replacement.
//!
//! Mirrors the approach used by Claude Code, Kimi Code, and Pi — the model
//! supplies the exact text to find and its replacement. No line numbers, no
//! diff format, no AST. Just find-and-replace with good error messages.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::Value;

use super::Tool;

pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replace old_string with new_string in an existing file. \
         old_string must appear exactly once in the file (or set replace_all: true). \
         Include enough surrounding context in old_string to make it unique — \
         2-3 lines above and below the change is usually sufficient."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find and replace. Must match the file content character-for-character, including indentation. Include surrounding context lines to ensure uniqueness."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences of old_string. Default: false."
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn call(&self, args: Value) -> Result<String> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .context("missing required argument 'path'")?;
        let old_string = args
            .get("old_string")
            .and_then(Value::as_str)
            .context("missing required argument 'old_string'")?;
        let new_string = args
            .get("new_string")
            .and_then(Value::as_str)
            .context("missing required argument 'new_string'")?;
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if old_string == new_string {
            bail!("old_string and new_string are identical — nothing to change");
        }

        let original = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("could not read '{path}'"))?;

        // Count matches. Use byte indices because old_string might contain
        // non-ASCII characters that affect char indexing.
        let matches: Vec<usize> = original.match_indices(old_string).map(|(i, _)| i).collect();

        if matches.is_empty() {
            // Build a helpful error: find the line where the first few chars
            // of old_string might appear, and show surrounding context.
            let needle_first_line = old_string.lines().next().unwrap_or("").trim();
            let mut hint = String::new();
            if !needle_first_line.is_empty() {
                for (line_num, line) in original.lines().enumerate() {
                    if line.contains(needle_first_line) {
                        let start = line_num.saturating_sub(2);
                        let end = (line_num + 3).min(original.lines().count());
                        let ctx: Vec<String> = original
                            .lines()
                            .skip(start)
                            .take(end - start)
                            .enumerate()
                            .map(|(i, l)| format!("  {:>5}  {}", start + i + 1, l))
                            .collect();
                        hint = format!(
                            "\n\nFile contains similar text near line {}:\n{}",
                            line_num + 1,
                            ctx.join("\n")
                        );
                        break;
                    }
                }
            }
            bail!(
                "old_string not found in '{path}'. \
                 The file may have changed since it was last read, or the old_string \
                 doesn't match exactly (check indentation, line endings, surrounding context).\
                 {hint}"
            );
        }

        if matches.len() > 1 && !replace_all {
            // Show the first few match locations to help the model disambiguate.
            let locations: Vec<String> = matches
                .iter()
                .take(5)
                .map(|&pos| {
                    let line = original[..pos].lines().count() + 1;
                    let preview: String = original[pos..]
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect();
                    format!("  line {}: \"{}\"", line, preview)
                })
                .collect();
            let more = if matches.len() > 5 {
                format!("\n  ... and {} more occurrences", matches.len() - 5)
            } else {
                String::new()
            };
            bail!(
                "old_string appears {} times in '{path}', but replace_all is false. \
                 Either set replace_all: true, or make old_string more specific by \
                 including more surrounding context.\n\
                 Occurrences:\n{}{}",
                matches.len(),
                locations.join("\n"),
                more,
            );
        }

        // Perform the replacement.
        let new_content = if replace_all {
            original.replace(old_string, new_string)
        } else {
            original.replacen(old_string, new_string, 1)
        };

        tokio::fs::write(path, &new_content)
            .await
            .with_context(|| format!("could not write '{path}'"))?;

        let count = if replace_all { matches.len() } else { 1 };
        Ok(format!(
            "replaced {} occurrence(s) of old_string in '{path}'",
            count
        ))
    }
}
