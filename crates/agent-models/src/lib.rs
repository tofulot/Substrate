use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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
    ) -> Result<Vec<Value>>;
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
            let end = self
                .bytes
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|p| (p, 2))
                .or_else(|| {
                    self.bytes
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|p| (p, 4))
                });
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
        let request=self.client.post(format!("{}/chat/completions",self.base_url)).bearer_auth(&self.key)
            .json(&json!({"model":model,"messages":chat_messages(instructions,input),"tools":tools,"stream":true,"store":false,"max_tokens":16384,"reasoning_effort":"medium"})).send();
        let response = tokio::select! {_=cancel.cancelled()=>bail!("Interrupted"),r=request=>r.context("Could not reach Subquadratic")?};
        let status = response.status();
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
        loop {
            let next =
                tokio::select! {_=cancel.cancelled()=>bail!("Interrupted"),n=stream.next()=>n};
            let Some(chunk) = next else {
                break;
            };
            for event in parser.push(&chunk?)? {
                if event["done"] == true {
                    ensure!(finished, "Provider stream ended before a finish reason");
                    break;
                }
                ensure!(
                    event.get("error").is_none(),
                    "Subquadratic stream returned an error"
                );
                let choice = &event["choices"][0];
                let delta = &choice["delta"];
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
                    ensure!(
                        matches!(reason, "stop" | "tool_calls"),
                        "Model stopped with {reason}; output may have reached its token limit"
                    );
                    finished = true;
                }
            }
            if finished {
                break;
            }
        }
        ensure!(finished, "Provider stream ended unexpectedly");
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
