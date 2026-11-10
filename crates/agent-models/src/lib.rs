use agent_protocol::{ModelMetrics, TokenUsage};
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
pub struct ModelResponse {
    pub output: Result<Vec<Value>>,
    pub metrics: ModelMetrics,
}

pub const BASE_URL: &str = "https://api.subq.ai/v1";
pub const DEFAULT_MODEL: &str = "subq-preview";
#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn respond(
        &self,
        model: &str,
        instructions: &str,
        input: &[Value],
        tools: Vec<Value>,
        cancel: &CancellationToken,
        deltas: mpsc::Sender<String>,
    ) -> ModelResponse;
}
pub struct Subquadratic {
    client: reqwest::Client,
    key: String,
    base_url: String,
}
impl Subquadratic {
    pub fn new(key: String) -> Result<Self> {
        // The override is a process-level developer/testing option, never renderer-controlled.
        let base_url = std::env::var("SUBSTRATE_API_BASE").unwrap_or_else(|_| BASE_URL.into());
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(20))
                .timeout(std::time::Duration::from_secs(600))
                .build()?,
            key,
            base_url,
        })
    }
    pub async fn models(&self) -> Result<Value> {
        ensure!(
            !self.key.is_empty(),
            "Add your Subquadratic API key in Settings"
        );
        let response = self
            .client
            .get(format!("{}/models", self.base_url))
            .bearer_auth(&self.key)
            .send()
            .await?;
        let status = response.status();
        ensure!(
            status.is_success(),
            "Subquadratic model listing failed (HTTP {status})"
        );
        let result: Value = response.json().await?;
        Ok(result["data"].clone())
    }
}
pub fn parse_usage(usage: &Value) -> TokenUsage {
    TokenUsage {
        input_tokens: usage["prompt_tokens"]
            .as_u64()
            .or_else(|| usage["input_tokens"].as_u64()),
        output_tokens: usage["completion_tokens"]
            .as_u64()
            .or_else(|| usage["output_tokens"].as_u64()),
        total_tokens: usage["total_tokens"].as_u64(),
        cached_tokens: usage["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .or_else(|| usage["input_tokens_details"]["cached_tokens"].as_u64()),
        reasoning_tokens: usage["completion_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .or_else(|| usage["output_tokens_details"]["reasoning_tokens"].as_u64()),
    }
}
// Buffer bytes, not lossy UTF-8 chunks: network packets may split Unicode characters.
#[derive(Default)]
struct Sse {
    bytes: Vec<u8>,
}
impl Sse {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>> {
        self.bytes.extend_from_slice(chunk);
        ensure!(self.bytes.len() < 8 * 1024 * 1024, "SSE frame too large");
        let mut out = vec![];
        loop {
            let lf = self
                .bytes
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|p| (p, 2));
            let crlf = self
                .bytes
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| (p, 4));
            let end = lf.into_iter().chain(crlf).min_by_key(|(p, _)| *p);
            let Some((index, len)) = end else {
                break;
            };
            let frame = String::from_utf8(self.bytes.drain(..index + len).collect())?;
            let data = frame
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>()
                .join("\n");
            if data == "[DONE]" {
                out.push(json!({"done":true}));
            } else if !data.is_empty() {
                out.push(serde_json::from_str(&data)?);
            }
        }
        Ok(out)
    }
}
// The harness owns history locally. Translate its provider-neutral items into the
// documented stateless Chat Completions contract (Responses requires server-side IDs).
fn chat_messages(instructions: &str, input: &[Value]) -> Vec<Value> {
    let mut messages = vec![json!({"role":"system","content":instructions})];
    let mut assistant: Option<Value> = None;
    for item in input {
        match item["type"].as_str().unwrap_or("") {
            "message" => {
                if let Some(a) = assistant.take() {
                    messages.push(a);
                }
                let text = item["content"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                assistant = Some(json!({"role":"assistant","content":text}));
            }
            "function_call" => {
                let a = assistant.get_or_insert_with(|| json!({"role":"assistant","content":null}));
                if a.get("tool_calls").is_none() {
                    a["tool_calls"] = json!([]);
                }
                a["tool_calls"].as_array_mut().unwrap().push(json!({"id":item["call_id"],"type":"function","function":{"name":item["name"],"arguments":item["arguments"]}}));
            }
            "function_call_output" => {
                if let Some(a) = assistant.take() {
                    messages.push(a);
                }
                messages.push(
                    json!({"role":"tool","tool_call_id":item["call_id"],"content":item["output"]}),
                );
            }
            _ => {
                if let Some(a) = assistant.take() {
                    messages.push(a);
                }
                messages.push(item.clone());
            }
        }
    }
    if let Some(a) = assistant {
        messages.push(a);
    }
    messages
}
#[async_trait]
impl ModelProvider for Subquadratic {
    async fn respond(
        &self,
        model: &str,
        instructions: &str,
        input: &[Value],
        tools: Vec<Value>,
        cancel: &CancellationToken,
        deltas: mpsc::Sender<String>,
    ) -> ModelResponse {
        let start = Instant::now();
        let mut metrics = ModelMetrics::default();
        let output = self
            .stream(
                model,
                instructions,
                input,
                tools,
                cancel,
                deltas,
                &mut metrics,
                start,
            )
            .await;
        metrics.duration_ms = Some(start.elapsed().as_millis() as u64);
        ModelResponse { output, metrics }
    }
}
impl Subquadratic {
    #[allow(clippy::too_many_arguments)]
    async fn stream(
        &self,
        model: &str,
        instructions: &str,
        input: &[Value],
        tools: Vec<Value>,
        cancel: &CancellationToken,
        deltas: mpsc::Sender<String>,
        metrics: &mut ModelMetrics,
        start: Instant,
    ) -> Result<Vec<Value>> {
        ensure!(
            !self.key.is_empty(),
            "Add your Subquadratic API key in Settings"
        );
        let tools: Vec<_> = tools
            .into_iter()
            .map(|mut t| {
                t.as_object_mut().unwrap().remove("type");
                json!({"type":"function","function":t})
            })
            .collect();
        let payload = serde_json::to_vec(
            &json!({"model":model,"messages":chat_messages(instructions,input),"tools":tools,"stream":true,"store":false,"max_tokens":16384,"reasoning_effort":"medium"}),
        )?;
        metrics.request_bytes = Some(payload.len() as u64);
        let request = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload)
            .send();
        let response = tokio::select! {_=cancel.cancelled()=>bail!("Interrupted"),r=request=>r.context("Could not reach Subquadratic")?};
        let status = response.status();
        metrics.http_status = Some(status.as_u16());
        if !status.is_success() {
            bail!(
                "Subquadratic returned HTTP {}. {}",
                status.as_u16(),
                match status.as_u16() {
                    401 => "Check your API key.",
                    403 => "SubQ Preview requires early-access permission for your organization.",
                    429 => "Rate limit or quota reached; try again later.",
                    _ => "Check provider availability and your model configuration.",
                }
            );
        }
        let mut stream = response.bytes_stream();
        let mut parser = Sse::default();
        let mut calls = std::collections::BTreeMap::<u64, (String, String, String)>::new();
        let mut text = String::new();
        let mut finished = false;
        let mut done = false;
        loop {
            let next =
                tokio::select! {_=cancel.cancelled()=>bail!("Interrupted"),n=stream.next()=>n};
            let Some(chunk) = next else {
                break;
            };
            let chunk = chunk?;
            metrics.response_bytes = Some(metrics.response_bytes.unwrap_or(0) + chunk.len() as u64);
            for event in parser.push(&chunk)? {
                if event["usage"].is_object() {
                    metrics.usage = Some(parse_usage(&event["usage"]));
                }
                if event["done"] == true {
                    ensure!(finished, "Provider stream ended before a finish reason");
                    done = true;
                    break;
                }
                ensure!(
                    event.get("error").is_none_or(Value::is_null),
                    "Subquadratic stream returned an error"
                );
                let choice = &event["choices"][0];
                let delta = &choice["delta"];
                if metrics.first_output_ms.is_none()
                    && (delta["content"].as_str().is_some_and(|s| !s.is_empty())
                        || delta["tool_calls"]
                            .as_array()
                            .is_some_and(|a| !a.is_empty()))
                {
                    metrics.first_output_ms = Some(start.elapsed().as_millis() as u64);
                }
                if let Some(part) = delta["content"].as_str() {
                    text.push_str(part);
                    let _ = deltas.send(part.into()).await;
                }
                if let Some(parts) = delta["tool_calls"].as_array() {
                    for part in parts {
                        let index = part["index"].as_u64().context("Missing tool call index")?;
                        ensure!(index < 64, "Too many tool calls");
                        let entry = calls.entry(index).or_default();
                        if let Some(id) = part["id"].as_str() {
                            entry.0.push_str(id);
                        }
                        if let Some(name) = part["function"]["name"].as_str() {
                            entry.1.push_str(name);
                        }
                        if let Some(args) = part["function"]["arguments"].as_str() {
                            entry.2.push_str(args);
                        }
                        ensure!(entry.2.len() < 1024 * 1024, "Tool arguments exceed limit");
                    }
                }
                if let Some(reason) = choice["finish_reason"].as_str() {
                    // Read the trailer even for length/content-filter stops: it may
                    // contain the only provider usage report for this failed call.
                    metrics.finish_reason = Some(reason.into());
                    finished = true;
                }
            }
            if done {
                break;
            }
        }
        ensure!(finished, "Provider stream ended unexpectedly");
        let reason = metrics.finish_reason.as_deref().unwrap_or("unknown");
        ensure!(
            matches!(reason, "stop" | "tool_calls"),
            "Model stopped with {reason}; output may have reached its token limit"
        );
        let mut out = vec![];
        if !text.is_empty() {
            out.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}));
        }
        for (_, (id, name, args)) in calls {
            ensure!(!id.is_empty() && !name.is_empty(), "Incomplete tool call");
            out.push(json!({"type":"function_call","call_id":id,"name":name,"arguments":args}));
        }
        ensure!(
            !out.is_empty(),
            "Model returned an empty response; its output budget may have been spent on reasoning"
        );
        Ok(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_unicode_and_crlf() {
        let mut s = Sse::default();
        let bytes = "data: {\"delta\":\"世界\"}\r\n\r\n".as_bytes();
        let mut out = vec![];
        for b in bytes {
            out.extend(s.push(&[*b]).unwrap());
        }
        assert_eq!(out[0]["delta"], "世界");
    }
    #[test]
    fn multiple_events() {
        let mut s = Sse::default();
        assert_eq!(
            s.push(b"data: {}\n\ndata: {}\n\ndata: [DONE]\n\n")
                .unwrap()
                .len(),
            3
        );
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    #[test]
    fn preserves_zero_and_missing_provider_fields() {
        let usage = parse_usage(
            &json!({"prompt_tokens":123,"completion_tokens":0,"prompt_tokens_details":{"cached_tokens":0}}),
        );
        assert_eq!(usage.input_tokens, Some(123));
        assert_eq!(usage.output_tokens, Some(0));
        assert_eq!(usage.cached_tokens, Some(0));
        assert_eq!(usage.reasoning_tokens, None);
        assert_eq!(usage.total_tokens, None);
        assert!(parse_usage(&json!({})).input_tokens.is_none());
    }
    #[test]
    fn accepts_usage_only_events_after_finish_and_mixed_framing() {
        let mut parser = Sse::default();
        let events=parser.push(b"data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\r\n\r\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":4}}\n\ndata: [DONE]\n\n").unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(parse_usage(&events[1]["usage"]).input_tokens, Some(4));
        assert_eq!(events[2]["done"], true);
    }
}
