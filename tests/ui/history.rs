//! 历史命令: the right sidebar's tool that lists the commands in bash's
//! history file on the SSH terminal's host, read over the terminal's own
//! connection, and puts them on the terminal's line.

use crate::support::*;
use shellrs::app::{CopyCommand, EnterCommand};

/// A history file bash wrote with times, 「ls -l」 run twice.
const HISTORY: &str = "\
@@size
120
@@history
#1727846400
ls -l
#1727846460
cd /var/log
#1727846520
ls -l
#1727846580
tail -f syslog
";

fn open_history(cx: &mut TestAppContext, factory: Arc<FakeTerminalFactory>) -> WindowHandle<Root> {
    let (handle, _) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    in_frame(cx, handle, |window, cx| {
        window.activate_window();
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-history", cx));
    handle
}

async fn wait_for_label(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    id: &'static str,
    label: &str,
) {
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find(id)
            .is_some_and(|element| element.label() == Some(label))
    })
    .await;
}

fn entry(command: &str) -> ElementId {
    ElementId::Name(format!("history-entry:{command}").into())
}

#[gpui_kit::test]
async fn the_history_lists_each_command_once_the_newest_first(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[HISTORY]));
    let handle = open_history(cx, factory.clone());
    wait_for_label(cx, handle, "history-summary", "共 3 条 · ~/.bash_history").await;
    assert!(factory.exec_commands()[0].contains(".bash_history"));

    in_frame(cx, handle, |window, _| {
        let tops: Vec<_> = ["tail -f syslog", "ls -l", "cd /var/log"]
            .into_iter()
            .map(|command| window.find(entry(command)).bounds().top())
            .collect();
        assert!(tops.windows(2).all(|pair| pair[0] < pair[1]), "{tops:?}");
        let label = window.find(entry("ls -l")).label().unwrap().to_string();
        assert!(label.starts_with("ls -l · "), "{label}");
        assert!(label.ends_with(" · 执行 2 次"), "{label}");
        // Why the commands just run are not there yet.
        assert!(window.find("history-note").visible());
    });

    // The search wants every word.
    in_frame(cx, handle, |window, cx| {
        window.click("history-search", cx);
        window.input("LS", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(entry("ls -l")).is_some());
        assert!(window.try_find(entry("tail -f syslog")).is_none());
    });
    in_frame(cx, handle, |window, cx| window.input(" kubectl", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("history-empty").label(),
            Some("没有符合条件的命令")
        );
    });
}

#[gpui_kit::test]
async fn the_history_is_read_when_shown_and_on_refresh_only(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[HISTORY]));
    let handle = open_history(cx, factory.clone());
    wait_for_label(cx, handle, "history-summary", "共 3 条 · ~/.bash_history").await;
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), 1);

    in_frame(cx, handle, |window, cx| window.click("history-refresh", cx));
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() == 2
    })
    .await;

    // Another tool: nothing runs.
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), 2);
}

#[gpui_kit::test]
async fn a_click_puts_a_command_on_the_terminals_line_and_run_runs_it(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[HISTORY]));
    let handle = open_history(cx, factory.clone());
    wait_for_label(cx, handle, "history-summary", "共 3 条 · ~/.bash_history").await;
    let typed_before = factory.written_text();

    // On the line in place of what is typed there, not run; the terminal
    // takes the keyboard to edit it.
    in_frame(cx, handle, |window, cx| {
        window.click(entry("cd /var/log"), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("terminal", INITIAL_WEB_TERMINAL)).focused(),
            Some(true)
        );
    });
    let typed = factory.written_text();
    assert_eq!(&typed[typed_before.len()..], "\x05\x15cd /var/log");

    // 执行 runs it, and only that: the card's click does not come too.
    let typed_before = typed;
    in_frame(cx, handle, |window, cx| {
        window.click(ElementId::Name("history-run:tail -f syslog".into()), cx)
    });
    let typed = factory.written_text();
    assert_eq!(&typed[typed_before.len()..], "\x05\x15tail -f syslog\r");

    // The menu's 复制.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CopyCommand("ls -l".into())), cx)
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("ls -l".into())
    );
}

#[gpui_kit::test]
async fn a_command_does_not_go_to_a_terminal_not_connected(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[HISTORY]));
    let handle = open_history(cx, factory.clone());
    wait_for_label(cx, handle, "history-summary", "共 3 条 · ~/.bash_history").await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(DisconnectTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        )
    });
    let typed_before = factory.written_text();
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(EnterCommand {
                command: "ls -l".into(),
                run: true,
            }),
            cx,
        )
    });
    // Said in a notification, and nothing typed.
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.notifications(cx).len(), 1);
    });
    assert_eq!(factory.written_text(), typed_before);
}

#[gpui_kit::test]
async fn a_host_without_a_history_file_says_so(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&["@@missing\n"]));
    let handle = open_history(cx, factory);
    // Asked again once the shell runs.
    wait_for_label(
        cx,
        handle,
        "history-message",
        "这台主机上还没有 bash 的历史命令（~/.bash_history）。bash 退出时才会写入它。",
    )
    .await;
}

#[gpui_kit::test]
fn the_history_is_not_offered_on_windows(cx: &mut TestAppContext) {
    let mut store = HostStore::seed();
    store.set_host_os_unnotified(HostId(WEB_01), Some(HostOs::Windows));
    let (handle, _) = open_workspace_with_store(cx, store);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("tool-history").is_none());
        assert!(window.find("tool-snippets").visible());
    });
}
