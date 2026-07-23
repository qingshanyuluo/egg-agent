//! OpenAI-compatible chat completions client (works with DeepSeek, Kimi, GLM, Qwen).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;

use super::{ChatMessage, FunctionCall, LlmClient, StreamEvent, ToolCall, ToolDef};

pub struct OpenAiClient {
    http: reqwest::Client,
    /// Shared so `/model` (and provider switching) can change credentials
    /// at runtime without rebuilding the client or agent's `Arc<dyn LlmClient>`.
    base_url: Mutex<String>,
    api_key: Mutex<String>,
    model: Arc<Mutex<String>>,
}

impl OpenAiClient {
    /// 10 s to establish the TCP+TLS connection.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
    /// 2 min hard cap on a single HTTP request (streaming + tool calls).
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
    /// If no chunk arrives within this window the stream is considered dead.
    /// More lenient for the first chunk (TTFT can be 5-15 s for large models);
    /// aggressive for subsequent chunks (a healthy stream pushes tokens every
    /// few hundred ms — a 5 s gap means the connection is stalled).
    const FIRST_CHUNK_TIMEOUT: Duration = Duration::from_secs(30);
    const NEXT_CHUNK_TIMEOUT: Duration = Duration::from_secs(5);

    fn build_http() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Self::CONNECT_TIMEOUT)
            .timeout(Self::REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client builder should never fail with these settings")
    }

    pub fn new(config: &Config) -> Self {
        Self {
            http: Self::build_http(),
            base_url: Mutex::new(config.base_url.clone()),
            api_key: Mutex::new(config.api_key.clone()),
            model: Arc::new(Mutex::new(config.model.clone())),
        }
    }

    /// Build a client with explicit parameters (used for the aux model, which
    /// may have a different model/provider than the main config).
    pub fn with_params(base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            http: Self::build_http(),
            base_url: Mutex::new(base_url.to_string()),
            api_key: Mutex::new(api_key.to_string()),
            model: Arc::new(Mutex::new(model.to_string())),
        }
    }

    pub fn base_url(&self) -> String {
        self.base_url.lock().unwrap().clone()
    }

    pub fn api_key(&self) -> String {
        self.api_key.lock().unwrap().clone()
    }

    /// Switch the active model. Takes effect on the next request.
    pub fn set_model(&self, model: impl Into<String>) {
        *self.model.lock().unwrap() = model.into();
    }

    /// Switch credentials to a different provider. Takes effect on the next request.
    pub fn set_provider(&self, base_url: &str, api_key: &str) {
        *self.base_url.lock().unwrap() = base_url.to_string();
        *self.api_key.lock().unwrap() = api_key.to_string();
    }

    fn current_model(&self) -> String {
        self.model.lock().unwrap().clone()
    }
}

/// Fetch the list of model IDs available at an OpenAI-compatible endpoint.
///
/// Calls `GET {base_url}/models`. This is a config-time helper (used by the
/// setup wizard and `egg model`), so it takes raw credentials rather than a
/// built client. Returns a sorted, de-duplicated list of model IDs.
pub async fn list_models(base_url: &str, api_key: &str) -> Result<Vec<String>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")?;

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..=2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(400 * (1 << (attempt - 1)))).await;
        }
        match fetch_models_once(&client, &url, api_key).await {
            Ok(ids) => return Ok(ids),
            Err(e) => {
                if !is_retryable(&e) {
                    return Err(e);
                }
                log::warn!("list_models: attempt {} failed: {e:#}", attempt + 1);
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("list_models failed after 3 tries")))
}

async fn fetch_models_once(client: &reqwest::Client, url: &str, api_key: &str) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct ModelsResponse {
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
    }

    let resp = client
        .get(url)
        .bearer_auth(api_key)
        .send()
        .await
        .context("failed to request model list")?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .context("failed to read model list response")?;

    if !status.is_success() {
        bail!("model list request returned {status}: {text}");
    }

    let parsed: ModelsResponse = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse model list: {text}"))?;

    let mut ids: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tools: &'a [ToolDef],
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a str>,
    stream: bool,
}

// --- Streaming chunk shapes (a subset of the OpenAI chunk schema) ---

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Delta,
}

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

/// A tool call arrives in fragments across chunks, keyed by `index`.
#[derive(Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Accumulator that reassembles a full message from streamed deltas.
#[derive(Default)]
struct Accumulator {
    content: String,
    reasoning: String,
    /// Tool calls being built up, indexed by their `index` field.
    tool_calls: Vec<PartialToolCall>,
}

#[derive(Default, Clone)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl Accumulator {
    fn apply(&mut self, delta: Delta, events: &UnboundedSender<StreamEvent>) {
        if let Some(r) = delta.reasoning_content {
            if !r.is_empty() {
                self.reasoning.push_str(&r);
                let _ = events.send(StreamEvent::Reasoning(r));
            }
        }
        if let Some(c) = delta.content {
            if !c.is_empty() {
                if self.content.is_empty() {
                    log::debug!(
                        "llm: first content token (reasoning_len={})",
                        self.reasoning.len()
                    );
                }
                self.content.push_str(&c);
                let _ = events.send(StreamEvent::Content(c));
            }
        }
        if let Some(calls) = delta.tool_calls {
            for tc in calls {
                if self.tool_calls.len() <= tc.index {
                    self.tool_calls
                        .resize(tc.index + 1, PartialToolCall::default());
                }
                let slot = &mut self.tool_calls[tc.index];
                if let Some(id) = tc.id {
                    slot.id = id;
                }
                if let Some(f) = tc.function {
                    if let Some(name) = f.name {
                        slot.name.push_str(&name);
                    }
                    if let Some(args) = f.arguments {
                        slot.arguments.push_str(&args);
                    }
                }
            }
        }
    }

    fn into_message(self) -> ChatMessage {
        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .into_iter()
            .filter(|t| !t.name.is_empty())
            .map(|t| ToolCall {
                id: t.id,
                kind: "function".to_string(),
                function: FunctionCall {
                    name: t.name,
                    arguments: t.arguments,
                },
            })
            .collect();

        ChatMessage {
            role: "assistant".to_string(),
            content: if self.content.is_empty() {
                None
            } else {
                Some(self.content)
            },
            reasoning_content: if self.reasoning.is_empty() {
                None
            } else {
                Some(self.reasoning)
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
        }
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    /// Stream one chat completion. Retries on pre-stream errors (connection
    /// refused, 5xx, 429) but NOT on in-stream stalls — a stalled stream means
    /// the server accepted the request and then stopped producing; retrying the
    /// same request would just hit the same slow/broken generation path.
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        events: &UnboundedSender<StreamEvent>,
    ) -> Result<ChatMessage> {
        const MAX_RETRIES: usize = 3;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let delay = Duration::from_millis(400 * (1 << (attempt - 1)));
                // Let the UI show what's happening.
                let _ = events.send(StreamEvent::Content(format!(
                    "\n\n[⟳ retry {attempt}/{MAX_RETRIES} in {}ms…]\n",
                    delay.as_millis()
                )));
                tokio::time::sleep(delay).await;
            }

            match self.chat_stream_once(messages, tools, events).await {
                Ok(msg) => {
                    if attempt > 0 {
                        log::info!("llm: succeeded on retry {attempt}");
                    }
                    return Ok(msg);
                }
                Err(e) => {
                    let is_stall = format!("{e:#}").to_lowercase().contains("stalled");
                    if is_stall {
                        // In-stream stall — retrying won't help.
                        log::warn!("llm: stream stalled, not retrying: {e:#}");
                        return Err(e);
                    }
                    if !is_retryable(&e) {
                        return Err(e);
                    }
                    log::warn!("llm: attempt {} failed: {e:#}", attempt + 1);
                }
            }
        }

        Err(anyhow::anyhow!(
            "LLM call failed after {MAX_RETRIES} retries"
        ))
    }
}

impl OpenAiClient {
    /// Single HTTP streaming call — no retry, no recovery. Wrapped by
    /// [`LlmClient::chat_stream`] which adds the retry loop.
    async fn chat_stream_once(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        events: &UnboundedSender<StreamEvent>,
    ) -> Result<ChatMessage> {
        let url = format!("{}/chat/completions", self.base_url());
        let model = self.current_model();
        let body = ChatRequest {
            model: &model,
            messages,
            tools,
            tool_choice: if tools.is_empty() { None } else { Some("auto") },
            stream: true,
        };

        // Cap the initial POST (connect + TLS + response headers) at 30 s.
        // The reqwest Client timeout is a coarse total-request cap; this gives
        // us a tighter bound on the round-trip before streaming even starts.
        let req = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key())
            .json(&body)
            .send();

        let resp = tokio::time::timeout(Duration::from_secs(30), req)
            .await
            .context("LLM API request timed out before response headers")?
            .context("failed to send request to LLM API")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("LLM API returned {status}: {text}");
        }

        let mut acc = Accumulator::default();
        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        let mut first_chunk = true;

        loop {
            let deadline = if first_chunk {
                Self::FIRST_CHUNK_TIMEOUT
            } else {
                Self::NEXT_CHUNK_TIMEOUT
            };

            let chunk = match tokio::time::timeout(deadline, stream.next()).await {
                Ok(Some(Ok(bytes))) => {
                    first_chunk = false;
                    bytes
                }
                Ok(Some(Err(e))) => {
                    return Err(e).context("error reading response stream");
                }
                Ok(None) => break,
                Err(_elapsed) => {
                    let label = if first_chunk { "first token" } else { "stream" };
                    bail!("{label} stalled — no data for {} s", deadline.as_secs());
                }
            };

            buf.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete lines; keep any trailing partial line in `buf`.
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim_end_matches('\r').to_string();
                buf.drain(..=nl);

                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload.is_empty() {
                    continue;
                }
                if payload == "[DONE]" {
                    let msg = acc.into_message();
                    log::debug!(
                        "llm: [DONE] reasoning_len={} content_len={} tool_calls={}",
                        msg.reasoning_content.as_deref().unwrap_or("").len(),
                        msg.content.as_deref().unwrap_or("").len(),
                        msg.tool_calls.as_ref().map_or(0, |v| v.len()),
                    );
                    return Ok(msg);
                }

                match serde_json::from_str::<StreamChunk>(payload) {
                    Ok(chunk) => {
                        if let Some(choice) = chunk.choices.into_iter().next() {
                            acc.apply(choice.delta, events);
                        }
                    }
                    Err(_) => continue,
                }
            }
        }

        // Stream ended without an explicit [DONE]; return what we have.
        let msg = acc.into_message();
        log::debug!(
            "llm: stream end (no [DONE]) reasoning_len={} content_len={}",
            msg.reasoning_content.as_deref().unwrap_or("").len(),
            msg.content.as_deref().unwrap_or("").len(),
        );
        Ok(msg)
    }
}

/// Whether an error is worth retrying. Retries on timeouts, connection issues,
/// server errors (5xx), rate limits (429), and unexpected stream termination.
/// Does NOT retry on client errors (4xx = bad key / bad request).
fn is_retryable(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    s.contains("timed out")
        || s.contains("timeout")
        || s.contains("stalled")       // per-chunk stall detection
        || s.contains("connection")
        || s.contains("500 ")
        || s.contains("502 ")
        || s.contains("503 ")
        || s.contains("504 ")
        || s.contains("429")
        || s.contains("broken pipe")
        || s.contains("reset by peer")
        || s.contains("eof")
        || s.contains("unexpected")
        || s.contains("stream closed")
        || s.contains("incomplete")
}
