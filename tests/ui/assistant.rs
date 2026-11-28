//! AI 助手: the right sidebar's conversation beside an SSH terminal, with a
//! scripted model service and the fake terminal's commands. No model
//! service, server or keychain is reached.

use shellrs::app::{
    AiModelChoice, EditAiProvider, NewAiProvider, RunAssistantCall, SetConversationApproval,
    SetDefaultAiModel, ToolKind, UseAiModel,
};
use shellrs::assistant::AssistantServices;
use shellrs::host::{AiProtocol, AiProviderDraft};
use shellrs::settings::AssistantApproval;
use shellrs::terminal::{ExecEnd, ExecLimits, KeptBytes};
use shellrs_assistant::testing::{
    ScriptedTransport, anthropic_text, anthropic_tool, anthropic_tools, models_answer, openai_text,
};

use crate::support::*;

/// web-01's terminal in front with a model service saved, and the
/// assistant's answers coming from `transport`.
fn open(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
    transport: Arc<ScriptedTransport>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_with_models(cx, factory, transport, &["claude-test"])
}

/// The same, the service offering `models`.
fn open_with_models(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
    transport: Arc<ScriptedTransport>,
    models: &[&str],
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_with(cx, factory, AssistantServices::in_memory(transport), models)
}

/// The same, with the conversations kept where the test can look.
fn open_with_conversations(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
    transport: Arc<ScriptedTransport>,
) -> (
    WindowHandle<Root>,
    Entity<Workspace>,
    shellrs_assistant::Store,
) {
    let services = AssistantServices::in_memory(transport);
    let conversations = services.store.clone();
    let (handle, workspace) = open_with(cx, factory, services, &["claude-test"]);
    (handle, workspace, conversations)
}

fn open_with(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
    services: AssistantServices,
    models: &[&str],
) -> (WindowHandle<Root>, Entity<Workspace>) {
    let mut store = HostStore::seed();
    // On the data: no window to settle it in yet.
    shellrs::host::HostData::insert_ai_provider(
        &mut store,
        AiProviderDraft::new(
            "Claude",
            AiProtocol::Anthropic,
            "https://api.anthropic.com",
            models.iter().copied(),
        ),
    );
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory);
    workspace.update(cx, |workspace, cx| {
        workspace.set_assistant_services(Ok(services), cx)
    });
    turn_on(cx, &workspace, true);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    (handle, workspace)
}

/// 启用 AI 助手, which is off until the user turns it on.
fn turn_on(cx: &mut TestAppContext, workspace: &Entity<Workspace>, enabled: bool) {
    workspace.update(cx, |workspace, cx| {
        workspace.settings().update(cx, |settings, cx| {
            settings.update(|settings| settings.assistant.enabled = enabled, cx)
        });
    });
}

fn exited(stdout: &str) -> Result<ExecOutcome, String> {
    let limits = ExecLimits {
        head: 1024,
        tail: 1024,
    };
    let mut out = KeptBytes::new(limits);
    out.push(stdout.as_bytes());
    Ok(ExecOutcome {
        stdout: out,
        stderr: KeptBytes::new(limits),
        end: ExecEnd::Exited(Some(shellrs::ssh::ExecExit::Code(0))),
    })
}

/// Type `text` into the panel's composer and send it.
fn ask(cx: &mut TestAppContext, handle: WindowHandle<Root>, text: &str) {
    in_frame(cx, handle, |window, cx| {
        window.click("assistant-composer", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.input(text, cx);
        window.press("enter", cx);
    });
}

async fn wait_for(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    what: impl Fn(&mut gpui_kit::Window) -> bool,
) {
    cx.wait_for(handle.into(), Duration::from_secs(10), move |window, _| {
        what(window)
    })
    .await;
}

#[gpui_kit::test]
async fn the_assistant_is_offered_beside_an_ssh_terminal_by_default(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open(cx, factory, ScriptedTransport::new());
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("tool-assistant").is_some());
    });
    let files = ToolKind::ALL
        .iter()
        .position(|tool| *tool == ToolKind::Files);
    let assistant = ToolKind::ALL
        .iter()
        .position(|tool| *tool == ToolKind::Assistant);
    assert_eq!(assistant, files.map(|files| files + 1));
    // Turned off in 侧栏功能, it is not offered.
    workspace.update(cx, |workspace, cx| {
        workspace.settings().update(cx, |settings, cx| {
            settings.update(
                |settings| settings.sidebar_tools.set(ToolKind::Assistant, false),
                cx,
            )
        });
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("tool-assistant").is_none());
    });
}

/// AI 助手 opens the right sidebar wider than the other tools do, and each
/// keeps the width it was dragged to.
#[gpui_kit::test]
async fn the_assistant_is_wider_than_the_other_tools(cx: &mut TestAppContext) {
    use gpui_kit::component::dock::DockPlacement;

    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open(cx, factory, ScriptedTransport::new());
    let area = cx.update(|cx| workspace.read(cx).dock_area().clone());
    let right = |cx: &mut TestAppContext| {
        cx.update(|cx| area.read(cx).dock_size(DockPlacement::Right).unwrap())
    };
    in_frame(cx, handle, |window, cx| window.click("tool-history", cx));
    assert_eq!(right(cx), px(320.));
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    assert_eq!(right(cx), px(420.));
    // Dragged beside the assistant (the dock reports a size set here as it
    // does a drag): the other tools keep their width.
    cx.update_window(handle.into(), |_, window, cx| {
        area.update(cx, |area, cx| {
            area.set_dock_size(DockPlacement::Right, px(500.), window, cx)
        })
    })
    .unwrap();
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| window.click("tool-history", cx));
    assert_eq!(right(cx), px(320.));
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    assert_eq!(right(cx), px(500.));
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    let widths = cx.update(|cx| {
        workspace
            .read(cx)
            .settings()
            .read(cx)
            .settings()
            .sidebar_widths
    });
    assert_eq!(widths.assistant, Some(500.));
    assert_eq!(widths.right, None);
}

#[gpui_kit::test]
async fn turned_off_the_panel_points_to_where_it_is_turned_on(cx: &mut TestAppContext) {
    assert!(!shellrs::settings::AssistantSettings::default().enabled);
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open(cx, factory, ScriptedTransport::new());
    turn_on(cx, &workspace, false);
    // Off, it is still beside the terminal, and says where to turn it on.
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("assistant-disabled").is_some());
        assert!(window.try_find("assistant-composer").is_none());
        window.click("assistant-open-settings", cx);
    });
    // 设置 opens at AI 助手.
    in_frame(cx, handle, |window, _| {
        assert!(window.find("settings").visible());
        assert!(window.find("settings-new-ai-provider").visible());
    });
    turn_on(cx, &workspace, true);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("assistant-disabled").is_none());
        assert!(window.try_find("assistant-composer").is_some());
    });
}

#[gpui_kit::test]
async fn a_model_service_is_made_with_the_models_it_lists(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    let transport = ScriptedTransport::new();
    // OpenAI lists a great many: only its newest generation is added.
    transport.push(models_answer(&[
        "gpt-5.5",
        "gpt-6.2-sol",
        "gpt-6.2-sol-2026-10-01",
        "text-embedding-3-large",
        "gpt-6-luna",
    ]));
    transport.push(openai_text("OK"));
    workspace.update(cx, |workspace, cx| {
        workspace.set_assistant_services(Ok(AssistantServices::in_memory(transport.clone())), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewAiProvider), cx)
    });
    // A new service starts as OpenAI, on Responses.
    let openai = |window: &mut gpui_kit::Window| {
        assert_eq!(window.find("provider-name").value(), Some("OpenAI"));
        assert_eq!(window.find("provider-preset").value(), Some("OpenAI"));
        assert_eq!(
            window.find("provider-protocol").value(),
            Some("OpenAI Responses")
        );
        assert_eq!(
            window.find("provider-endpoint").label(),
            Some("请求发到 https://api.openai.com/v1/responses")
        );
    };
    in_frame(cx, handle, |window, cx| {
        openai(window);
        // The Anthropic preset: its name, API type and address.
        window.within("provider-preset").click("input", cx);
    });
    for key in ["up", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("provider-name").value(), Some("Anthropic"));
        assert_eq!(window.find("provider-protocol").value(), Some("Anthropic"));
        assert_eq!(
            window.find("provider-endpoint").label(),
            Some("请求发到 https://api.anthropic.com/v1/messages")
        );
        window.within("provider-preset").click("input", cx);
    });
    for key in ["down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        openai(window);
        // Chat Completions at the same address: still the OpenAI preset.
        window.within("provider-protocol").click("input", cx);
    });
    for key in ["up", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("provider-endpoint").label(),
            Some("请求发到 https://api.openai.com/v1/chat/completions")
        );
        assert_eq!(window.find("provider-preset").value(), Some("OpenAI"));
        assert_eq!(
            window.find("provider-models-empty").label(),
            Some("还没有模型。点「添加模型」填写，或点「刷新模型」从服务读取。")
        );
        // 添加模型: a row to type in, focused.
        window.click("provider-add-model", cx);
    });
    in_frame(cx, handle, |window, cx| window.input("my-model", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("provider-model", 0usize)).value(),
            Some("my-model")
        );
        window.click("provider-key", cx);
        window.input("sk-test", cx);
        window.click("provider-fetch-models", cx);
    });
    wait_for(cx, handle, |window| {
        window.try_find(("provider-model", 2usize)).is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("provider-model", 1usize)).value(),
            Some("gpt-6.2-sol")
        );
        assert_eq!(
            window.find(("provider-model", 2usize)).value(),
            Some("gpt-6-luna")
        );
        assert!(window.try_find(("provider-model", 3usize)).is_none());
        assert_eq!(
            window.find("provider-fetch-result").label(),
            Some("服务列出 5 个模型，只加常用系列的最新一代：2 个里新加了 2 个")
        );
        window.click(("provider-model-remove", 1usize), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find(("provider-model", 1usize)).is_none());
        // A row left empty is not a model.
        window.click("provider-add-model", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find(("provider-model", 3usize)).value(), Some(""));
        // 测试连接, in the footer: the first model.
        window.click("provider-check", cx);
    });
    wait_for(cx, handle, |window| {
        window
            .try_find("provider-check-result")
            .is_some_and(|result| result.label() == Some("连接成功，my-model 可以使用"))
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("provider-fetch-result").is_none());
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;

    let asked = &transport.requests()[0];
    assert_eq!(asked.url, "https://api.openai.com/v1/models");
    assert_eq!(asked.header("authorization"), Some("Bearer sk-test"));
    let checked = &transport.requests()[1];
    assert_eq!(checked.url, "https://api.openai.com/v1/chat/completions");
    assert_eq!(checked.json()["model"], "my-model");
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let provider = &store.ai_providers()[0];
        assert_eq!(provider.protocol, AiProtocol::OpenAi);
        assert_eq!(provider.base_url.as_ref(), "https://api.openai.com/v1");
        assert_eq!(provider.models, ["my-model", "gpt-6-luna"]);
    });
}

#[gpui_kit::test]
async fn a_typed_address_shows_its_preset(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, _workspace) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewAiProvider), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("provider-base-url", cx);
        window.press("secondary-a", cx);
        window.input("https://api.deepseek.com/v1", cx);
    });
    in_frame(cx, handle, |window, cx| {
        // The name a preset gave follows the address.
        assert_eq!(window.find("provider-preset").value(), Some("DeepSeek"));
        assert_eq!(window.find("provider-name").value(), Some("DeepSeek"));
        // 自定义, the last: the address and the name are left to be typed.
        window.within("provider-preset").click("input", cx);
    });
    for _ in 0..9 {
        in_frame(cx, handle, |window, cx| window.press("down", cx));
    }
    in_frame(cx, handle, |window, cx| window.press("enter", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("provider-preset").value(), Some("自定义"));
        assert_eq!(window.find("provider-base-url").value(), Some(""));
        assert_eq!(window.find("provider-name").value(), Some(""));
        // A name of the user's own stays.
        window.click("provider-name", cx);
        window.input("Office", cx);
        window.within("provider-preset").click("input", cx);
    });
    for key in ["up", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("provider-preset").value(), Some("LM Studio"));
        assert_eq!(
            window.find("provider-base-url").value(),
            Some("http://localhost:1234/v1")
        );
        assert_eq!(window.find("provider-name").value(), Some("Office"));
    });
}

#[gpui_kit::test]
async fn a_conversation_uses_the_model_picked_for_it(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let transport = ScriptedTransport::new();
    transport.push(anthropic_text("One."));
    transport.push(anthropic_text("Two."));
    let (handle, workspace) =
        open_with_models(cx, factory, transport.clone(), &["claude-a", "claude-b"]);
    let provider = cx.update(|cx| {
        workspace.read(cx).store().read(cx).ai_providers()[0]
            .public_id
            .to_string()
    });
    let choice = |model: &str| AiModelChoice {
        provider: provider.clone(),
        model: model.into(),
    };
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    // The settings' default model, for a conversation that picked none.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(SetDefaultAiModel(choice("claude-b"))), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("assistant-model").label(), Some("claude-b"));
    });
    ask(cx, handle, "one?");
    wait_for(cx, handle, |_| transport.requests().len() == 1).await;
    assert_eq!(transport.requests()[0].json()["model"], "claude-b");
    wait_for(cx, handle, |window| {
        window
            .try_find("assistant-usage")
            .is_some_and(|usage| usage.label() == Some("120 tokens"))
    })
    .await;

    // Another one picked in the conversation, from its next message on.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(UseAiModel(choice("claude-a"))), cx)
    });
    ask(cx, handle, "two?");
    wait_for(cx, handle, |_| transport.requests().len() == 2).await;
    let second = transport.requests()[1].json();
    assert_eq!(second["model"], "claude-a");
    // The whole conversation goes along: the first question and its answer.
    let messages = second["messages"].as_array().unwrap();
    let roles: Vec<_> = messages.iter().map(|message| &message["role"]).collect();
    assert_eq!(roles, ["user", "assistant", "user"]);
    let said = second["messages"].to_string();
    for text in ["one?", "One.", "two?"] {
        assert!(said.contains(text), "{text} is not in {said}");
    }
}

#[gpui_kit::test]
async fn without_a_model_service_the_panel_offers_to_add_one(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    turn_on(cx, &workspace, true);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("assistant-no-provider").is_some());
    });
}

#[gpui_kit::test]
async fn a_question_is_answered_and_its_tokens_counted(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let transport = ScriptedTransport::new();
    transport.push(anthropic_text("The disk is **98%** full."));
    let (handle, _) = open(cx, factory, transport.clone());
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    ask(cx, handle, "Why is the disk full?");
    // 100 tokens in and 20 out, as the script's service counts.
    wait_for(cx, handle, |window| {
        window
            .try_find("assistant-usage")
            .is_some_and(|usage| usage.label() == Some("120 tokens"))
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("assistant-title").label(),
            Some("Why is the disk full?")
        );
    });
    let sent = transport.requests()[0].json();
    let question = sent["messages"][0]["content"].as_array().unwrap();
    assert!(question.last().unwrap()["text"] == "Why is the disk full?");
    assert_eq!(sent["model"], "claude-test");
}

#[gpui_kit::test]
async fn a_command_asked_about_runs_beside_the_shell_once_approved(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    factory
        .background_outcomes
        .lock()
        .unwrap()
        .push_back(exited(" 10:00:00 up 3 days\n"));
    let transport = ScriptedTransport::new();
    transport.push(anthropic_tool(
        "Let me look.",
        "toolu_1",
        "run_command",
        serde_json::json!({"command": "uptime"}),
    ));
    transport.push(anthropic_text("Up for 3 days."));
    let (handle, workspace) = open(cx, factory.clone(), transport.clone());
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    // Ask for approval of every command in this conversation.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(SetConversationApproval(AssistantApproval::Ask)),
            cx,
        )
    });
    ask(cx, handle, "How long has it been up?");
    wait_for(cx, handle, |window| {
        window.try_find("assistant-run-c1_0").is_some()
    })
    .await;
    assert!(factory.background_commands.lock().unwrap().is_empty());
    // Asked about, it opens to the command in full before it runs.
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("assistant-call-detail-c1_0").is_none());
        window.click("assistant-call-c1_0", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("assistant-call-detail-c1_0").label(),
            Some("$ uptime")
        );
        window.click("assistant-run-c1_0", cx)
    });
    wait_for(cx, handle, |_| transport.requests().len() == 2).await;
    assert_eq!(*factory.background_commands.lock().unwrap(), ["uptime"]);
    let second = transport.requests()[1].json();
    let result = &second["messages"][2]["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert!(result["content"].as_str().unwrap().contains("up 3 days"));
    let _ = workspace;
}

#[gpui_kit::test]
async fn approving_for_the_user_asks_only_about_dangerous_commands(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    factory
        .background_outcomes
        .lock()
        .unwrap()
        .push_back(exited("/dev/sda1 98% /\n"));
    let transport = ScriptedTransport::new();
    transport.push(anthropic_tool(
        "",
        "toolu_1",
        "run_command",
        serde_json::json!({"command": "df -h"}),
    ));
    transport.push(anthropic_tool(
        "",
        "toolu_2",
        "run_command",
        serde_json::json!({"command": "rm -rf /var/log/old"}),
    ));
    let (handle, _) = open(cx, factory.clone(), transport.clone());
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    ask(cx, handle, "Free some room");
    // `df -h` ran unasked; `rm -rf` waits for the user.
    wait_for(cx, handle, |window| {
        window.try_find("assistant-run-c3_0").is_some()
    })
    .await;
    assert_eq!(*factory.background_commands.lock().unwrap(), ["df -h"]);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(RunAssistantCall {
                id: "nope".into(),
                remember: false,
            }),
            cx,
        )
    });
    assert_eq!(factory.background_commands.lock().unwrap().len(), 1);
}

/// 批准方式 is a chip in the composer, as in Codex: its menu says what each
/// way asks about, and picking one changes this conversation's alone.
#[gpui_kit::test]
async fn the_approval_chip_sets_how_this_conversation_asks(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open(cx, factory, ScriptedTransport::new());
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("assistant-approval").label(), Some("帮我批准"));
        window.click("assistant-approval", cx)
    });
    in_frame(cx, handle, |window, cx| {
        use gpui_kit::test::TestQueryExt as _;
        // Every choice and its description fit in the menu, which opens
        // above the chip: a long description wraps, not runs past its edge.
        let menu = window.find_all_by_role(gpui_kit::Role::Menu)[0].bounds();
        let chip = window.find("assistant-approval").bounds();
        assert!(menu.bottom() <= chip.top(), "{menu:?} {chip:?}");
        for key in ["ask", "approve_for_me", "full_access"] {
            let choice = window.find(format!("assistant-approval-{key}"));
            assert!(choice.visible());
            let bounds = choice.bounds();
            assert!(
                bounds.left() >= menu.left() && bounds.right() <= menu.right(),
                "{key}: {bounds:?} is not inside {menu:?}"
            );
        }
        window.click("assistant-approval-ask", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("assistant-approval").label(), Some("请求批准"));
    });
    let global = workspace.read_with(cx, |workspace, cx| {
        workspace.settings().read(cx).settings().assistant.approval
    });
    assert_eq!(global, AssistantApproval::ApproveForMe);
}

/// 「执行，本对话不再问相似命令」 on `systemctl restart`: later `systemctl`
/// commands run
/// unasked whatever their words; another program is still asked about.
#[gpui_kit::test]
async fn a_kind_of_command_runs_unasked_once_the_user_says_so(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    for _ in 0..2 {
        factory
            .background_outcomes
            .lock()
            .unwrap()
            .push_back(exited("active\n"));
    }
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
            serde_json::json!({"command": command}),
        ));
    }
    transport.push(anthropic_text("Restarted."));
    let (handle, _) = open(cx, factory.clone(), transport.clone());
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(SetConversationApproval(AssistantApproval::Ask)),
            cx,
        )
    });
    ask(cx, handle, "Restart nginx");
    wait_for(cx, handle, |window| {
        window.try_find("assistant-run-always-c1_0").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("assistant-run-always-c1_0").label(),
            Some("执行，本对话不再问相似命令")
        );
        window.click("assistant-run-always-c1_0", cx)
    });
    // `systemctl status` ran without asking; `journalctl` waits.
    wait_for(cx, handle, |window| {
        window.try_find("assistant-run-c5_0").is_some()
    })
    .await;
    assert_eq!(
        *factory.background_commands.lock().unwrap(),
        [
            "systemctl restart nginx",
            "systemctl status nginx --no-pager"
        ]
    );
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("assistant-run-always-c5_0").label(),
            Some("执行，本对话不再问相似命令")
        );
    });
}

/// The model's other calls of the same reply, asked about before the user
/// answered: those of the kind not asked about again run along.
#[gpui_kit::test]
async fn calls_of_the_same_reply_and_kind_run_along(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    for _ in 0..2 {
        factory
            .background_outcomes
            .lock()
            .unwrap()
            .push_back(exited(""));
    }
    let transport = ScriptedTransport::new();
    transport.push(anthropic_tools(&[
        (
            "toolu_1",
            "run_command",
            serde_json::json!({"command": "systemctl restart nginx"}),
        ),
        (
            "toolu_2",
            "run_command",
            serde_json::json!({"command": "systemctl restart php-fpm"}),
        ),
    ]));
    transport.push(anthropic_text("Both restarted."));
    let (handle, _) = open(cx, factory.clone(), transport.clone());
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(SetConversationApproval(AssistantApproval::Ask)),
            cx,
        )
    });
    ask(cx, handle, "Restart the web stack");
    wait_for(cx, handle, |window| {
        window.try_find("assistant-run-always-c1_0").is_some()
            && window.try_find("assistant-run-c1_1").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("assistant-run-always-c1_0", cx)
    });
    wait_for(cx, handle, |_| transport.requests().len() == 2).await;
    let mut ran = factory.background_commands.lock().unwrap().clone();
    ran.sort();
    assert_eq!(
        ran,
        ["systemctl restart nginx", "systemctl restart php-fpm"]
    );
}

/// Until the turn's answer is in: the script's service counts 120 tokens.
async fn answered(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    wait_for(cx, handle, |window| {
        window
            .try_find("assistant-usage")
            .is_some_and(|usage| usage.label() == Some("120 tokens"))
    })
    .await;
}

/// One conversation deleted by itself (issue #171), from its header, after
/// asking: the other stays, and the panel starts a new one.
#[gpui_kit::test]
async fn a_conversation_is_deleted_by_itself(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let transport = ScriptedTransport::new();
    transport.push(anthropic_text("Disk first."));
    transport.push(anthropic_text("Memory next."));
    let (handle, _, conversations) = open_with_conversations(cx, factory, transport);
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    let titles = || -> Vec<String> {
        let mut titles: Vec<String> = conversations
            .conversations(None)
            .unwrap()
            .into_iter()
            .map(|conversation| conversation.title)
            .collect();
        titles.sort();
        titles
    };
    // Nothing saved yet: nothing to delete.
    in_frame(cx, handle, |window, cx| {
        window.click("assistant-delete", cx)
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("ok").is_none())
    });

    ask(cx, handle, "Why is the disk full?");
    answered(cx, handle).await;
    in_frame(cx, handle, |window, cx| window.click("assistant-new", cx));
    ask(cx, handle, "Why is memory short?");
    answered(cx, handle).await;
    assert_eq!(titles(), ["Why is memory short?", "Why is the disk full?"]);

    in_frame(cx, handle, |window, cx| {
        window.click("assistant-delete", cx)
    });
    wait_for(cx, handle, |window| {
        window.try_find("ok").is_some_and(|ok| ok.visible())
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    assert_eq!(titles(), ["Why is the disk full?"]);
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("assistant-title").label(), Some("新对话"));
    });
}

/// 高级设置 › 上下文大小 (issue #171): set in the service's form, it is the
/// size its conversations count against, and the form opens on it again.
/// Something else is refused there and then.
#[gpui_kit::test]
async fn a_model_services_context_size_is_set_under_advanced(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let transport = ScriptedTransport::new();
    transport.push(anthropic_text("The disk is full."));
    let (handle, workspace) = open(cx, factory, transport);
    let provider = |cx: &mut TestAppContext| {
        cx.update(|cx| workspace.read(cx).store().read(cx).ai_providers()[0].clone())
    };
    let id = provider(cx).id;
    let edit = |cx: &mut TestAppContext| {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(EditAiProvider(id)), cx)
        });
        cx.run_until_parked();
    };

    // Folded away until opened, as for a service with none of it set.
    edit(cx);
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("provider-context").is_none());
        window.click("provider-advanced", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("provider-context").value(), Some(""));
        window.click("provider-context", cx);
        window.input("64K", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("provider-context").is_none()
    })
    .await;
    assert_eq!(provider(cx).context_window, Some(64_000));

    // A conversation counts against it.
    in_frame(cx, handle, |window, cx| window.click("tool-assistant", cx));
    ask(cx, handle, "Why is the disk full?");
    wait_for(cx, handle, |window| {
        window
            .try_find("assistant-usage")
            .is_some_and(|usage| usage.label() == Some("120 tokens"))
    })
    .await;
    let context = cx.update(|cx| {
        let panel = workspace.read(cx).assistant_panel(cx);
        panel.read(cx).context()
    });
    assert_eq!(context, Some((120, 64_000)));

    // The form opens on it; something else is refused, and nothing saved.
    edit(cx);
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("provider-context").value(), Some("64K"));
        window.click("provider-context", cx);
        window.press("secondary-a", cx);
        window.input("lots", cx);
        window.click("commit", cx);
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert!(window.find("provider-context").visible());
    });
    assert_eq!(provider(cx).context_window, Some(64_000));
}
