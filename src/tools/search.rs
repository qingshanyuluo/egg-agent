//! Content search tool: regex search across files in a directory.
//!
//! Uses ripgrep (`rg`) if available (fast, respects .gitignore), falling back
//! to `grep -rn`. This is the same approach used by Claude Code, Kimi Code,
//! and pie — shell out to a battle-tested search binary rather than
//! reimplementing recursive regex search in Rust.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Command;

use super::{Tool, MAX_OUTPUT_CHARS};

pub struct Search;

#[async_trait]
impl Tool for Search {
    fn name(&self) -> &'static str {
        "search"
    }

    fn description(&self) -> &'static str {
        "Search for a regex pattern in files under a directory. \
         Returns matching file paths with line numbers and content. \
         Prefer grep/ripgrep-compatible regex syntax. \
         Use this to locate code, find function definitions, or discover \
         where a particular string appears in the codebase."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for. Examples: 'fn main', 'TODO', 'import.*from', 'struct \\w+Config'"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search in. Defaults to the current working directory ('.')."
                },
                "include": {
                    "type": "string",
                    "description": "Glob pattern to filter files. Examples: '*.rs', '*.{py,js}', 'src/**/*.ts'. Default: search all text files."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn call(&self, args: Value) -> Result<String> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .context("missing required argument 'pattern'")?;
        let search_path = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".");
        let include = args
            .get("include")
            .and_then(Value::as_str);

        // Prefer ripgrep — fast, .gitignore-aware, color-free JSON output.
        if let Ok(output) = run_rg(pattern, search_path, include) {
            return Ok(truncate_lines(&output));
        }

        // Fallback: GNU/BSD grep.
        if let Ok(output) = run_grep(pattern, search_path, include) {
            return Ok(truncate_lines(&output));
        }

        bail!("neither 'rg' nor 'grep' found on PATH — cannot search")
    }
}

/// Try ripgrep: `rg --no-heading --line-number --color never <pattern> <path>`
fn run_rg(pattern: &str, path: &str, include: Option<&str>) -> Result<String, std::io::Error> {
    let mut cmd = Command::new("rg");
    cmd.args(["--no-heading", "--line-number", "--color", "never"])
        .arg("--max-count=200"); // cap per-file matches

    if let Some(glob) = include {
        cmd.arg("--glob").arg(glob);
    }

    cmd.arg("--").arg(pattern).arg(path);

    // Don't search binary files.
    cmd.arg("--no-ignore"); // we want to search all text files in workspace
    cmd.arg("-g").arg("!*.{o,a,so,dylib,exe,dll,wasm,bin,class,pyc,pyd,jar,war,ear,zip,tar,gz,bz2,xz,7z,rar,png,jpg,jpeg,gif,bmp,ico,mp3,mp4,avi,mkv,pdf,doc,docx,xls,xlsx,ppt,pptx,ttf,otf,woff,woff2,eot,db,db3,sqlite,sqlite3}");

    let output = cmd.output()?;
    if !output.status.success() {
        // rg returns 1 for "no matches", which is not an error for us
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok("(no matches)".to_string());
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("rg failed: {}", String::from_utf8_lossy(&output.stderr)),
        ));
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(text)
}

/// Fallback: `grep -rn --color=never <pattern> <path>`
fn run_grep(pattern: &str, path: &str, include: Option<&str>) -> Result<String, std::io::Error> {
    let mut cmd = Command::new("grep");
    cmd.args(["-rn", "--color=never", "-I"]) // -I = skip binary
        .arg("--max-count=200");

    if let Some(glob) = include {
        cmd.arg("--include").arg(glob);
    }

    cmd.arg(pattern).arg(path);

    let output = cmd.output()?;
    match output.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&output.stdout).to_string()),
        Some(1) => Ok("(no matches)".to_string()),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("grep failed: {}", String::from_utf8_lossy(&output.stderr)),
        )),
    }
}

/// Keep output under the global char limit, truncating with context.
fn truncate_lines(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        if text.is_empty() {
            "(no matches)".to_string()
        } else {
            text.to_string()
        }
    } else {
        let kept: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
        let total_lines = text.lines().count();
        let kept_lines = kept.lines().count();
        format!(
            "{kept}\n\n... [truncated: showing {kept_lines} of {total_lines} matching lines] ..."
        )
    }
}
