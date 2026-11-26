//! OpenAI's Chat Completions API, as most services speak it: the request,
//! and its stream read back.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::error::{ProviderError, ProviderErrorKind};
use crate::http::HttpRequest;
use crate::message::{Part, Role, StopReason, Usage};
use crate::provider::{Decoder, Delta, Request, auth_headers, call_id, context_text};
use crate::sse::SseEvent;
use crate::tools::ELIDED;

pub(crate) fn build(request: &Request<'_>) -> HttpRequest {
    let config = request.config;
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.schema,
                },
            })
        })
        .collect();
    // No `max_tokens`: services disagree on its name and its most, and each
    // takes its model's own when it is left out.
    let body = json!({
        "model": config.model.trim(),
        "stream": true,
        "stream_options": {"include_usage": true},
        "messages": messages(request),
        "tools": tools,
        "tool_choice": "auto",
    });
    let mut headers = vec![("content-type".to_owned(), "application/json".to_owned())];
    headers.extend(auth_headers(config));
    HttpRequest {
        url: config.endpoint(),
        headers,
        body: body.to_string().into_bytes(),
    }
}

fn messages(request: &Request<'_>) -> Vec<Value> {
    let source = request.config.source();
    // Reasoning goes back to the model that wrote it, as DeepSeek and Kimi
    // want for their reasoning models, and only within the turn it belongs
    // to: from the last question of the user on.
    let turn_starts = request
        .messages
        .iter()
        .rposition(|message| message.role == Role::User)
        .unwrap_or(0);
    let mut out = vec![json!({"role": "system", "content": request.system})];
    let mut ids: HashMap<&str, &str> = HashMap::new();
    for (index, message) in request.messages.iter().enumerate() {
        match message.role {
            Role::User => {
                let text: Vec<String> = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Text { text } if !text.is_empty() => Some(text.clone()),
                        Part::Context { context } => Some(context_text(context)),
                        _ => None,
                    })
                    .collect();
                if text.is_empty() {
                    continue;
                }
                let text = text.join("\n\n");
                match out.last_mut() {
                    Some(last) if last["role"] == "user" => {
                        let joined =
                            format!("{}\n\n{text}", last["content"].as_str().unwrap_or_default());
                        last["content"] = json!(joined);
                    }
                    _ => out.push(json!({"role": "user", "content": text})),
                }
            }
            Role::Assistant => {
                let replayed = message.raw.as_ref().is_some_and(|raw| raw.source == source);
                let text = message.text();
                let calls: Vec<Value> = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::ToolCall {
                            id,
                            provider_id,
                            name,
                            input,
                        } => {
                            let sent = call_id(id, provider_id.as_deref(), replayed);
                            ids.insert(id, sent);
                            Some(json!({
                                "id": sent,
                                "type": "function",
                                "function": {"name": name, "arguments": input.to_string()},
                            }))
                        }
                        _ => None,
                    })
                    .collect();
                if text.is_empty() && calls.is_empty() {
                    continue;
                }
                let mut sent = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { json!(text) },
                });
                if !calls.is_empty() {
                    sent["tool_calls"] = json!(calls);
                }
                if replayed
                    && index > turn_starts
                    && let Some(reasoning) = message
                        .raw
                        .as_ref()
                        .and_then(|raw| raw.content.get("reasoning_content"))
                        .filter(|reasoning| reasoning.as_str().is_some_and(|text| !text.is_empty()))
                {
                    sent["reasoning_content"] = reasoning.clone();
                }
                out.push(sent);
            }
            Role::ToolResults => {
                let elided = request
                    .elided_before
                    .is_some_and(|before| message.seq < before);
                for part in &message.parts {
                    if let Part::ToolResult {
                        call_id, output, ..
                    } = part
                    {
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": ids.get(call_id.as_str()).copied().unwrap_or(call_id),
                            "content": if elided { ELIDED } else { output.as_str() },
                        }));
                    }
                }
            }
        }
    }
    out
}

/// Reads the stream into deltas, and keeps the reasoning, which some
/// services want back.
#[derive(Default)]
pub(crate) struct OpenAiDecoder {
    reasoning: String,
    /// The calls seen, by the index the service gave them.
    calls: Vec<usize>,
    done: bool,
}

fn number(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

impl Decoder for OpenAiDecoder {
    fn event(&mut self, event: &SseEvent) -> Result<Vec<Delta>, ProviderError> {
        let data = event.data.trim();
        if data == "[DONE]" {
            self.done = true;
            return Ok(Vec::new());
        }
        let data: Value = serde_json::from_str(data).map_err(|_| {
            ProviderError::new(ProviderErrorKind::Protocol).with_message("an event was not JSON")
        })?;
        if data.get("error").is_some() {
            return Err(ProviderError::from_stream(event.data.as_bytes()));
        }
        let mut deltas = Vec::new();
        if let Some(choice) = data.pointer("/choices/0") {
            let delta = choice.get("delta").cloned().unwrap_or_default();
            // DeepSeek, Kimi and Qwen call it `reasoning_content`,
            // OpenRouter `reasoning`.
            for field in ["reasoning_content", "reasoning"] {
                if let Some(text) = delta
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    self.reasoning.push_str(text);
                    deltas.push(Delta::Thinking(text.to_owned()));
                }
            }
            if let Some(text) = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                deltas.push(Delta::Text(text.to_owned()));
            }
            for call in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let given = number(call, "/index").unwrap_or(self.calls.len() as u64) as usize;
                let index = match self.calls.iter().position(|seen| *seen == given) {
                    Some(index) => index,
                    None => {
                        self.calls.push(given);
                        deltas.push(Delta::ToolStart {
                            index: self.calls.len() - 1,
                            id: call
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            name: call
                                .pointer("/function/name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        });
                        self.calls.len() - 1
                    }
                };
                if let Some(json) = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .filter(|json| !json.is_empty())
                {
                    deltas.push(Delta::ToolInput {
                        index,
                        json: json.to_owned(),
                    });
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                deltas.push(Delta::Stop(match reason {
                    "stop" => StopReason::EndTurn,
                    "tool_calls" | "function_call" => StopReason::ToolUse,
                    "length" => StopReason::MaxTokens,
                    "content_filter" => StopReason::Refusal,
                    _ => StopReason::Other,
                }));
            }
        }
        if let Some(usage) = data.get("usage").filter(|usage| usage.is_object()) {
            let prompt = number(usage, "/prompt_tokens").unwrap_or(0);
            // OpenAI says what was cached in the details; DeepSeek in hits.
            let cached = number(usage, "/prompt_tokens_details/cached_tokens")
                .or_else(|| number(usage, "/prompt_cache_hit_tokens"))
                .unwrap_or(0)
                .min(prompt);
            deltas.push(Delta::Usage(Usage {
                input: prompt - cached,
                output: number(usage, "/completion_tokens").unwrap_or(0),
                cache_read: cached,
                cache_write: 0,
            }));
        }
        Ok(deltas)
    }

    fn done(&self) -> bool {
        self.done
    }

    fn raw(&self) -> Option<Value> {
        (!self.reasoning.is_empty()).then(|| json!({"reasoning_content": self.reasoning}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiKey, Protocol, ProviderConfig};
    use crate::message::{Message, Raw, ResultStatus};
    use crate::sse::SseParser;
    use crate::tools::{Capabilities, tool_specs};

    fn config() -> ProviderConfig {
        ProviderConfig {
            protocol: Protocol::OpenAi,
            base_url: "https://api.deepseek.com/v1/".into(),
            model: "deepseek-x".into(),
            api_key: Some(ApiKey::new("sk-test")),
        }
    }

    fn message(seq: i64, role: Role, parts: Vec<Part>, raw: Option<Raw>) -> Message {
        Message {
            seq,
            role,
            parts,
            raw,
            stop: None,
            usage: None,
            created_ms: 0,
        }
    }

    #[test]
    fn a_conversation_becomes_chat_messages() {
        let config = config();
        let raw = Raw {
            source: config.source(),
            content: json!({"reasoning_content": "think"}),
        };
        let messages = [
            message(
                0,
                Role::User,
                vec![Part::Text {
                    text: "disk?".into(),
                }],
                None,
            ),
            message(
                1,
                Role::Assistant,
                vec![Part::ToolCall {
                    id: "c1_0".into(),
                    provider_id: Some("call_9".into()),
                    name: "run_command".into(),
                    input: json!({"command": "df -h"}),
                }],
                Some(raw),
            ),
            message(
                2,
                Role::ToolResults,
                vec![Part::ToolResult {
                    call_id: "c1_0".into(),
                    status: ResultStatus::Ok,
                    output: "98%".into(),
                    exit: Some(0),
                    total_bytes: None,
                    mode: None,
                }],
                None,
            ),
        ];
        let tools = tool_specs(Capabilities {
            run_command: true,
            read_file: true,
        });
        let request = build(&Request {
            config: &config,
            system: "SYSTEM",
            tools: &tools,
            messages: &messages,
            elided_before: None,
        });
        let body = request.json();

        assert_eq!(request.url, "https://api.deepseek.com/v1/chat/completions");
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["tools"][1]["function"]["name"], "run_command");
        let sent = body["messages"].as_array().unwrap();
        assert_eq!(sent[0]["role"], "system");
        assert_eq!(sent[1]["content"], "disk?");
        assert_eq!(sent[2]["tool_calls"][0]["id"], "call_9");
        assert_eq!(
            sent[2]["tool_calls"][0]["function"]["arguments"],
            r#"{"command":"df -h"}"#
        );
        // Within the turn, DeepSeek gets its reasoning back.
        assert_eq!(sent[2]["reasoning_content"], "think");
        assert_eq!(sent[3]["role"], "tool");
        assert_eq!(sent[3]["tool_call_id"], "call_9");
    }

    const STREAM: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"hm\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Let me check\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"run_command\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"command\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"uptime\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":30,\"prompt_tokens_details\":{\"cached_tokens\":100}}}\n\n",
        "data: [DONE]\n\n",
    );

    #[test]
    fn a_stream_reads_into_deltas() {
        let mut parser = SseParser::new();
        let mut decoder = OpenAiDecoder::default();
        let mut deltas = Vec::new();
        for event in parser.push(STREAM.as_bytes()).unwrap() {
            deltas.extend(decoder.event(&event).unwrap());
        }
        assert!(decoder.done());
        assert_eq!(
            deltas,
            [
                Delta::Thinking("hm".into()),
                Delta::Text("Let me check".into()),
                Delta::ToolStart {
                    index: 0,
                    id: "call_1".into(),
                    name: "run_command".into()
                },
                Delta::ToolInput {
                    index: 0,
                    json: "{\"command\":".into()
                },
                Delta::ToolInput {
                    index: 0,
                    json: "\"uptime\"}".into()
                },
                Delta::Stop(StopReason::ToolUse),
                Delta::Usage(Usage {
                    input: 20,
                    output: 30,
                    cache_read: 100,
                    cache_write: 0
                }),
            ]
        );
        assert_eq!(decoder.raw(), Some(json!({"reasoning_content": "hm"})));
    }

    #[test]
    fn an_error_in_the_stream_is_a_failure() {
        let mut decoder = OpenAiDecoder::default();
        let error = decoder
            .event(&SseEvent {
                event: None,
                data: r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error"}}"#
                    .into(),
            })
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    }
}
