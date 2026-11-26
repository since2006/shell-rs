//! A transport that answers from a script and keeps what it was sent, and
//! the streams the services send, for the tests of this crate and of what
//! builds on it. Only with the `testing` feature.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream;
use serde_json::{Value, json};

use crate::http::{HttpRequest, HttpResponse, Transport, TransportError};

/// One answer of a [`ScriptedTransport`].
#[derive(Clone, Debug)]
pub enum Scripted {
    Answer {
        status: u16,
        content_type: String,
        chunks: Vec<Vec<u8>>,
    },
    Fail(TransportError),
    /// Never answers: for a request the user stops.
    Hang,
}

/// Answers each request with the next scripted answer; with none left,
/// fails as a network error.
#[derive(Default)]
pub struct ScriptedTransport {
    answers: Mutex<VecDeque<Scripted>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl ScriptedTransport {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn push(&self, answer: Scripted) {
        self.answers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(answer);
    }

    /// What it was sent, in order.
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Transport for ScriptedTransport {
    fn post(
        &self,
        request: HttpRequest,
    ) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        self.answer(request)
    }

    fn get(
        &self,
        request: HttpRequest,
    ) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        self.answer(request)
    }
}

impl ScriptedTransport {
    fn answer(
        &self,
        request: HttpRequest,
    ) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request);
        let answer = self
            .answers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front();
        Box::pin(async move {
            match answer {
                None => Err(TransportError::Network("nothing scripted".into())),
                Some(Scripted::Fail(error)) => Err(error),
                Some(Scripted::Hang) => futures::future::pending().await,
                Some(Scripted::Answer {
                    status,
                    content_type,
                    chunks,
                }) => Ok(HttpResponse {
                    status,
                    content_type: Some(content_type),
                    retry_after: None,
                    body: stream::iter(chunks.into_iter().map(Ok)).boxed(),
                }),
            }
        })
    }
}

fn sse(events: &[Value]) -> Scripted {
    let text: String = events
        .iter()
        .map(|event| match event.get("type").and_then(Value::as_str) {
            Some(kind) => format!("event: {kind}\ndata: {event}\n\n"),
            None => format!("data: {event}\n\n"),
        })
        .collect();
    stream_of(text)
}

fn stream_of(text: String) -> Scripted {
    // In small pieces, as a network gives them.
    let chunks = text.as_bytes().chunks(37).map(<[u8]>::to_vec).collect();
    Scripted::Answer {
        status: 200,
        content_type: "text/event-stream".into(),
        chunks,
    }
}

fn anthropic(blocks: Vec<Value>, stop: &str) -> Scripted {
    let mut events = vec![json!({
        "type": "message_start",
        "message": {"usage": {"input_tokens": 100, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "output_tokens": 1}}
    })];
    for (index, block) in blocks.into_iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}));
                events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": block["text"]}}));
            }
            Some("tool_use") => {
                events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}}));
                events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": block["input"].to_string()}}));
            }
            _ => {}
        }
        events.push(json!({"type": "content_block_stop", "index": index}));
    }
    events.push(json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 20}}));
    events.push(json!({"type": "message_stop"}));
    sse(&events)
}

/// An Anthropic answer of text that ends the turn.
pub fn anthropic_text(text: &str) -> Scripted {
    anthropic(vec![json!({"type": "text", "text": text})], "end_turn")
}

/// An Anthropic answer that says `text`, then calls a tool.
pub fn anthropic_tool(text: &str, id: &str, name: &str, input: Value) -> Scripted {
    anthropic(
        vec![
            json!({"type": "text", "text": text}),
            json!({"type": "tool_use", "id": id, "name": name, "input": input}),
        ],
        "tool_use",
    )
}

/// An Anthropic answer that calls several tools at once: `(id, name,
/// input)` each.
pub fn anthropic_tools(calls: &[(&str, &str, Value)]) -> Scripted {
    anthropic(
        calls
            .iter()
            .map(|(id, name, input)| {
                json!({"type": "tool_use", "id": id, "name": name, "input": input})
            })
            .collect(),
        "tool_use",
    )
}

/// An Anthropic answer that stops for `reason` after `text`.
pub fn anthropic_stopping(text: &str, reason: &str) -> Scripted {
    anthropic(vec![json!({"type": "text", "text": text})], reason)
}

/// An OpenAI answer of text that ends the turn.
pub fn openai_text(text: &str) -> Scripted {
    let mut events = vec![
        json!({"choices": [{"index": 0, "delta": {"content": text}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        json!({"choices": [], "usage": {"prompt_tokens": 50, "completion_tokens": 10}}),
    ];
    let mut text = String::new();
    for event in events.drain(..) {
        text.push_str(&format!("data: {event}\n\n"));
    }
    text.push_str("data: [DONE]\n\n");
    stream_of(text)
}

/// An OpenAI answer that calls a tool.
pub fn openai_tool(id: &str, name: &str, input: Value) -> Scripted {
    let events = [
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]}}]}),
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": input.to_string()}}]}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
    ];
    let mut text: String = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    text.push_str("data: [DONE]\n\n");
    stream_of(text)
}

/// An OpenAI Responses answer of text that ends the turn.
pub fn responses_text(text: &str) -> Scripted {
    let events = [
        json!({"type": "response.created", "response": {"status": "in_progress"}}),
        json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": text}),
        json!({"type": "response.completed", "response": {"status": "completed", "usage": {"input_tokens": 60, "output_tokens": 12}}}),
    ];
    sse(&events)
}

/// An OpenAI Responses answer that calls a tool.
pub fn responses_tool(call_id: &str, name: &str, input: Value) -> Scripted {
    let events = [
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call", "call_id": call_id, "name": name, "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 0, "delta": input.to_string()}),
        json!({"type": "response.completed", "response": {"status": "completed", "usage": {"input_tokens": 60, "output_tokens": 12}}}),
    ];
    sse(&events)
}

/// A service's list of its models, as `GET /v1/models` answers in every
/// protocol.
pub fn models_answer(ids: &[&str]) -> Scripted {
    let data: Vec<Value> = ids
        .iter()
        .map(|id| json!({"id": id, "object": "model"}))
        .collect();
    Scripted::Answer {
        status: 200,
        content_type: "application/json".into(),
        chunks: vec![
            json!({"object": "list", "data": data})
                .to_string()
                .into_bytes(),
        ],
    }
}

/// An error answer, as the services send them.
pub fn error_answer(status: u16, kind: &str, message: &str) -> Scripted {
    Scripted::Answer {
        status,
        content_type: "application/json".into(),
        chunks: vec![
            json!({"type": "error", "error": {"type": kind, "message": message}})
                .to_string()
                .into_bytes(),
        ],
    }
}
