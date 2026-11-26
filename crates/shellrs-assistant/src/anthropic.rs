//! Anthropic's Messages API: the request, and its stream read back.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::error::{ProviderError, ProviderErrorKind};
use crate::http::HttpRequest;
use crate::message::{Message, Part, ResultStatus, Role, StopReason, Usage};
use crate::provider::{Decoder, Delta, Request, auth_headers, call_id, context_text};
use crate::sse::SseEvent;
use crate::tools::ELIDED;

/// Anthropic's server-side fallback, sent only to Claude: a request a
/// safety check declines goes on with a model the service picks.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

pub(crate) fn build(request: &Request<'_>) -> HttpRequest {
    let config = request.config;
    // Claude's own parameters, which a service that only speaks the
    // protocol may refuse.
    let claude = config.is_anthropic();
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            let mut spec = json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.schema,
            });
            if claude {
                spec["eager_input_streaming"] = json!(true);
            }
            spec
        })
        .collect();
    let mut body = json!({
        "model": config.model.trim(),
        "max_tokens": config.anthropic_max_tokens(),
        "stream": true,
        // The tools and the system prompt never change in a conversation:
        // cached once, read from the cache after.
        "system": [{"type": "text", "text": request.system, "cache_control": {"type": "ephemeral"}}],
        "messages": messages(request),
        "tools": tools,
        "tool_choice": {"type": "auto"},
    });
    let mut headers = vec![("content-type".to_owned(), "application/json".to_owned())];
    headers.extend(auth_headers(config));
    if claude {
        body["output_config"] = json!({"effort": "high"});
        body["fallbacks"] = json!("default");
        headers.push(("anthropic-beta".to_owned(), FALLBACK_BETA.to_owned()));
    }
    HttpRequest {
        url: config.endpoint(),
        headers,
        body: body.to_string().into_bytes(),
    }
}

/// The conversation as Anthropic's messages: the model's own content as it
/// gave it when the same model reads it again, else built from the parts;
/// messages of one role in a row joined into one.
fn messages(request: &Request<'_>) -> Vec<Value> {
    let source = request.config.source();
    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    // Each call's id as the request sends its tool_use.
    let mut ids: HashMap<&str, &str> = HashMap::new();
    for message in request.messages {
        let (role, blocks) = match message.role {
            Role::User => ("user", user_blocks(message)),
            Role::ToolResults => ("user", result_blocks(message, &ids, request.elided_before)),
            Role::Assistant => {
                let replayed = message.raw.as_ref().is_some_and(|raw| raw.source == source);
                for part in &message.parts {
                    if let Part::ToolCall {
                        id, provider_id, ..
                    } = part
                    {
                        ids.insert(id, call_id(id, provider_id.as_deref(), replayed));
                    }
                }
                let blocks = match &message.raw {
                    Some(raw) if replayed => raw.content.as_array().cloned().unwrap_or_default(),
                    _ => assistant_blocks(message),
                };
                ("assistant", blocks)
            }
        };
        if blocks.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some((last, joined)) if *last == role => joined.extend(blocks),
            _ => out.push((role, blocks)),
        }
    }
    // The conversation so far is cached up to its last block.
    if let Some(block) = out
        .last_mut()
        .and_then(|(_, blocks)| blocks.last_mut())
        .and_then(Value::as_object_mut)
    {
        block.insert("cache_control".into(), json!({"type": "ephemeral"}));
    }
    out.into_iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect()
}

fn user_blocks(message: &Message) -> Vec<Value> {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } if !text.is_empty() => Some(json!({"type": "text", "text": text})),
            Part::Context { context } => {
                Some(json!({"type": "text", "text": context_text(context)}))
            }
            _ => None,
        })
        .collect()
}

fn result_blocks(
    message: &Message,
    ids: &HashMap<&str, &str>,
    elided_before: Option<i64>,
) -> Vec<Value> {
    let elided = elided_before.is_some_and(|before| message.seq < before);
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolResult {
                call_id,
                status,
                output,
                ..
            } => Some(json!({
                "type": "tool_result",
                "tool_use_id": ids.get(call_id.as_str()).copied().unwrap_or(call_id),
                "content": if elided { ELIDED } else { output.as_str() },
                "is_error": *status != ResultStatus::Ok,
            })),
            _ => None,
        })
        .collect()
}

/// An assistant message built from its parts, for another model than the
/// one that wrote it: text and tool calls; its reasoning stays behind.
fn assistant_blocks(message: &Message) -> Vec<Value> {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } if !text.is_empty() => Some(json!({"type": "text", "text": text})),
            Part::ToolCall {
                id, name, input, ..
            } => Some(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            })),
            _ => None,
        })
        .collect()
}

/// Reads the stream: deltas for the session, and the content blocks as
/// they were, to send back.
#[derive(Default)]
pub(crate) struct AnthropicDecoder {
    blocks: Vec<Value>,
    /// Each tool_use block's input as streamed, by the block's index.
    inputs: HashMap<usize, String>,
    /// The order the tool calls started in: their place among the calls.
    calls: Vec<usize>,
    usage: Usage,
    done: bool,
}

impl AnthropicDecoder {
    fn block(&mut self, index: usize) -> Option<&mut Value> {
        self.blocks.get_mut(index)
    }
}

fn number(value: &Value, field: &str) -> Option<u64> {
    value.get(field).and_then(Value::as_u64)
}

impl Decoder for AnthropicDecoder {
    fn event(&mut self, event: &SseEvent) -> Result<Vec<Delta>, ProviderError> {
        let data: Value = serde_json::from_str(&event.data).map_err(|_| {
            ProviderError::new(ProviderErrorKind::Protocol).with_message("an event was not JSON")
        })?;
        let kind = data.get("type").and_then(Value::as_str).unwrap_or_default();
        let index = number(&data, "index").unwrap_or(0) as usize;
        let mut deltas = Vec::new();
        match kind {
            "message_start" => {
                if let Some(usage) = data.pointer("/message/usage") {
                    self.usage = Usage {
                        input: number(usage, "input_tokens").unwrap_or(0),
                        output: number(usage, "output_tokens").unwrap_or(0),
                        cache_read: number(usage, "cache_read_input_tokens").unwrap_or(0),
                        cache_write: number(usage, "cache_creation_input_tokens").unwrap_or(0),
                    };
                    deltas.push(Delta::Usage(self.usage));
                }
            }
            "content_block_start" => {
                let block = data.get("content_block").cloned().unwrap_or_default();
                if self.blocks.len() <= index {
                    self.blocks.resize(index + 1, Value::Null);
                }
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        deltas.push(Delta::ToolStart {
                            index: self.calls.len(),
                            id: id.to_owned(),
                            name: name.to_owned(),
                        });
                        self.calls.push(index);
                        self.inputs.insert(index, String::new());
                        self.blocks[index] =
                            json!({"type": "tool_use", "id": id, "name": name, "input": {}});
                    }
                    Some("text") => {
                        self.blocks[index] = json!({"type": "text", "text": ""});
                        if let Some(text) = block
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            self.blocks[index]["text"] = json!(text);
                            deltas.push(Delta::Text(text.to_owned()));
                        }
                    }
                    Some("thinking") => {
                        self.blocks[index] =
                            json!({"type": "thinking", "thinking": "", "signature": ""});
                    }
                    // Redacted reasoning and blocks newer than this code:
                    // given back as they came.
                    _ => self.blocks[index] = block,
                }
            }
            "content_block_delta" => {
                let delta = data.get("delta").cloned().unwrap_or_default();
                let text = |field: &str| {
                    delta
                        .get(field)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let piece = text("text");
                        if let Some(block) = self.block(index) {
                            let joined =
                                format!("{}{piece}", block["text"].as_str().unwrap_or_default());
                            block["text"] = json!(joined);
                        }
                        deltas.push(Delta::Text(piece));
                    }
                    Some("thinking_delta") => {
                        let piece = text("thinking");
                        if let Some(block) = self.block(index) {
                            let joined = format!(
                                "{}{piece}",
                                block["thinking"].as_str().unwrap_or_default()
                            );
                            block["thinking"] = json!(joined);
                        }
                        deltas.push(Delta::Thinking(piece));
                    }
                    Some("signature_delta") => {
                        let piece = text("signature");
                        if let Some(block) = self.block(index) {
                            let joined = format!(
                                "{}{piece}",
                                block["signature"].as_str().unwrap_or_default()
                            );
                            block["signature"] = json!(joined);
                        }
                    }
                    Some("input_json_delta") => {
                        let piece = text("partial_json");
                        if let Some(input) = self.inputs.get_mut(&index) {
                            input.push_str(&piece);
                        }
                        if let Some(call) = self.calls.iter().position(|block| *block == index) {
                            deltas.push(Delta::ToolInput {
                                index: call,
                                json: piece,
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(input) = self.inputs.get(&index)
                    && let Ok(parsed) =
                        serde_json::from_str::<Value>(if input.is_empty() { "{}" } else { input })
                    && let Some(block) = self.blocks.get_mut(index)
                {
                    block["input"] = parsed;
                }
            }
            "message_delta" => {
                if let Some(usage) = data.get("usage") {
                    if let Some(output) = number(usage, "output_tokens") {
                        self.usage.output = output;
                    }
                    if let Some(input) = number(usage, "input_tokens") {
                        self.usage.input = input;
                    }
                    if let Some(read) = number(usage, "cache_read_input_tokens") {
                        self.usage.cache_read = read;
                    }
                    if let Some(write) = number(usage, "cache_creation_input_tokens") {
                        self.usage.cache_write = write;
                    }
                    deltas.push(Delta::Usage(self.usage));
                }
                if let Some(stop) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    deltas.push(Delta::Stop(StopReason::from_key(stop)));
                }
            }
            "message_stop" => self.done = true,
            "error" => return Err(ProviderError::from_stream(event.data.as_bytes())),
            _ => {}
        }
        Ok(deltas)
    }

    fn done(&self) -> bool {
        self.done
    }

    fn raw(&self) -> Option<Value> {
        let blocks: Vec<Value> = self
            .blocks
            .iter()
            .filter(|block| !block.is_null())
            .cloned()
            .collect();
        (!blocks.is_empty()).then_some(Value::Array(blocks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiKey, Protocol, ProviderConfig};
    use crate::message::{HostContext, Raw};
    use crate::sse::SseParser;
    use crate::tools::{Capabilities, tool_specs};

    fn config(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            protocol: Protocol::Anthropic,
            base_url: base_url.into(),
            model: "claude-x".into(),
            api_key: Some(ApiKey::new("sk-ant-test")),
        }
    }

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

    fn conversation(source: &str) -> Vec<Message> {
        let mut call = message(
            1,
            Role::Assistant,
            vec![
                Part::Thinking { text: "hmm".into() },
                Part::ToolCall {
                    id: "c1_0".into(),
                    provider_id: Some("toolu_A".into()),
                    name: "run_command".into(),
                    input: json!({"command": "df -h"}),
                },
            ],
        );
        call.raw = Some(Raw {
            source: source.into(),
            content: json!([
                {"type": "thinking", "thinking": "hmm", "signature": "SIG"},
                {"type": "tool_use", "id": "toolu_A", "name": "run_command", "input": {"command": "df -h"}}
            ]),
        });
        vec![
            message(
                0,
                Role::User,
                vec![
                    Part::Context {
                        context: HostContext {
                            os: Some("linux".into()),
                            ..HostContext::default()
                        },
                    },
                    Part::Text {
                        text: "disk?".into(),
                    },
                ],
            ),
            call,
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
            ),
            message(
                3,
                Role::User,
                vec![Part::Text {
                    text: "why?".into(),
                }],
            ),
        ]
    }

    fn body(
        config: &ProviderConfig,
        messages: &[Message],
        elided_before: Option<i64>,
    ) -> (Value, HttpRequest) {
        let tools = tool_specs(Capabilities {
            run_command: true,
            read_file: false,
        });
        let request = build(&Request {
            config,
            system: "SYSTEM",
            tools: &tools,
            messages,
            elided_before,
        });
        (request.json(), request)
    }

    #[test]
    fn the_same_model_gets_its_content_back_as_it_gave_it() {
        let config = config("https://api.anthropic.com");
        let (body, request) = body(&config, &conversation(&config.source()), None);

        assert_eq!(request.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(request.header("x-api-key"), Some("sk-ant-test"));
        assert_eq!(request.header("authorization"), None);
        assert_eq!(request.header("anthropic-beta"), Some(FALLBACK_BETA));
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["tools"][0]["eager_input_streaming"], true);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3, "{messages:#?}");
        assert_eq!(
            messages[0]["content"][0]["text"],
            "<context>\nsystem: linux\n</context>"
        );
        assert_eq!(messages[1]["content"][0]["signature"], "SIG");
        // The results and the next question in one user message, the
        // result naming the call as the service wrote it.
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_A");
        assert_eq!(messages[2]["content"][1]["text"], "why?");
        assert_eq!(
            messages[2]["content"][1]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn another_model_gets_the_parts_without_the_reasoning() {
        let config = config("https://api.deepseek.com/anthropic");
        let (body, request) = body(&config, &conversation("openai|other|model"), Some(3));

        assert_eq!(request.header("authorization"), Some("Bearer sk-ant-test"));
        assert!(body.get("output_config").is_none());
        assert!(body.get("fallbacks").is_none());
        assert_eq!(body["max_tokens"], 8192);
        let messages = body["messages"].as_array().unwrap();
        let call = &messages[1]["content"];
        assert_eq!(call.as_array().unwrap().len(), 1);
        assert_eq!(call[0]["id"], "c1_0");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "c1_0");
        assert_eq!(messages[2]["content"][0]["content"], ELIDED);
    }

    const STREAM: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":0,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"look\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"SIG\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: ping\ndata: {\"type\":\"ping\"}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Checking\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_B\",\"name\":\"run_command\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\": \\\"df\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\" -h\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":42}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    #[test]
    fn a_stream_reads_into_deltas_and_the_content_to_give_back() {
        let mut parser = SseParser::new();
        let mut decoder = AnthropicDecoder::default();
        let mut deltas = Vec::new();
        for event in parser.push(STREAM.as_bytes()).unwrap() {
            deltas.extend(decoder.event(&event).unwrap());
        }
        assert!(decoder.done());
        assert!(deltas.contains(&Delta::Thinking("look".into())));
        assert!(deltas.contains(&Delta::Text("Checking".into())));
        assert!(deltas.contains(&Delta::ToolStart {
            index: 0,
            id: "toolu_B".into(),
            name: "run_command".into()
        }));
        assert_eq!(deltas.last(), Some(&Delta::Stop(StopReason::ToolUse)));
        assert!(deltas.contains(&Delta::Usage(Usage {
            input: 10,
            output: 42,
            cache_read: 100,
            cache_write: 0
        })));
        let raw = decoder.raw().unwrap();
        assert_eq!(raw[0]["signature"], "SIG");
        assert_eq!(raw[1]["text"], "Checking");
        assert_eq!(raw[2]["input"]["command"], "df -h");
    }

    #[test]
    fn an_error_in_the_stream_is_a_failure() {
        let mut decoder = AnthropicDecoder::default();
        let error = decoder
            .event(&SseEvent {
                event: Some("error".into()),
                data:
                    r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
                        .into(),
            })
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Overloaded);
    }
}
