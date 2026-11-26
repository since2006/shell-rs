//! The loop of a conversation, against a scripted model.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::testing::{
    Scripted, ScriptedTransport, anthropic_stopping, anthropic_text, anthropic_tool, error_answer,
    models_answer, openai_text, responses_text, responses_tool,
};
use crate::*;

const ALL: Capabilities = Capabilities {
    run_command: true,
    read_file: true,
};

fn claude() -> ProviderConfig {
    ProviderConfig {
        protocol: Protocol::Anthropic,
        base_url: "https://api.anthropic.com".into(),
        model: "claude-test".into(),
        api_key: Some(ApiKey::new("sk-ant-test")),
    }
}

fn setup(
    transport: &Arc<ScriptedTransport>,
    store: &Store,
    approval: Approval,
    conversation: ConversationChoice,
    provider: ProviderConfig,
) -> SessionSetup {
    SessionSetup {
        provider,
        provider_public_id: Some("p1".into()),
        transport: transport.clone(),
        env: Arc::new(FixedEnv::new(1_000)),
        store: store.clone(),
        conversation,
        language: "English".into(),
        capabilities: ALL,
        approval,
    }
}

fn new_conversation() -> ConversationChoice {
    ConversationChoice::New(NewConversation {
        host_public_id: Some("host1".into()),
        host_label: "web-01".into(),
    })
}

fn start(transport: &Arc<ScriptedTransport>, store: &Store, approval: Approval) -> Session {
    Session::start(
        &tokio::runtime::Handle::current(),
        setup(transport, store, approval, new_conversation(), claude()),
    )
}

/// The events up to and with the first that `last` picks.
async fn until(session: &Session, last: impl Fn(&SessionEvent) -> bool) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(300), session.next_event())
            .await
            .expect("an event in time")
            .expect("the session goes on");
        let done = last(&event);
        events.push(event);
        if done {
            return events;
        }
    }
}

fn turn_ended(event: &SessionEvent) -> bool {
    matches!(event, SessionEvent::TurnEnded(_) | SessionEvent::Failed(_))
}

fn tool_call(event: &SessionEvent) -> bool {
    matches!(event, SessionEvent::ToolCall { .. })
}

fn ended(events: &[SessionEvent]) -> Option<TurnEnd> {
    events.iter().find_map(|event| match event {
        SessionEvent::TurnEnded(end) => Some(*end),
        _ => None,
    })
}

fn call_of(events: &[SessionEvent]) -> (String, bool, bool) {
    events
        .iter()
        .find_map(|event| match event {
            SessionEvent::ToolCall {
                id,
                needs_approval,
                dangerous,
                ..
            } => Some((id.clone(), *needs_approval, *dangerous)),
            _ => None,
        })
        .expect("a call")
}

fn ran(command: &str, stdout: &str) -> ToolAnswer {
    ToolAnswer::Command(Box::new(CommandAnswer {
        command: command.into(),
        run_in: RunIn::Background,
        stdout: Output::whole(stdout.as_bytes().to_vec()),
        stderr: Some(Output::default()),
        end: CommandEnd::Exited(Some(0)),
        ran_as: None,
    }))
}

fn only_conversation(store: &Store) -> ConversationSummary {
    let conversations = store.conversations(None).unwrap();
    assert_eq!(conversations.len(), 1, "{conversations:?}");
    conversations.into_iter().next().unwrap()
}

#[tokio::test]
async fn a_question_is_answered_streamed_and_saved() {
    let transport = ScriptedTransport::new();
    transport.push(anthropic_text("The disk is 98% full."));
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send(
        "Why is the disk full?",
        Some(HostContext {
            os: Some("linux".into()),
            ..HostContext::default()
        }),
    );
    let events = until(&session, turn_ended).await;

    assert!(matches!(events[0], SessionEvent::Opened { .. }));
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "The disk is 98% full.");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, SessionEvent::Usage { total, .. } if total.output == 20))
    );
    assert_eq!(ended(&events), Some(TurnEnd::Done));

    let conversation = only_conversation(&store);
    assert_eq!(conversation.title, "Why is the disk full?");
    assert_eq!(conversation.host_public_id.as_deref(), Some("host1"));
    let messages = store.messages(&conversation.public_id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].text(), "The disk is 98% full.");
    // The context went with the question, before it.
    let sent = transport.requests()[0].json();
    assert_eq!(
        sent["messages"][0]["content"][0]["text"],
        "<context>\nsystem: linux\n</context>"
    );
    assert_eq!(
        sent["messages"][0]["content"][1]["text"],
        "Why is the disk full?"
    );
}

#[tokio::test]
async fn a_command_runs_once_approved_and_its_output_goes_back() {
    let transport = ScriptedTransport::new();
    transport.push(anthropic_tool(
        "Let me look.",
        "toolu_1",
        "run_command",
        json!({"command": "df -h"}),
    ));
    transport.push(anthropic_text("/ is 98% full."));
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("disk?", None);
    let events = until(&session, tool_call).await;
    let (id, needs_approval, dangerous) = call_of(&events);
    assert!(needs_approval);
    assert!(!dangerous);
    session.answer(&id, ran("df -h", "/dev/sda1 98% /\n"), false);
    let events = until(&session, turn_ended).await;
    assert_eq!(ended(&events), Some(TurnEnd::Done));

    let second = transport.requests()[1].json();
    let results = &second["messages"][2]["content"][0];
    assert_eq!(results["type"], "tool_result");
    assert_eq!(results["tool_use_id"], "toolu_1");
    assert!(
        results["content"]
            .as_str()
            .unwrap()
            .contains("/dev/sda1 98% /")
    );
    assert_eq!(results["is_error"], false);
    assert_eq!(
        store
            .messages(&only_conversation(&store).public_id)
            .unwrap()
            .len(),
        4
    );
}

#[tokio::test]
async fn approving_for_the_user_asks_only_about_dangerous_commands() {
    for (approval, command, asked) in [
        (Approval::ApproveForMe, "df -h", false),
        (Approval::ApproveForMe, "rm -rf /tmp/x", true),
        (Approval::FullAccess, "rm -rf /tmp/x", false),
        (Approval::Ask, "df -h", true),
    ] {
        let transport = ScriptedTransport::new();
        transport.push(anthropic_tool(
            "",
            "toolu_1",
            "run_command",
            json!({"command": command}),
        ));
        transport.push(anthropic_text("done"));
        let store = Store::in_memory().unwrap();
        let session = start(&transport, &store, approval);

        session.send("go", None);
        let events = until(&session, tool_call).await;
        let (id, needs_approval, dangerous) = call_of(&events);
        assert_eq!(needs_approval, asked, "{approval:?} {command}");
        assert_eq!(dangerous, command.starts_with("rm"));
        session.answer(id, ran(command, ""), false);
        until(&session, turn_ended).await;
    }
}

/// 「执行，本对话不再问」 lets the commands of the same kind run: those of
/// the same program, not the same words.
#[tokio::test]
async fn a_command_the_user_said_to_remember_is_not_asked_about_again() {
    let transport = ScriptedTransport::new();
    for (id, command) in [
        ("toolu_1", "systemctl restart nginx"),
        ("toolu_2", "systemctl status nginx --no-pager"),
        ("toolu_3", "journalctl -u nginx -n 20"),
    ] {
        transport.push(anthropic_tool(
            "",
            id,
            "run_command",
            json!({"command": command}),
        ));
    }
    transport.push(anthropic_text("done"));
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("restart nginx", None);
    let (id, needs_approval, _) = call_of(&until(&session, tool_call).await);
    assert!(needs_approval);
    session.answer(id, ran("systemctl restart nginx", ""), true);
    let (id, needs_approval, _) = call_of(&until(&session, tool_call).await);
    assert!(!needs_approval);
    session.answer(
        id,
        ran("systemctl status nginx --no-pager", "active"),
        false,
    );
    // Another program is asked about.
    let (id, needs_approval, _) = call_of(&until(&session, tool_call).await);
    assert!(needs_approval);
    session.answer(id, ran("journalctl -u nginx -n 20", ""), false);
    until(&session, turn_ended).await;
}

#[tokio::test]
async fn a_declined_call_tells_the_model_why() {
    let transport = ScriptedTransport::new();
    transport.push(anthropic_tool(
        "",
        "toolu_1",
        "run_command",
        json!({"command": "reboot"}),
    ));
    transport.push(anthropic_text("OK, I won't."));
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("fix it", None);
    let (id, _, _) = call_of(&until(&session, tool_call).await);
    session.answer(
        id,
        ToolAnswer::Denied {
            reason: Some("not now".into()),
        },
        false,
    );
    until(&session, turn_ended).await;

    let result = &transport.requests()[1].json()["messages"][2]["content"][0];
    assert_eq!(result["is_error"], true);
    assert!(result["content"].as_str().unwrap().contains("not now"));
}

#[tokio::test]
async fn stopping_a_streaming_answer_keeps_what_came_and_saves() {
    let transport = ScriptedTransport::new();
    transport.push(Scripted::Hang);
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("tell me", None);
    until(&session, |event| {
        matches!(event, SessionEvent::RequestStarted)
    })
    .await;
    session.stop();
    let events = until(&session, turn_ended).await;
    assert_eq!(ended(&events), Some(TurnEnd::Stopped));

    let messages = store
        .messages(&only_conversation(&store).public_id)
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[1].parts,
        [Part::Notice {
            kind: NoticeKind::Stopped
        }]
    );
}

#[tokio::test]
async fn stopping_while_a_call_waits_answers_it_as_cancelled() {
    let transport = ScriptedTransport::new();
    transport.push(anthropic_tool(
        "",
        "toolu_1",
        "run_command",
        json!({"command": "sleep 100"}),
    ));
    transport.push(anthropic_text("ok"));
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("wait", None);
    until(&session, tool_call).await;
    session.stop();
    let events = until(&session, turn_ended).await;
    assert_eq!(ended(&events), Some(TurnEnd::Stopped));

    // The next question goes with the call answered, as both protocols
    // require.
    session.send("never mind", None);
    until(&session, turn_ended).await;
    let sent = transport.requests()[1].json();
    let last = &sent["messages"][2]["content"];
    assert_eq!(last[0]["type"], "tool_result");
    assert!(last[0]["content"].as_str().unwrap().contains("stopped"));
    assert_eq!(last[1]["text"], "never mind");
}

#[tokio::test(start_paused = true)]
async fn an_overloaded_service_is_asked_again_after_a_while() {
    let transport = ScriptedTransport::new();
    transport.push(error_answer(529, "overloaded_error", "Overloaded"));
    transport.push(anthropic_text("fine now"));
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("hi", None);
    let events = until(&session, turn_ended).await;

    let retry = events
        .iter()
        .find_map(|event| match event {
            SessionEvent::Retrying {
                attempt,
                of,
                after,
                error,
            } => Some((*attempt, *of, *after, error.kind)),
            _ => None,
        })
        .expect("a retry");
    assert_eq!(
        (retry.0, retry.1, retry.3),
        (1, 4, ProviderErrorKind::Overloaded)
    );
    assert!(retry.2 >= Duration::from_millis(800) && retry.2 <= Duration::from_millis(1200));
    assert_eq!(ended(&events), Some(TurnEnd::Done));
    assert_eq!(transport.requests().len(), 2);
}

#[tokio::test]
async fn a_rejected_key_fails_at_once_and_the_question_is_kept() {
    let transport = ScriptedTransport::new();
    transport.push(error_answer(
        401,
        "authentication_error",
        "invalid x-api-key",
    ));
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("hi", None);
    let events = until(&session, turn_ended).await;

    let failure = events.iter().find_map(|event| match event {
        SessionEvent::Failed(SessionFailure::Provider(error)) => Some(error.clone()),
        _ => None,
    });
    let failure = failure.expect("a failure");
    assert_eq!(failure.kind, ProviderErrorKind::KeyRejected);
    assert_eq!(failure.message.as_deref(), Some("invalid x-api-key"));
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(
        store
            .messages(&only_conversation(&store).public_id)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_call_with_input_the_tool_does_not_take_is_answered_as_invalid() {
    let transport = ScriptedTransport::new();
    transport.push(anthropic_tool(
        "",
        "toolu_1",
        "run_command",
        json!({"cmd": "ls"}),
    ));
    transport.push(anthropic_text("sorry"));
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("list", None);
    let events = until(&session, turn_ended).await;

    assert!(!events.iter().any(tool_call));
    let result = &transport.requests()[1].json()["messages"][2]["content"][0];
    assert_eq!(result["is_error"], true);
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("`command` is required")
    );
}

#[tokio::test]
async fn a_refusal_ends_the_turn_as_refused() {
    let transport = ScriptedTransport::new();
    transport.push(anthropic_stopping("", "refusal"));
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("something", None);
    let events = until(&session, turn_ended).await;
    assert_eq!(ended(&events), Some(TurnEnd::Refused));
}

#[tokio::test]
async fn after_fifteen_commands_the_user_says_whether_to_go_on() {
    let transport = ScriptedTransport::new();
    for index in 0..16 {
        transport.push(anthropic_tool(
            "",
            &format!("toolu_{index}"),
            "run_command",
            json!({"command": "uptime"}),
        ));
    }
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::FullAccess);

    session.send("loop", None);
    for _ in 0..15 {
        let (id, _, _) = call_of(&until(&session, tool_call).await);
        session.answer(id, ran("uptime", "up"), false);
    }
    let events = until(&session, turn_ended).await;
    assert_eq!(ended(&events), Some(TurnEnd::StepLimit));
    assert_eq!(transport.requests().len(), 15);
}

#[tokio::test]
async fn a_conversation_opened_again_answers_the_calls_left_waiting() {
    let transport = ScriptedTransport::new();
    transport.push(anthropic_tool(
        "",
        "toolu_1",
        "run_command",
        json!({"command": "uptime"}),
    ));
    let store = Store::in_memory().unwrap();
    {
        let session = start(&transport, &store, Approval::Ask);
        session.send("load?", None);
        until(&session, tool_call).await;
        // Saved as the turn went: the app quits here.
        session.stop();
        until(&session, turn_ended).await;
    }
    let public_id = only_conversation(&store).public_id;
    // As if ShellRS had quit while the call waited.
    let mut messages = store.messages(&public_id).unwrap();
    messages.truncate(2);
    store.delete_all().unwrap();
    let summary = ConversationSummary {
        public_id: public_id.clone(),
        host_public_id: None,
        host_label: "web-01".into(),
        title: "load?".into(),
        title_by_user: false,
        provider_public_id: None,
        model: "claude-test".into(),
        approval: None,
        run_in: None,
        created_ms: 0,
        updated_ms: 0,
        elided_before: None,
    };
    store.create(&summary).unwrap();
    store
        .append(
            &public_id,
            &messages,
            crate::store::TurnSaved {
                updated_ms: 0,
                title: None,
                provider_public_id: None,
                model: "claude-test",
                elided_before: None,
            },
        )
        .unwrap();

    transport.push(anthropic_text("ok"));
    let session = Session::start(
        &tokio::runtime::Handle::current(),
        setup(
            &transport,
            &store,
            Approval::Ask,
            ConversationChoice::Existing(public_id),
            claude(),
        ),
    );
    let opened = until(&session, |event| {
        matches!(event, SessionEvent::Opened { .. })
    })
    .await;
    let SessionEvent::Opened { messages, .. } = &opened[0] else {
        unreachable!()
    };
    assert_eq!(messages.len(), 3);
    session.send("again", None);
    until(&session, turn_ended).await;
    let sent = transport.requests().last().unwrap().json();
    assert_eq!(sent["messages"][2]["content"][0]["type"], "tool_result");
}

#[tokio::test]
async fn another_service_goes_on_from_the_parts() {
    let transport = ScriptedTransport::new();
    transport.push(anthropic_tool(
        "Checking.",
        "toolu_1",
        "run_command",
        json!({"command": "uptime"}),
    ));
    transport.push(anthropic_text("All good."));
    let store = Store::in_memory().unwrap();
    {
        let session = start(&transport, &store, Approval::FullAccess);
        session.send("load?", None);
        let (id, _, _) = call_of(&until(&session, tool_call).await);
        session.answer(id, ran("uptime", "up 3 days"), false);
        until(&session, turn_ended).await;
    }
    let public_id = only_conversation(&store).public_id;

    transport.push(openai_text("Still good."));
    let openai = ProviderConfig {
        protocol: Protocol::OpenAi,
        base_url: "http://localhost:11434/v1".into(),
        model: "qwen".into(),
        api_key: None,
    };
    let session = Session::start(
        &tokio::runtime::Handle::current(),
        setup(
            &transport,
            &store,
            Approval::Ask,
            ConversationChoice::Existing(public_id),
            openai,
        ),
    );
    session.send("and now?", None);
    let events = until(&session, turn_ended).await;
    assert_eq!(ended(&events), Some(TurnEnd::Done));

    let sent = transport.requests().last().unwrap().json();
    let messages = sent["messages"].as_array().unwrap();
    assert_eq!(messages[2]["tool_calls"][0]["id"], "c1_0");
    assert_eq!(messages[3]["tool_call_id"], "c1_0");
    assert_eq!(messages.last().unwrap()["content"], "and now?");
}

#[tokio::test]
async fn a_conversation_too_long_for_the_model_does_not_go_on() {
    let transport = ScriptedTransport::new();
    // 190,000 of Claude's 200,000 tokens.
    let full = {
        let text = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":190000,\"output_tokens\":1}}}\n\n\
                    data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                    data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"long\"}}\n\n\
                    data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":10}}\n\n\
                    data: {\"type\":\"message_stop\"}\n\n";
        Scripted::Answer {
            status: 200,
            content_type: "text/event-stream".into(),
            chunks: vec![text.as_bytes().to_vec()],
        }
    };
    transport.push(full);
    let store = Store::in_memory().unwrap();
    let session = start(&transport, &store, Approval::Ask);

    session.send("one", None);
    until(&session, turn_ended).await;
    session.send("two", None);
    let events = until(&session, turn_ended).await;
    assert_eq!(ended(&events), Some(TurnEnd::ContextFull));
    assert_eq!(transport.requests().len(), 1);
}

#[tokio::test]
async fn a_provider_check_asks_one_short_question() {
    let transport = ScriptedTransport::new();
    transport.push(anthropic_text("OK"));
    transport.push(error_answer(404, "not_found_error", "model: claude-nope"));
    assert_eq!(check_provider(&claude(), transport.clone()).await, Ok(()));
    let sent = transport.requests()[0].json();
    assert_eq!(sent["max_tokens"], 16);
    assert!(sent.get("tools").is_none());
    let error = check_provider(&claude(), transport.clone())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::NotFound);
}

#[tokio::test]
async fn a_responses_service_runs_a_command_and_answers() {
    let transport = ScriptedTransport::new();
    transport.push(responses_tool(
        "call_7",
        "run_command",
        json!({"command": "uptime"}),
    ));
    transport.push(responses_text("Up for 3 days."));
    let store = Store::in_memory().unwrap();
    let provider = ProviderConfig {
        protocol: Protocol::OpenAiResponses,
        base_url: "https://api.openai.com".into(),
        model: "gpt-test".into(),
        api_key: Some(ApiKey::new("sk-test")),
    };
    let session = Session::start(
        &tokio::runtime::Handle::current(),
        setup(
            &transport,
            &store,
            Approval::FullAccess,
            new_conversation(),
            provider,
        ),
    );
    session.send("load?", None);
    let (id, _, _) = call_of(&until(&session, tool_call).await);
    session.answer(id, ran("uptime", "up 3 days"), false);
    let events = until(&session, turn_ended).await;
    assert_eq!(ended(&events), Some(TurnEnd::Done));

    let requests = transport.requests();
    assert_eq!(requests[1].url, "https://api.openai.com/v1/responses");
    let input = requests[1].json()["input"].clone();
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["call_id"], input[1]["call_id"]);
    assert!(input[2]["output"].as_str().unwrap().contains("up 3 days"));
    let usage = store.usage(&only_conversation(&store).public_id).unwrap();
    assert_eq!(usage.output, 24);
}

#[tokio::test]
async fn a_service_lists_its_models() {
    let transport = ScriptedTransport::new();
    transport.push(models_answer(&[
        "deepseek-chat",
        "deepseek-reasoner",
        "deepseek-chat",
    ]));
    let openai = ProviderConfig {
        protocol: Protocol::OpenAi,
        base_url: "https://api.deepseek.com".into(),
        model: String::new(),
        api_key: Some(ApiKey::new("sk-test")),
    };
    assert_eq!(
        list_models(&openai, transport.clone()).await,
        Ok(vec!["deepseek-chat".into(), "deepseek-reasoner".into()])
    );
    let asked = &transport.requests()[0];
    assert_eq!(asked.url, "https://api.deepseek.com/v1/models");
    assert_eq!(asked.header("authorization"), Some("Bearer sk-test"));

    transport.push(models_answer(&["claude-test"]));
    assert_eq!(
        list_models(&claude(), transport.clone()).await,
        Ok(vec!["claude-test".into()])
    );
    let asked = &transport.requests()[1];
    assert_eq!(asked.url, "https://api.anthropic.com/v1/models?limit=1000");
    assert_eq!(asked.header("x-api-key"), Some("sk-ant-test"));
    assert_eq!(asked.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(asked.header("authorization"), None);

    transport.push(error_answer(
        401,
        "authentication_error",
        "invalid x-api-key",
    ));
    let error = list_models(&claude(), transport.clone()).await.unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::KeyRejected);
}
