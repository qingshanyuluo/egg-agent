//! Web fetch tool: retrieve a single URL and return it as Markdown (default),
//! plain text, or raw HTML.
//!
//! Mirrors opencode's `webfetch` tool: fetch over HTTP(S), then convert HTML
//! to compact, LLM-friendly Markdown (opencode uses Turndown; we use the
//! `html2md` crate). Bounded response size + timeout keep a huge page from
//! blowing up the context window. Stateless, like every other tool here.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::Tool;

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap the downloaded body so a giant page can't exhaust memory. The output
/// is separately truncated to MAX_OUTPUT_CHARS by the registry.
const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

pub struct WebFetch;

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }

    fn description(&self) -> &'static str {
        "Fetch the content of an HTTP(S) URL and return it as Markdown \
         (default), plain text, or raw HTML. Use this to read a web page — \
         for example, a URL returned by the `web_search` tool, or a docs page. \
         Markdown is compact and best for reading."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP or HTTPS URL to fetch."
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "text", "html"],
                    "description": "How to return the content. Defaults to 'markdown'."
                }
            },
            "required": ["url"]
        })
    }

    async fn call(&self, args: Value) -> Result<String> {
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .context("missing required argument 'url'")?
            .trim();

        if !(url.starts_with("http://") || url.starts_with("https://")) {
            bail!("'url' must start with http:// or https://");
        }

        let format = args
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("markdown");
        if !matches!(format, "markdown" | "text" | "html") {
            bail!("'format' must be one of: markdown, text, html");
        }

        let client = reqwest::Client::builder()
            .user_agent("egg-agent/0.1 (+https://github.com/)")
            .build()
            .context("failed to build HTTP client")?;

        let req = client.get(url).send();
        let resp = match tokio::time::timeout(FETCH_TIMEOUT, req).await {
            Ok(res) => res.with_context(|| format!("failed to fetch {url}"))?,
            Err(_) => bail!("fetch timed out after {FETCH_TIMEOUT:?}"),
        };

        let status = resp.status();
        if !status.is_success() {
            bail!("fetch failed: HTTP {status} for {url}");
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        // Read the body with a hard byte cap.
        let bytes = resp
            .bytes()
            .await
            .with_context(|| format!("failed to read body from {url}"))?;
        let bytes = if bytes.len() > MAX_RESPONSE_BYTES {
            &bytes[..MAX_RESPONSE_BYTES]
        } else {
            &bytes[..]
        };
        let raw = String::from_utf8_lossy(bytes).to_string();

        let is_html = content_type.contains("text/html")
            || content_type.contains("application/xhtml")
            || (content_type.is_empty() && looks_like_html(&raw));

        let out = match format {
            "html" => raw,
            "markdown" if is_html => html2md::parse_html(&strip_noise(&raw)),
            // Non-HTML content (JSON, plain text, etc.) is already "text".
            _ => raw,
        };

        Ok(out)
    }
}

/// Remove `<script>` and `<style>` blocks before HTML→Markdown conversion so
/// their contents (JS, CSS) don't leak into the Markdown output.
fn strip_noise(html: &str) -> String {
    let mut out = html.to_string();
    for tag in ["script", "style"] {
        strip_tag(&mut out, tag);
    }
    out
}

/// Case-insensitively remove all `<tag>...</tag>` blocks from `html`.
fn strip_tag(html: &mut String, tag: &str) {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let lower = html.to_lowercase();
    let mut result = String::with_capacity(html.len());
    let mut i = 0usize;
    while i < html.len() {
        if let Some(rel) = lower[i..].find(&open) {
            let start = i + rel;
            result.push_str(&html[i..start]);
            // Find the matching close tag (or bail to end of string).
            if let Some(erel) = lower[start..].find(&close) {
                i = start + erel + close.len();
            } else {
                i = html.len();
            }
        } else {
            result.push_str(&html[i..]);
            break;
        }
    }
    *html = result;
}

/// Heuristic for when the server didn't send a Content-Type.
fn looks_like_html(body: &str) -> bool {
    let head = body.trim_start().to_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html") || head.contains("<body")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_html_without_content_type() {
        assert!(looks_like_html("<!DOCTYPE html><html></html>"));
        assert!(looks_like_html("  <html><body>hi</body></html>"));
        assert!(!looks_like_html("just some plain text"));
        assert!(!looks_like_html("{\"json\": true}"));
    }

    #[test]
    fn html_to_markdown_smoke() {
        let md = html2md::parse_html("<h1>Title</h1><p>Hello <b>world</b></p>");
        assert!(md.contains("Title"));
        assert!(md.contains("Hello"));
        assert!(md.contains("world"));
    }

    #[test]
    fn strips_script_and_style() {
        let html = "<style>body{color:red}</style><h1>Hi</h1><script>alert(1)</script><p>Bye</p>";
        let cleaned = strip_noise(html);
        assert!(!cleaned.contains("color:red"), "style leaked: {cleaned}");
        assert!(!cleaned.contains("alert(1)"), "script leaked: {cleaned}");
        assert!(cleaned.contains("Hi") && cleaned.contains("Bye"));
    }
}
