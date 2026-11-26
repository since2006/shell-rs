//! OpenAI's Responses API: the request, and its stream read back.
//!
//! Nothing is kept on the service (`store: false`): the conversation is
//! sent whole each time, as with the other protocols. Its reasoning is
//! shown but not sent back, which a service that keeps nothing can only
//! take sealed, and not every model seals it.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::error::{ProviderError, ProviderErrorKind};
use crate::http::HttpRequest;
use crate::message::{Part, ResultStatus, Role, StopReason, Usage};
use crate::provider::{Decoder, Delta, Request, auth_headers, context_text};
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
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.schema,
            })
        })
        .collect();
    // No `max_output_tokens`: each model takes its own most when it is
    // left out.
    let body = json!({
        "model": config.model.trim(),
        "stream": true,
        "store": false,
        "instructions": request.system,
        "input": input(request),
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

/// The conversation as the API's input items: messages, and each tool call
/// and its output as items of their own, named by the assistant's ids.
fn input(request: &Request<'_>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for message in request.messages {
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
                let text = message.text();
                if !text.is_empty() {
                    out.push(json!({"role": "assistant", "content": text}));
                }
                for part in &message.parts {
                    if let Part::ToolCall {
                        id, name, input, ..
                    } = part
                    {
                        out.push(json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": input.to_string(),
                        }));
                    }
                }
            }
            Role::ToolResults => {
                let elided = request
                    .elided_before
                    .is_some_and(|before| message.seq < before);
                for part in &message.parts {
                    if let Part::ToolResult {
                        call_id,
                        status,
                        output,
                        ..
                    } = part
                    {
                        let output = if elided { ELIDED } else { output.as_str() };
                        // The API has no mark for a failed call: the output
                        // says so.
                        let output = match status {
                            ResultStatus::Ok => output.to_owned(),
                            _ => format!("[error] {output}"),
                        };
                        out.push(json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": output,
                        }));
                    }
                }
            }
        }
    }
    out
}

/// Reads the stream into deltas.
#[derive(Default)]
pub(crate) struct ResponsesDecoder {
    /// Each function call's place among the calls, by its output index.
    calls: HashMap<u64, usize>,
    /// The calls whose arguments came in pieces: their `done` repeats them.
    streamed: Vec<bool>,
    refused: bool,
    done: bool,
}

fn number(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

fn text(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

impl ResponsesDecoder {
    fn start_call(&mut self, output_index: u64, item: &Value, deltas: &mut Vec<Delta>) -> usize {
        if let Some(index) = self.calls.get(&output_index) {
            return *index;
        }
        let index = self.calls.len();
        self.calls.insert(output_index, index);
        self.streamed.push(false);
        deltas.push(Delta::ToolStart {
            index,
            id: text(item, "call_id"),
            name: text(item, "name"),
        });
        index
    }

    fn usage(response: &Value) -> Option<Delta> {
        let usage = response.get("usage").filter(|usage| usage.is_object())?;
        let input = number(usage, "/input_tokens").unwrap_or(0);
        let cached = number(usage, "/input_tokens_details/cached_tokens")
            .unwrap_or(0)
            .min(input);
        Some(Delta::Usage(Usage {
            input: input - cached,
            output: number(usage, "/output_tokens").unwrap_or(0),
            cache_read: cached,
            cache_write: 0,
        }))
    }
}

impl Decoder for ResponsesDecoder {
    fn event(&mut self, event: &SseEvent) -> Result<Vec<Delta>, ProviderError> {
        let data = event.data.trim();
        if data == "[DONE]" {
            self.done = true;
            return Ok(Vec::new());
        }
        let data: Value = serde_json::from_str(data).map_err(|_| {
            ProviderError::new(ProviderErrorKind::Protocol).with_message("an event was not JSON")
        })?;
        let kind = data.get("type").and_then(Value::as_str).unwrap_or_default();
        let output_index = number(&data, "/output_index").unwrap_or(0);
        let mut deltas = Vec::new();
        match kind {
            "response.output_text.delta" => {
                let piece = text(&data, "delta");
                if !piece.is_empty() {
                    deltas.push(Delta::Text(piece));
                }
            }
            "response.refusal.delta" => {
                self.refused = true;
                let piece = text(&data, "delta");
                if !piece.is_empty() {
                    deltas.push(Delta::Text(piece));
                }
            }
            // OpenAI's models show a summary of their reasoning; open models
            // served this way, the reasoning itself.
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let piece = text(&data, "delta");
                if !piece.is_empty() {
                    deltas.push(Delta::Thinking(piece));
                }
            }
            "response.output_item.added" => {
                let item = data.get("item").cloned().unwrap_or_default();
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let index = self.start_call(output_index, &item, &mut deltas);
                    let arguments = text(&item, "arguments");
                    if !arguments.is_empty() {
                        self.streamed[index] = true;
                        deltas.push(Delta::ToolInput {
                            index,
                            json: arguments,
                        });
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(&index) = self.calls.get(&output_index) {
                    let piece = text(&data, "delta");
                    if !piece.is_empty() {
                        self.streamed[index] = true;
                        deltas.push(Delta::ToolInput { index, json: piece });
                    }
                }
            }
            // A service that sends the arguments whole, at the end.
            "response.function_call_arguments.done" => {
                if let Some(&index) = self.calls.get(&output_index)
                    && !self.streamed[index]
                {
                    self.streamed[index] = true;
                    let arguments = text(&data, "arguments");
                    if !arguments.is_empty() {
                        deltas.push(Delta::ToolInput {
                            index,
                            json: arguments,
                        });
                    }
                }
            }
            "response.output_item.done" => {
                let item = data.get("item").cloned().unwrap_or_default();
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let index = self.start_call(output_index, &item, &mut deltas);
                    if !self.streamed[index] {
                        self.streamed[index] = true;
                        let arguments = text(&item, "arguments");
                        if !arguments.is_empty() {
                            deltas.push(Delta::ToolInput {
                                index,
                                json: arguments,
                            });
                        }
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                let response = data.get("response").cloned().unwrap_or_default();
                deltas.extend(Self::usage(&response));
                let reason = response
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str);
                deltas.push(Delta::Stop(match (kind, reason) {
                    (_, Some("max_output_tokens")) => StopReason::MaxTokens,
                    (_, Some("content_filter")) => StopReason::Refusal,
                    _ if self.refused => StopReason::Refusal,
                    _ if !self.calls.is_empty() => StopReason::ToolUse,
                    ("response.incomplete", _) => StopReason::Other,
                    _ => StopReason::EndTurn,
                }));
                self.done = true;
            }
            "response.failed" => {
                let error = data
                    .pointer("/response/error")
                    .cloned()
                    .unwrap_or_else(|| json!({"message": "the response failed"}));
                let body = json!({"error": error}).to_string();
                return Err(ProviderError::from_stream(body.as_bytes()));
            }
            "error" => return Err(ProviderError::from_stream(event.data.as_bytes())),
            _ => {}
        }
        Ok(deltas)
    }

    fn done(&self) -> bool {
        self.done
    }

    fn raw(&self) -> Option<Value> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiKey, Protocol, ProviderConfig};
    use crate::message::Message;
    use crate::sse::SseParser;
    use crate::tools::{Capabilities, tool_specs};

    fn message(seq: i64, role: Role, parts: Vec<Part>) -> Message {
        Message {
            seq,
            role,
            parts,
            raw: None,
            stop: None,
            usage: None,
            created_ms: 0,
        }
    }

    #[test]
    fn a_conversation_becomes_input_items() {
        let config = ProviderConfig {
            protocol: Protocol::OpenAiResponses,
            base_url: "https://api.openai.com".into(),
            model: "gpt-x".into(),
            api_key: Some(ApiKey::new("sk-test")),
        };
        let messages = [
            message(
                0,
                Role::User,
                vec![Part::Text {
                    text: "disk?".into(),
                }],
            ),
            message(
                1,
                Role::Assistant,
                vec![
                    Part::Thinking { text: "hm".into() },
                    Part::Text {
                        text: "Let me look.".into(),
                    },
                    Part::ToolCall {
                        id: "c1_0".into(),
                        provider_id: Some("call_9".into()),
                        name: "run_command".into(),
                        input: json!({"command": "df -h"}),
                    },
                ],
            ),
            message(
                2,
                Role::ToolResults,
                vec![Part::ToolResult {
                    call_id: "c1_0".into(),
                    status: ResultStatus::Denied,
                    output: "the user said no".into(),
                    exit: None,
                    total_bytes: None,
                    mode: None,
                }],
            ),
        ];
        let tools = tool_specs(Capabilities {
            run_command: true,
            read_file: false,
        });
        let request = build(&Request {
            config: &config,
            system: "SYSTEM",
            tools: &tools,
            messages: &messages,
            elided_before: None,
        });
        let body = request.json();

        assert_eq!(request.url, "https://api.openai.com/v1/responses");
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
        assert_eq!(body["store"], false);
        assert_eq!(body["instructions"], "SYSTEM");
        assert_eq!(body["tools"][1]["name"], "run_command");
        assert_eq!(body["tools"][1]["type"], "function");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 4, "{input:#?}");
        assert_eq!(input[0], json!({"role": "user", "content": "disk?"}));
        assert_eq!(
            input[1],
            json!({"role": "assistant", "content": "Let me look."})
        );
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "c1_0");
        assert_eq!(input[2]["arguments"], r#"{"command":"df -h"}"#);
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "c1_0");
        assert_eq!(input[3]["output"], "[error] the user said no");
    }

    fn events(lines: &[Value]) -> String {
        lines
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
            })
            .collect()
    }

    fn decode(stream: &str) -> (Vec<Delta>, ResponsesDecoder) {
        let mut parser = SseParser::new();
        let mut decoder = ResponsesDecoder::default();
        let mut deltas = Vec::new();
        for event in parser.push(stream.as_bytes()).unwrap() {
            deltas.extend(decoder.event(&event).unwrap());
        }
        (deltas, decoder)
    }

    #[test]
    fn a_stream_reads_into_deltas() {
        let stream = events(&[
            json!({"type": "response.created", "response": {"id": "resp_1", "status": "in_progress"}}),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "summary_index": 0, "delta": "Checking load"}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "id": "msg_1", "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "Let me "}),
            json!({"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "look."}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "run_command", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 2, "item_id": "fc_1", "delta": "{\"command\":"}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 2, "item_id": "fc_1", "delta": "\"uptime\"}"}),
            json!({"type": "response.function_call_arguments.done", "output_index": 2, "item_id": "fc_1", "arguments": "{\"command\":\"uptime\"}"}),
            json!({"type": "response.output_item.done", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "run_command", "arguments": "{\"command\":\"uptime\"}"}}),
            json!({"type": "response.completed", "response": {"status": "completed", "usage": {"input_tokens": 120, "input_tokens_details": {"cached_tokens": 100}, "output_tokens": 30}}}),
        ]);
        let (deltas, decoder) = decode(&stream);
        assert!(decoder.done());
        assert_eq!(
            deltas,
            [
                Delta::Thinking("Checking load".into()),
                Delta::Text("Let me ".into()),
                Delta::Text("look.".into()),
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
                Delta::Usage(Usage {
                    input: 20,
                    output: 30,
                    cache_read: 100,
                    cache_write: 0
                }),
                Delta::Stop(StopReason::ToolUse),
            ]
        );
        assert_eq!(decoder.raw(), None);
    }

    #[test]
    fn arguments_sent_whole_are_taken_once() {
        let stream = events(&[
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call", "call_id": "call_1", "name": "read_terminal", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.done", "output_index": 0, "arguments": "{\"lines\":50}"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "function_call", "call_id": "call_1", "name": "read_terminal", "arguments": "{\"lines\":50}"}}),
            json!({"type": "response.completed", "response": {"status": "completed"}}),
        ]);
        let (deltas, _) = decode(&stream);
        let inputs: Vec<_> = deltas
            .iter()
            .filter(|delta| matches!(delta, Delta::ToolInput { .. }))
            .collect();
        assert_eq!(
            inputs,
            [&Delta::ToolInput {
                index: 0,
                json: "{\"lines\":50}".into()
            }]
        );
    }

    #[test]
    fn a_cut_answer_says_why() {
        let stream = events(&[
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "Half"}),
            json!({"type": "response.incomplete", "response": {"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}}}),
        ]);
        let (deltas, decoder) = decode(&stream);
        assert!(decoder.done());
        assert_eq!(deltas.last(), Some(&Delta::Stop(StopReason::MaxTokens)));
    }

    #[test]
    fn a_failure_in_the_stream_is_an_error() {
        let mut decoder = ResponsesDecoder::default();
        let failed = SseEvent {
            event: Some("response.failed".into()),
            data: json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "rate_limit_exceeded", "message": "Slow down"}}}).to_string(),
        };
        let error = decoder.event(&failed).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RateLimited);
        assert_eq!(error.message.as_deref(), Some("Slow down"));

        let error = decoder
            .event(&SseEvent {
                event: Some("error".into()),
                data: json!({"type": "error", "code": "context_length_exceeded", "message": "Your input exceeds the context window"}).to_string(),
            })
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ContextTooLong);
    }
}
