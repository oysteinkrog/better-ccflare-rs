//! Anthropic ↔ OpenAI message format translation.
//!
//! Converts between Anthropic Messages API format and OpenAI Chat Completions
//! format for request bodies and (non-streaming) response bodies.

use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Request: Anthropic → OpenAI
// ---------------------------------------------------------------------------

/// Convert an Anthropic Messages API request body into an OpenAI Chat
/// Completions request body.
///
/// Key transformations:
/// - `system` (top-level string) → first message with `role: "system"`
/// - `max_tokens` → `max_tokens`
/// - `stop_sequences` → `stop`
/// - Messages content blocks → flattened OpenAI messages
/// - Tools (Anthropic schema) → OpenAI function tools
pub fn anthropic_to_openai_request(body: &[u8]) -> Option<Vec<u8>> {
    let input: Value = serde_json::from_slice(body).ok()?;
    let obj = input.as_object()?;

    let mut oai: Map<String, Value> = Map::new();

    // Model
    if let Some(model) = obj.get("model") {
        oai.insert("model".into(), model.clone());
    }

    // Build messages array
    let mut messages: Vec<Value> = Vec::new();

    // System message (Anthropic top-level → OpenAI system role)
    if let Some(system) = obj.get("system") {
        if let Some(s) = system.as_str() {
            if !s.is_empty() {
                messages.push(json!({"role": "system", "content": s}));
            }
        } else if let Some(arr) = system.as_array() {
            // System can be an array of content blocks
            let text: String = arr
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                messages.push(json!({"role": "system", "content": text}));
            }
        }
    }

    // Convert Anthropic messages to OpenAI messages
    if let Some(msgs) = obj.get("messages").and_then(|v| v.as_array()) {
        for msg in msgs {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = msg.get("content");

            match content {
                // Simple string content
                Some(Value::String(text)) => {
                    messages.push(json!({"role": role, "content": text}));
                }
                // Array of content blocks
                Some(Value::Array(blocks)) => {
                    convert_content_blocks(&mut messages, role, blocks);
                }
                _ => {
                    // Pass through with null content (e.g. assistant with tool_calls only)
                    messages.push(json!({"role": role, "content": null}));
                }
            }
        }
    }

    oai.insert("messages".into(), Value::Array(messages));

    // max_tokens
    if let Some(mt) = obj.get("max_tokens") {
        oai.insert("max_tokens".into(), mt.clone());
    }

    // temperature, top_p — pass through
    for key in ["temperature", "top_p", "top_k"] {
        if let Some(v) = obj.get(key) {
            oai.insert(key.into(), v.clone());
        }
    }

    // stop_sequences → stop
    if let Some(stops) = obj.get("stop_sequences") {
        oai.insert("stop".into(), stops.clone());
    }

    // stream
    if let Some(stream) = obj.get("stream") {
        oai.insert("stream".into(), stream.clone());
        // Include usage in stream responses
        if stream.as_bool() == Some(true) {
            oai.insert("stream_options".into(), json!({"include_usage": true}));
        }
    }

    // Tools (Anthropic → OpenAI function format)
    if let Some(tools) = obj.get("tools").and_then(|v| v.as_array()) {
        let oai_tools: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?;
                let mut func = json!({
                    "type": "function",
                    "function": {
                        "name": name,
                    }
                });
                if let Some(desc) = tool.get("description") {
                    func["function"]["description"] = desc.clone();
                }
                if let Some(schema) = tool.get("input_schema") {
                    func["function"]["parameters"] = schema.clone();
                }
                Some(func)
            })
            .collect();
        if !oai_tools.is_empty() {
            oai.insert("tools".into(), Value::Array(oai_tools));
        }
    }

    serde_json::to_vec(&Value::Object(oai)).ok()
}

/// Convert Anthropic content blocks into OpenAI messages.
fn convert_content_blocks(messages: &mut Vec<Value>, role: &str, blocks: &[Value]) {
    // Collect text blocks and tool_use blocks separately
    let mut text_parts: Vec<&str> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut tool_results: Vec<(&str, &str)> = Vec::new(); // (tool_use_id, content)

    for block in blocks {
        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    text_parts.push(text);
                }
            }
            "tool_use" => {
                let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let input = block.get("input").cloned().unwrap_or(json!({}));
                let args = serde_json::to_string(&input).unwrap_or_default();

                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args,
                    }
                }));
            }
            "tool_result" => {
                let tool_use_id = block
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let content = block
                    .get("content")
                    .and_then(|v| {
                        // Content can be a string or array of blocks
                        v.as_str().or_else(|| {
                            v.as_array().and_then(|arr| {
                                arr.first()
                                    .and_then(|b| b.get("text"))
                                    .and_then(|t| t.as_str())
                            })
                        })
                    })
                    .unwrap_or("");
                tool_results.push((tool_use_id, content));
            }
            _ => {} // Ignore unknown block types
        }
    }

    // Emit assistant message with text + tool_calls
    if role == "assistant" {
        let content = if text_parts.is_empty() {
            Value::Null
        } else {
            Value::String(text_parts.join(""))
        };

        if tool_calls.is_empty() {
            messages.push(json!({"role": "assistant", "content": content}));
        } else {
            messages.push(json!({
                "role": "assistant",
                "content": content,
                "tool_calls": tool_calls,
            }));
        }
    } else if !tool_results.is_empty() {
        // Tool result messages (role "user" with tool_result blocks)
        for (tool_use_id, content) in tool_results {
            messages.push(json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": content,
            }));
        }
    } else {
        // User message with text content
        let content = text_parts.join("");
        messages.push(json!({"role": role, "content": content}));
    }
}

// ---------------------------------------------------------------------------
// Response: OpenAI → Anthropic (non-streaming)
// ---------------------------------------------------------------------------

/// Convert an OpenAI Chat Completions response body into an Anthropic Messages
/// API response body.
pub fn openai_to_anthropic_response(body: &[u8]) -> Option<Vec<u8>> {
    let input: Value = serde_json::from_slice(body).ok()?;
    let choice = input.get("choices")?.as_array()?.first()?;
    let message = choice.get("message")?;

    let mut content: Vec<Value> = Vec::new();

    // Text content
    if let Some(text) = message.get("content").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }

    // Tool calls → tool_use blocks
    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(func) = tc.get("function") {
                let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args_str = func
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

                content.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }));
            }
        }
    }

    // Map finish reason
    let stop_reason = match choice.get("finish_reason").and_then(|v| v.as_str()) {
        Some("stop") => "end_turn",
        Some("length") => "max_tokens",
        Some("function_call" | "tool_calls") => "tool_use",
        Some("content_filter") => "stop_sequence",
        _ => "end_turn",
    };

    // Usage
    let usage = input.get("usage").cloned().unwrap_or(json!({}));
    let model = input
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let response = json!({
        "id": input.get("id").cloned().unwrap_or(json!("msg_unknown")),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": usage.get("prompt_tokens").cloned().unwrap_or(json!(0)),
            "output_tokens": usage.get("completion_tokens").cloned().unwrap_or(json!(0)),
        },
    });

    serde_json::to_vec(&response).ok()
}

// ---------------------------------------------------------------------------
// Finish reason mapping
// ---------------------------------------------------------------------------

/// Map OpenAI finish_reason to Anthropic stop_reason.
pub fn map_finish_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("stop") => "end_turn",
        Some("length") => "max_tokens",
        Some("function_call" | "tool_calls") => "tool_use",
        Some("content_filter") => "stop_sequence",
        _ => "end_turn",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_request_translation() {
        let anthropic = br#"{
            "model": "claude-3-opus",
            "max_tokens": 100,
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        }"#;

        let result = anthropic_to_openai_request(anthropic).unwrap();
        let json: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(json["model"], "claude-3-opus");
        assert_eq!(json["max_tokens"], 100);

        let msgs = json["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "Hello");
    }

    #[test]
    fn content_blocks_translation() {
        let anthropic = br#"{
            "model": "gpt-4",
            "max_tokens": 100,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Hello "},
                        {"type": "text", "text": "World"}
                    ]
                }
            ]
        }"#;

        let result = anthropic_to_openai_request(anthropic).unwrap();
        let json: Value = serde_json::from_slice(&result).unwrap();

        let msgs = json["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["content"], "Hello World");
    }

    #[test]
    fn tool_use_translation() {
        let anthropic = br#"{
            "model": "gpt-4",
            "max_tokens": 100,
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Let me search."},
                        {
                            "type": "tool_use",
                            "id": "tc-1",
                            "name": "search",
                            "input": {"query": "hello"}
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "tc-1",
                            "content": "Found it."
                        }
                    ]
                }
            ]
        }"#;

        let result = anthropic_to_openai_request(anthropic).unwrap();
        let json: Value = serde_json::from_slice(&result).unwrap();

        let msgs = json["messages"].as_array().unwrap();
        // Assistant message with tool_calls
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[0]["content"], "Let me search.");
        assert!(msgs[0]["tool_calls"].is_array());
        assert_eq!(msgs[0]["tool_calls"][0]["function"]["name"], "search");

        // Tool result → tool message
        assert_eq!(msgs[1]["role"], "tool");
        assert_eq!(msgs[1]["tool_call_id"], "tc-1");
        assert_eq!(msgs[1]["content"], "Found it.");
    }

    #[test]
    fn tools_schema_translation() {
        let anthropic = br#"{
            "model": "gpt-4",
            "max_tokens": 100,
            "messages": [],
            "tools": [
                {
                    "name": "get_weather",
                    "description": "Get weather for a city",
                    "input_schema": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}}
                    }
                }
            ]
        }"#;

        let result = anthropic_to_openai_request(anthropic).unwrap();
        let json: Value = serde_json::from_slice(&result).unwrap();

        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(
            tools[0]["function"]["description"],
            "Get weather for a city"
        );
        assert!(tools[0]["function"]["parameters"].is_object());
    }

    #[test]
    fn stop_sequences_to_stop() {
        let anthropic = br#"{
            "model": "gpt-4",
            "max_tokens": 100,
            "messages": [],
            "stop_sequences": ["END", "STOP"]
        }"#;

        let result = anthropic_to_openai_request(anthropic).unwrap();
        let json: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(json["stop"], json!(["END", "STOP"]));
        assert!(json.get("stop_sequences").is_none());
    }

    #[test]
    fn stream_includes_usage_option() {
        let anthropic = br#"{
            "model": "gpt-4",
            "max_tokens": 100,
            "messages": [],
            "stream": true
        }"#;

        let result = anthropic_to_openai_request(anthropic).unwrap();
        let json: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);
    }

    #[test]
    fn response_text_only() {
        let openai = br#"{
            "id": "chatcmpl-123",
            "model": "gpt-4",
            "choices": [{
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        }"#;

        let result = openai_to_anthropic_response(openai).unwrap();
        let json: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(json["type"], "message");
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["model"], "gpt-4");
        assert_eq!(json["stop_reason"], "end_turn");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "Hello!");
        assert_eq!(json["usage"]["input_tokens"], 10);
        assert_eq!(json["usage"]["output_tokens"], 5);
    }

    #[test]
    fn response_tool_calls() {
        let openai = br#"{
            "id": "chatcmpl-123",
            "model": "gpt-4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"London\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        }"#;

        let result = openai_to_anthropic_response(openai).unwrap();
        let json: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(json["stop_reason"], "tool_use");
        let content = json["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "call_1");
        assert_eq!(content[0]["name"], "get_weather");
        assert_eq!(content[0]["input"]["city"], "London");
    }

    #[test]
    fn finish_reason_mapping() {
        assert_eq!(map_finish_reason(Some("stop")), "end_turn");
        assert_eq!(map_finish_reason(Some("length")), "max_tokens");
        assert_eq!(map_finish_reason(Some("tool_calls")), "tool_use");
        assert_eq!(map_finish_reason(Some("content_filter")), "stop_sequence");
        assert_eq!(map_finish_reason(None), "end_turn");
    }
}
