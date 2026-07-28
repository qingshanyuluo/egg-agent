//! Web search tool, backed by Exa's public MCP endpoint.
//!
//! Design follows opencode's `websearch` tool (packages/core/src/tool/
//! websearch.ts): rather than scraping a search engine's HTML — which major
//! engines (Google/Bing/DuckDuckGo) have blocked for datacenter IPs since
//! mid-2025 — we call a hosted search backend over MCP (JSON-RPC 2.0). Exa's
//! endpoint works with a free tier and no key; if the user provides a key
//! (via `EGG_EXA_API_KEY` or `EXA_API_KEY`) it's attached for higher limits.
//!
//! This keeps the tool stateless (like every other tool here) and leans on a
//! battle-tested backend instead of reimplementing web search.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::Tool;

/// Exa's hosted MCP search endpoint (works with a free tier, no key required).
const EXA_MCP_URL: &str = "https://mcp.exa.ai/mcp";
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_NUM_RESULTS: u64 = 8;
const MAX_NUM_RESULTS: u64 = 20;

pub struct WebSearch;

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the web for current information beyond the model's knowledge \
         cutoff. Returns a list of relevant results (titles, URLs, and text \
         snippets). Use this for recent events, library docs, or anything you \
         are unsure about. Follow up with the `web_fetch` tool to read a full \
         page. Backed by Exa's hosted search."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query."
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results to return (default: 8, maximum: 20)."
                }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, args: Value) -> Result<String> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .context("missing required argument 'query'")?;
        if query.trim().is_empty() {
            bail!("'query' must not be empty");
        }
        let num_results = args
            .get("num_results")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_NUM_RESULTS)
            .clamp(1, MAX_NUM_RESULTS);

        // Optional API key for higher rate limits; empty means "use free tier".
        let api_key = std::env::var("EGG_EXA_API_KEY")
            .or_else(|_| std::env::var("EXA_API_KEY"))
            .ok()
            .filter(|k| !k.trim().is_empty());

        let url = match &api_key {
            Some(key) => format!("{EXA_MCP_URL}?exaApiKey={key}"),
            None => EXA_MCP_URL.to_string(),
        };

        // MCP `tools/call` for Exa's `web_search_exa` tool.
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": query,
                    "numResults": num_results
                }
            }
        });

        let client = reqwest::Client::new();
        let req = client
            .post(&url)
            .header("Content-Type", "application/json")
            // Exa's MCP endpoint may reply as JSON or as an SSE stream; accept both.
            .header("Accept", "application/json, text/event-stream")
            .json(&body)
            .send();

        let resp = match tokio::time::timeout(SEARCH_TIMEOUT, req).await {
            Ok(res) => res.context("failed to reach the search backend")?,
            Err(_) => bail!("web search timed out after {SEARCH_TIMEOUT:?}"),
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("search backend returned HTTP {status}: {}", text.trim());
        }

        let raw = resp
            .text()
            .await
            .context("failed to read search response body")?;

        let text = parse_mcp_response(&raw)
            .context("could not parse search backend response")?;

        if text.trim().is_empty() {
            return Ok("No search results found. Try a different query.".to_string());
        }
        Ok(text)
    }
}

/// Extract the `result.content[].text` payload from an MCP `tools/call`
/// response. Handles both a plain JSON body and an SSE stream where the JSON
/// lives on a `data: ` line (same two shapes opencode handles).
fn parse_mcp_response(body: &str) -> Result<String> {
    // Try the body as-is first.
    if let Some(text) = extract_content_text(body) {
        return Ok(text);
    }
    // Otherwise scan for an SSE `data: {...}` line.
    for line in body.lines() {
        if let Some(payload) = line.strip_prefix("data: ") {
            if let Some(text) = extract_content_text(payload) {
                return Ok(text);
            }
        }
    }
    bail!("unexpected response shape: {}", truncate_for_err(body))
}

/// Parse one JSON object and pull out the first non-empty `content[].text`.
fn extract_content_text(payload: &str) -> Option<String> {
    let trimmed = payload.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;

    // Surface an MCP-level error if present.
    if let Some(err) = value.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Some(format!("search backend error: {msg}"));
    }

    let content = value.get("result")?.get("content")?.as_array()?;
    let joined = content
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn truncate_for_err(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 200 {
        let head: String = s.chars().take(200).collect();
        format!("{head}...")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_json_body() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"Result A\nResult B"}]}}"#;
        let out = parse_mcp_response(body).unwrap();
        assert!(out.contains("Result A"));
        assert!(out.contains("Result B"));
    }

    #[test]
    fn parses_sse_stream_body() {
        let body = "event: message\ndata: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"SSE result\"}]}}\n\n";
        let out = parse_mcp_response(body).unwrap();
        assert_eq!(out, "SSE result");
    }

    #[test]
    fn surfaces_mcp_error() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"rate limited"}}"#;
        let out = parse_mcp_response(body).unwrap();
        assert!(out.contains("rate limited"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_mcp_response("not json at all").is_err());
    }
}
