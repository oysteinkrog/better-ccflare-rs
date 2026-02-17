//! OpenAI SSE stream → Anthropic SSE stream transformation.
//!
//! Converts OpenAI Chat Completions streaming format to Anthropic Messages
//! streaming format. This is a **stateful** state machine that accumulates
//! tool call fragments and emits properly structured Anthropic events.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::openai_format::map_finish_reason;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum accumulated length for tool call arguments (safety limit).
const MAX_TOOL_CALL_ARGS_LEN: usize = 1_000_000;

/// Maximum tool call index (safety limit).
const MAX_TOOL_CALL_INDEX: usize = 100;

// ---------------------------------------------------------------------------
// Stream context (state machine)
// ---------------------------------------------------------------------------

/// State machine context for transforming an OpenAI SSE stream into Anthropic
/// SSE format.
///
/// Create one per stream. Feed chunks via [`transform_chunk`], then call
/// [`finish`] to emit final events.
pub struct OpenAiStreamContext {
    /// Incomplete line buffer (SSE lines split across chunks).
    buffer: String,
    /// Whether the first event has been processed.
    has_started: bool,
    /// Whether `message_start` has been emitted.
    has_sent_start: bool,
    /// Whether `content_block_start` for text has been emitted.
    has_sent_content_block_start: bool,
    /// Model name extracted from the first chunk.
    model: String,
    /// Whether we've encountered any tool call.
    has_tool_calls: bool,
    /// Per-tool-call-index accumulated argument strings.
    tool_accumulators: HashMap<usize, ToolCallAccumulator>,
    /// Running prompt token count.
    prompt_tokens: i64,
    /// Running completion token count.
    completion_tokens: i64,
    /// Current content block index (for Anthropic indexing).
    block_index: usize,
    /// Finish reason from the last chunk.
    finish_reason: Option<String>,
}

/// Accumulated state for a single tool call (streamed across multiple deltas).
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
    /// Whether content_block_start has been emitted for this tool.
    started: bool,
    /// Anthropic content block index.
    block_index: usize,
}

impl OpenAiStreamContext {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            has_started: false,
            has_sent_start: false,
            has_sent_content_block_start: false,
            model: String::new(),
            has_tool_calls: false,
            tool_accumulators: HashMap::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            block_index: 0,
            finish_reason: None,
        }
    }

    /// Transform a raw SSE chunk (may contain multiple lines or partial lines)
    /// into zero or more Anthropic SSE events.
    ///
    /// Returns a `Vec<String>` of complete SSE events (each prefixed with
    /// `event:` and `data:` lines, ready to send to the client).
    pub fn transform_chunk(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);

        let mut events = Vec::new();

        // Process complete lines — use cursor to avoid O(n^2) buffer copies.
        // Line must be copied into `line` because process_line borrows &mut self.
        let mut cursor = 0;
        loop {
            let remaining = &self.buffer[cursor..];
            let Some(pos) = remaining.find('\n') else {
                break;
            };
            let line = remaining[..pos].trim_end_matches('\r').to_string();
            cursor += pos + 1;

            if let Some(sse_events) = self.process_line(&line) {
                events.extend(sse_events);
            }
        }
        // Compact buffer in-place (no allocation vs previous to_string())
        if cursor > 0 {
            self.buffer.drain(..cursor);
        }

        events
    }

    /// Finish the stream — emit final close events.
    pub fn finish(&mut self) -> Vec<String> {
        let mut events = Vec::new();

        // Close any open content block
        if self.has_sent_content_block_start {
            events.push(sse_event(
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": self.current_text_index()}),
            ));
        }

        // Close any open tool call blocks
        for acc in self.tool_accumulators.values() {
            if acc.started {
                events.push(sse_event(
                    "content_block_stop",
                    &json!({"type": "content_block_stop", "index": acc.block_index}),
                ));
            }
        }

        // message_delta with stop_reason
        let stop_reason = map_finish_reason(self.finish_reason.as_deref());
        events.push(sse_event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {"output_tokens": self.completion_tokens},
            }),
        ));

        // message_stop
        events.push(sse_event("message_stop", &json!({"type": "message_stop"})));

        events
    }

    /// Process a single SSE line.
    fn process_line(&mut self, line: &str) -> Option<Vec<String>> {
        // Skip empty lines and comments
        if line.is_empty() || line.starts_with(':') {
            return None;
        }

        // Extract data from "data: ..." lines
        let data = line.strip_prefix("data: ")?;

        // Handle [DONE] sentinel
        if data == "[DONE]" {
            return Some(self.finish());
        }

        // Parse JSON
        let json: Value = serde_json::from_str(data).ok()?;

        let mut events = Vec::new();

        // Extract model from first chunk
        if !self.has_started {
            self.has_started = true;
            if let Some(model) = json.get("model").and_then(|v| v.as_str()) {
                self.model = model.to_string();
            }
        }

        // Emit message_start on first chunk
        if !self.has_sent_start {
            self.has_sent_start = true;
            events.push(sse_event(
                "message_start",
                &json!({
                    "type": "message_start",
                    "message": {
                        "id": json.get("id").cloned().unwrap_or(json!("msg_unknown")),
                        "type": "message",
                        "role": "assistant",
                        "model": self.model,
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {"input_tokens": self.prompt_tokens, "output_tokens": 0},
                    }
                }),
            ));
            // Ping event
            events.push(sse_event("ping", &json!({"type": "ping"})));
        }

        // Extract usage
        if let Some(usage) = json.get("usage") {
            if let Some(pt) = usage.get("prompt_tokens").and_then(|v| v.as_i64()) {
                self.prompt_tokens = pt;
            }
            if let Some(ct) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
                self.completion_tokens = ct;
            }
        }

        // Process choices
        let choices = json.get("choices").and_then(|v| v.as_array());
        if let Some(choices) = choices {
            for choice in choices {
                // Capture finish_reason
                if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                    self.finish_reason = Some(reason.to_string());
                }

                let delta = match choice.get("delta") {
                    Some(d) => d,
                    None => continue,
                };

                // Text content delta
                if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                    if !content.is_empty() {
                        // Emit content_block_start if not sent yet
                        if !self.has_sent_content_block_start {
                            self.has_sent_content_block_start = true;
                            events.push(sse_event(
                                "content_block_start",
                                &json!({
                                    "type": "content_block_start",
                                    "index": self.block_index,
                                    "content_block": {"type": "text", "text": ""},
                                }),
                            ));
                        }

                        events.push(sse_event(
                            "content_block_delta",
                            &json!({
                                "type": "content_block_delta",
                                "index": self.current_text_index(),
                                "delta": {"type": "text_delta", "text": content},
                            }),
                        ));
                    }
                }

                // Tool call deltas
                if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        let tc_events = self.process_tool_call_delta(tc);
                        events.extend(tc_events);
                    }
                }
            }
        }

        if events.is_empty() {
            None
        } else {
            Some(events)
        }
    }

    /// Process a tool call delta event.
    fn process_tool_call_delta(&mut self, tc: &Value) -> Vec<String> {
        let mut events = Vec::new();

        let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if index > MAX_TOOL_CALL_INDEX {
            return events;
        }

        if !self.has_tool_calls {
            self.has_tool_calls = true;
            // Close text content block if open
            if self.has_sent_content_block_start {
                events.push(sse_event(
                    "content_block_stop",
                    &json!({"type": "content_block_stop", "index": self.current_text_index()}),
                ));
                self.block_index += 1;
                self.has_sent_content_block_start = false;
            }
        }

        let acc = self.tool_accumulators.entry(index).or_insert_with(|| {
            let bi = self.block_index + index;
            ToolCallAccumulator {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
                started: false,
                block_index: bi,
            }
        });

        // Update id
        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
            acc.id = id.to_string();
        }

        // Update function name
        if let Some(func) = tc.get("function") {
            if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                acc.name = name.to_string();
            }

            // Accumulate arguments
            if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                if acc.arguments.len() + args.len() <= MAX_TOOL_CALL_ARGS_LEN {
                    acc.arguments.push_str(args);
                }
            }
        }

        // Emit content_block_start for this tool if not yet sent
        if !acc.started && !acc.name.is_empty() {
            acc.started = true;
            let block_index = acc.block_index;
            let id = acc.id.clone();
            let name = acc.name.clone();
            events.push(sse_event(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": block_index,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {},
                    },
                }),
            ));
        }

        // Emit input_json_delta for new argument chars
        if let Some(func) = tc.get("function") {
            if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                if !args.is_empty() && acc.started {
                    let block_index = acc.block_index;
                    events.push(sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": block_index,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": args,
                            },
                        }),
                    ));
                }
            }
        }

        events
    }

    /// Get the current text block index (always 0 for the first text block).
    fn current_text_index(&self) -> usize {
        0 // Text block is always index 0
    }
}

impl Default for OpenAiStreamContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format an SSE event with event type and JSON data.
fn sse_event(event_type: &str, data: &Value) -> String {
    format!(
        "event: {event_type}\ndata: {}\n\n",
        serde_json::to_string(data).unwrap_or_default()
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_stream_basic() {
        let mut ctx = OpenAiStreamContext::new();

        let events = ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
        );
        // Should emit message_start + ping
        assert!(events.iter().any(|e| e.contains("message_start")));
        assert!(events.iter().any(|e| e.contains("ping")));

        let events = ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
        );
        assert!(events.iter().any(|e| e.contains("content_block_start")));
        assert!(events.iter().any(|e| e.contains("text_delta")));
        assert!(events.iter().any(|e| e.contains("Hello")));

        let events = ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
        );
        assert!(events.iter().any(|e| e.contains("text_delta")));
        assert!(events.iter().any(|e| e.contains(" world")));

        let events = ctx.transform_chunk("data: [DONE]\n\n");
        assert!(events.iter().any(|e| e.contains("content_block_stop")));
        assert!(events.iter().any(|e| e.contains("message_delta")));
        assert!(events.iter().any(|e| e.contains("message_stop")));
    }

    #[test]
    fn tool_call_stream() {
        let mut ctx = OpenAiStreamContext::new();

        // First chunk: message start
        ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
        );

        // Tool call start
        let events = ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"search\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        );
        assert!(events.iter().any(|e| e.contains("content_block_start")));
        assert!(events.iter().any(|e| e.contains("tool_use")));
        assert!(events.iter().any(|e| e.contains("search")));

        // Tool call arguments (streamed)
        let events = ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"q\"}}]},\"finish_reason\":null}]}\n\n",
        );
        assert!(events.iter().any(|e| e.contains("input_json_delta")));

        let events = ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"uery\\\":\\\"hi\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        );
        assert!(events.iter().any(|e| e.contains("input_json_delta")));

        // Done
        let events = ctx.transform_chunk("data: [DONE]\n\n");
        assert!(events.iter().any(|e| e.contains("content_block_stop")));
        assert!(events.iter().any(|e| e.contains("message_stop")));
    }

    #[test]
    fn partial_line_buffering() {
        let mut ctx = OpenAiStreamContext::new();

        // Send partial line
        let events = ctx.transform_chunk("data: {\"id\":\"chat-1\",\"model\":");
        assert!(events.is_empty());

        // Complete the line
        let events = ctx.transform_chunk(
            "\"gpt-4\",\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
        );
        assert!(events.iter().any(|e| e.contains("message_start")));
    }

    #[test]
    fn usage_extraction() {
        let mut ctx = OpenAiStreamContext::new();

        // Start
        ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
        );

        // Content
        ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n",
        );

        // Usage chunk (often last before [DONE])
        ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[],\"usage\":{\"prompt_tokens\":25,\"completion_tokens\":10}}\n\n",
        );

        let events = ctx.transform_chunk("data: [DONE]\n\n");
        // message_delta should contain output_tokens
        let delta_event = events.iter().find(|e| e.contains("message_delta")).unwrap();
        assert!(delta_event.contains("\"output_tokens\":10"));
    }

    #[test]
    fn finish_reason_in_stream() {
        let mut ctx = OpenAiStreamContext::new();

        ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
        );
        ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n",
        );
        ctx.transform_chunk(
            "data: {\"id\":\"chat-1\",\"model\":\"gpt-4\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );

        let events = ctx.transform_chunk("data: [DONE]\n\n");
        let delta = events.iter().find(|e| e.contains("message_delta")).unwrap();
        assert!(delta.contains("end_turn"));
    }

    #[test]
    fn sse_event_format() {
        let event = sse_event("ping", &json!({"type": "ping"}));
        assert_eq!(event, "event: ping\ndata: {\"type\":\"ping\"}\n\n");
    }
}
