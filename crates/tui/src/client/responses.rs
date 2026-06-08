//! OpenAI Responses API bridge for the OpenAI Codex / ChatGPT provider.
//!
//! Implements a dedicated Responses API client that maps CodeWhale's internal
//! message/tool types to the Responses wire format and parses streaming SSE
//! events back into CodeWhale's `StreamEvent` / `MessageResponse` types.
//!
//! This is intentionally separate from the Chat Completions path
//! (`client/chat.rs`) to avoid protocol hacks.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::llm_client::StreamEventBox;
use crate::logging;
use crate::models::{
    ContentBlock, ContentBlockStart, Delta, MessageDelta, MessageRequest, MessageResponse,
    StreamEvent, Tool, Usage,
};

use super::{DeepSeekClient, ERROR_BODY_MAX_BYTES, bounded_error_text, system_to_instructions};

/// Base URL path for the Codex Responses endpoint.
const CODEX_RESPONSES_PATH: &str = "/codex/responses";

impl DeepSeekClient {
    /// Build the Responses API request body from a `MessageRequest`.
    fn build_responses_body(&self, request: &MessageRequest) -> Value {
        let model = &request.model;
        let mut body = json!({
            "model": model,
            "stream": true,
            "store": false,
        });

        // Instructions (system prompt).
        if let Some(instructions) = system_to_instructions(request.system.clone()) {
            body["instructions"] = json!(instructions);
        }

        // Convert messages to Responses input items.
        let input = convert_messages_to_responses_input(request);
        body["input"] = json!(input);

        // Convert tools to Responses function tools.
        if let Some(tools) = request.tools.as_ref() {
            let responses_tools: Vec<Value> =
                tools.iter().map(tool_to_responses_function).collect();
            if !responses_tools.is_empty() {
                body["tools"] = json!(responses_tools);
                body["tool_choice"] = json!("auto");
                body["parallel_tool_calls"] = json!(true);
            }
        }

        // Reasoning configuration.
        if let Some(effort) = request.reasoning_effort.as_deref() {
            let summary = match effort {
                "off" | "disabled" | "none" | "false" => "off",
                _ => "auto",
            };
            if summary != "off" {
                body["reasoning"] = json!({
                    "effort": effort,
                    "summary": summary,
                });
            }
        }

        // Include reasoning summaries in the stream.
        body["include"] = json!(["reasoning.encrypted_content"]);

        body
    }

    /// Handle a streaming Responses API request for the OpenAI Codex provider.
    pub(super) async fn handle_responses_stream(
        &self,
        request: MessageRequest,
    ) -> Result<StreamEventBox> {
        let body = self.build_responses_body(&request);
        let url = format!("{}{}", self.base_url, CODEX_RESPONSES_PATH);

        // The bearer Authorization header is already installed as a default
        // header on `http_client` (resolved from the Codex OAuth access token),
        // so it must not be set again here or it would be duplicated. The
        // ChatGPT backend additionally requires the account id and the
        // experimental Responses beta opt-in.
        let mut builder = self
            .http_client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs");
        if let Some(account_id) = crate::oauth::codex_account_id() {
            builder = builder.header("chatgpt-account-id", account_id);
        }

        let response = builder
            .json(&body)
            .send()
            .await
            .context("Responses API request failed")?;

        let status = response.status();
        if !status.is_success() {
            let raw = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            anyhow::bail!("Responses API error (HTTP {status}): {raw}");
        }

        let stream_idle_timeout = self.stream_idle_timeout;
        let byte_stream = response.bytes_stream();

        let stream = async_stream::stream! {
            use futures_util::StreamExt;

            // Emit synthetic MessageStart.
            yield Ok(StreamEvent::MessageStart {
                message: MessageResponse {
                    id: String::new(),
                    r#type: "message".to_string(),
                    role: "assistant".to_string(),
                    content: vec![],
                    model: request.model.clone(),
                    stop_reason: None,
                    stop_sequence: None,
                    container: None,
                    usage: Usage::default(),
                },
            });

            let mut _response_id = String::new();
            let mut _current_item_type: Option<String> = None;
            let mut current_block_index: Option<u32> = None;
            let mut saw_tool_call = false;
            let mut _output_text = String::new();
            let mut _thinking_text = String::new();
            let mut usage_data: Option<Usage> = None;
            let mut buffer = String::new();
            let mut done = false;
            let mut content_block_counter: u32 = 0;

            tokio::pin!(byte_stream);

            while !done {
                let chunk = match tokio::time::timeout(stream_idle_timeout, byte_stream.next()).await {
                    Ok(Some(Ok(chunk))) => chunk,
                    Ok(Some(Err(e))) => {
                        yield Err(anyhow::anyhow!("Stream read error: {e}"));
                        return;
                    }
                    Ok(None) => break,
                    Err(_) => {
                        yield Err(anyhow::anyhow!("Stream idle timeout"));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Process complete SSE lines.
                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim().to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() || line.starts_with(':') {
                        continue;
                    }

                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            done = true;
                            break;
                        }

                        let event: Value = match serde_json::from_str(data) {
                            Ok(v) => v,
                            Err(e) => {
                                logging::warn(format!(
                                    "Failed to parse Responses SSE event: {e}"
                                ));
                                continue;
                            }
                        };

                        let event_type =
                            event.get("type").and_then(|t| t.as_str()).unwrap_or("");

                        match event_type {
                            "response.created" => {
                                if let Some(resp) = event.get("response") {
                                    _response_id = resp
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                }
                            }
                            "response.output_item.added" => {
                                if let Some(item) = event.get("item") {
                                    let item_type = item
                                        .get("type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    _current_item_type = Some(item_type.to_string());

                                    match item_type {
                                        "message" => {
                                            content_block_counter += 1;
                                            yield Ok(StreamEvent::ContentBlockStart {
                                                index: content_block_counter - 1,
                                                content_block: ContentBlockStart::Text {
                                                    text: String::new(),
                                                },
                                            });
                                            current_block_index =
                                                Some(content_block_counter - 1);
                                        }
                                        "function_call" => {
                                            let call_id = item
                                                .get("call_id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let item_id = item
                                                .get("id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let name = item
                                                .get("name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            saw_tool_call = true;
                                            // call_id and item_id are folded
                                            // into a composite tool-use id so
                                            // the function_call_output can be
                                            // routed back to the right call.
                                            let composite_id =
                                                format!("{call_id}|{item_id}");
                                            content_block_counter += 1;
                                            yield Ok(StreamEvent::ContentBlockStart {
                                                index: content_block_counter - 1,
                                                content_block:
                                                    ContentBlockStart::ToolUse {
                                                        id: composite_id,
                                                        name,
                                                        input: json!({}),
                                                        caller: None,
                                                    },
                                            });
                                            current_block_index =
                                                Some(content_block_counter - 1);
                                        }
                                        "reasoning" => {
                                            content_block_counter += 1;
                                            yield Ok(StreamEvent::ContentBlockStart {
                                                index: content_block_counter - 1,
                                                content_block:
                                                    ContentBlockStart::Thinking {
                                                        thinking: String::new(),
                                                    },
                                            });
                                            current_block_index =
                                                Some(content_block_counter - 1);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            "response.output_text.delta" => {
                                if let Some(delta_text) =
                                    event.get("delta").and_then(|d| d.as_str())
                                {
                                    _output_text.push_str(delta_text);
                                    if let Some(idx) = current_block_index {
                                        yield Ok(StreamEvent::ContentBlockDelta {
                                            index: idx,
                                            delta: Delta::TextDelta {
                                                text: delta_text.to_string(),
                                            },
                                        });
                                    }
                                }
                            }
                            "response.function_call_arguments.delta" => {
                                if let Some(delta_text) =
                                    event.get("delta").and_then(|d| d.as_str())
                                {
                                    if let Some(idx) = current_block_index {
                                        yield Ok(StreamEvent::ContentBlockDelta {
                                            index: idx,
                                            delta: Delta::InputJsonDelta {
                                                partial_json: delta_text.to_string(),
                                            },
                                        });
                                    }
                                }
                            }
                            "response.reasoning_summary_text.delta"
                            | "response.reasoning_text.delta" => {
                                if let Some(delta_text) =
                                    event.get("delta").and_then(|d| d.as_str())
                                {
                                    _thinking_text.push_str(delta_text);
                                    if let Some(idx) = current_block_index {
                                        yield Ok(StreamEvent::ContentBlockDelta {
                                            index: idx,
                                            delta: Delta::ThinkingDelta {
                                                thinking: delta_text.to_string(),
                                            },
                                        });
                                    }
                                }
                            }
                            "response.output_item.done" => {
                                if let Some(idx) = current_block_index {
                                    yield Ok(StreamEvent::ContentBlockStop { index: idx });
                                    current_block_index = None;
                                    _current_item_type = None;
                                }
                            }
                            "response.completed" => {
                                if let Some(resp) = event.get("response") {
                                    if let Some(usage_val) = resp.get("usage") {
                                        usage_data =
                                            Some(parse_responses_usage(usage_val));
                                    }
                                    let status = resp
                                        .get("status")
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("completed");
                                    let stop_reason = match status {
                                        "completed" => {
                                            if saw_tool_call {
                                                "tool_use"
                                            } else {
                                                "end_turn"
                                            }
                                        }
                                        "incomplete" => "max_tokens",
                                        _ => "end_turn",
                                    };
                                    yield Ok(StreamEvent::MessageDelta {
                                        delta: MessageDelta {
                                            stop_reason: Some(stop_reason.to_string()),
                                            stop_sequence: None,
                                        },
                                        usage: usage_data.take(),
                                    });
                                }
                            }
                            "error" => {
                                let msg = event
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("Unknown error");
                                let code = event
                                    .get("code")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("unknown");
                                yield Err(anyhow::anyhow!(
                                    "Responses API error [{code}]: {msg}"
                                ));
                                return;
                            }
                            _ => {
                                // Ignore unknown event types.
                            }
                        }
                    }
                }
            }

            // Emit MessageStop.
            yield Ok(StreamEvent::MessageStop);
        };

        Ok(Box::pin(stream))
    }
}

/// Convert CodeWhale messages to Responses API input items.
fn convert_messages_to_responses_input(request: &MessageRequest) -> Vec<Value> {
    let mut items = Vec::new();

    for msg in &request.messages {
        match msg.role.as_str() {
            "user" => {
                let mut content_items = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            content_items.push(json!({
                                "type": "input_text",
                                "text": text,
                            }));
                        }
                        ContentBlock::ImageUrl { image_url } => {
                            content_items.push(json!({
                                "type": "input_image",
                                "image_url": image_url.url,
                            }));
                        }
                        _ => {}
                    }
                }
                if !content_items.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": content_items,
                    }));
                }
            }
            "assistant" => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            items.push(json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": text,
                                }],
                            }));
                        }
                        ContentBlock::ToolUse { id, name, input, .. } => {
                            let (call_id, _item_id) = parse_tool_use_id(id);
                            items.push(json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": name,
                                "arguments": serde_json::to_string(input).unwrap_or_default(),
                            }));
                        }
                        ContentBlock::Thinking { thinking } => {
                            items.push(json!({
                                "type": "reasoning",
                                "summary": [{
                                    "type": "summary_text",
                                    "text": thinking,
                                }],
                            }));
                        }
                        _ => {}
                    }
                }
            }
            "tool" => {
                for block in &msg.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = block
                    {
                        let (call_id, _item_id) = parse_tool_use_id(tool_use_id);
                        items.push(json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": content,
                        }));
                    }
                }
            }
            _ => {}
        }
    }

    items
}

/// Convert a CodeWhale tool definition to a Responses API function tool.
fn tool_to_responses_function(tool: &Tool) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
        "strict": false,
    })
}

/// Parse a composite tool_use_id back to (call_id, item_id).
/// Composite format: "call_id|item_id"
fn parse_tool_use_id(id: &str) -> (String, String) {
    if let Some(pipe_pos) = id.find('|') {
        (
            id[..pipe_pos].to_string(),
            id[pipe_pos + 1..].to_string(),
        )
    } else {
        (id.to_string(), String::new())
    }
}

/// Parse usage from a Responses API usage object.
fn parse_responses_usage(val: &Value) -> Usage {
    let input = val
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let output = val
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cached = val
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    Usage {
        input_tokens: input,
        output_tokens: output,
        prompt_cache_hit_tokens: if cached > 0 { Some(cached) } else { None },
        prompt_cache_miss_tokens: None,
        reasoning_tokens: None,
        reasoning_replay_tokens: None,
        server_tool_use: None,
    }
}
