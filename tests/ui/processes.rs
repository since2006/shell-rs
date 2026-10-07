//! 进程管理: the right sidebar's tool that lists the processes of the SSH
//! terminal's host, and ends them, over the terminal's own connection.

use gpui_kit::{SharedString, Window};
use shellrs::app::EndProcess;

use crate::support::*;

/// A small host: systemd, a java service and a node app, with 2 000 000 kB
/// of memory.
const FIRST: &str = "\
@@uptime
1000.00 3000.00
@@memory
2000000
@@clock
100
@@page
4096
@@users
0 root
1000 ecs-user
@@processes
0 1 (systemd) S 0 1 1 0 -1 4194560 0 0 0 0 150 80 0 0 20 0 1 0 5 172000000 3000 0
0 2202 (java) S 1 2202 2202 0 -1 4194560 0 0 0 0 1000 0 0 0 20 0 30 0 5000 900000000 120000 0
1000 1479 (node) R 1 1479 1479 0 -1 4194560 0 0 0 0 4000 0 0 0 20 0 11 0 9000 900000000 30000 0
";

/// Two seconds later: java used a tenth of a second of CPU, node a whole
/// second.
const SECOND: &str = "\
@@uptime
1002.00 3000.00
@@memory
2000000
@@clock
100
@@page
4096
@@users
0 root
1000 ecs-user
@@processes
0 1 (systemd) S 0 1 1 0 -1 4194560 0 0 0 0 150 80 0 0 20 0 1 0 5 172000000 3000 0
0 2202 (java) S 1 2202 2202 0 -1 4194560 0 0 0 0 1010 0 0 0 20 0 30 0 5000 900000000 120000 0
1000 1479 (node) R 1 1479 1479 0 -1 4194560 0 0 0 0 4100 0 0 0 20 0 11 0 9000 900000000 30000 0
";

const JAVA: u64 = 2202;
const NODE: u64 = 1479;
const SYSTEMD: u64 = 1;

fn open_processes(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
) -> WindowHandle<Root> {
    let (handle, _) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-processes", cx));
    handle
}

async fn wait_for_label(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    id: impl Into<ElementId> + Clone,
    label: &str,
) {
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find(id.clone())
            .is_some_and(|element| element.label() == Some(label))
    })
    .await;
}

/// The processes from the top of the list down.
fn listed(window: &mut Window) -> Vec<u64> {
    let mut cards: Vec<(f32, u64)> = [JAVA, NODE, SYSTEMD]
        .into_iter()
        .filter_map(|pid| {
            let card = window.try_find(("process", pid))?;
            Some((f32::from(card.bounds().origin.y), pid))
        })
        .collect();
    cards.sort_by(|a, b| a.0.total_cmp(&b.0));
    cards.into_iter().map(|(_, pid)| pid).collect()
}

#[gpui_kit::test]
async fn the_processes_show_by_memory_and_their_cpu_once_read_twice(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST, SECOND]));
    let handle = open_processes(cx, factory.clone());
    wait_for_label(
        cx,
        handle,
        "processes-summary",
        "共 3 个进程 · 每 15 秒刷新",
    )
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("tool-sidebar", INITIAL_WEB_TERMINAL)).label(),
            Some("进程管理")
        );
        assert_eq!(listed(window), [JAVA, NODE, SYSTEMD]);
        assert_eq!(window.find("processes-count").label(), Some("3"));
        assert_eq!(window.find("processes-sort-memory").checked(), Some(true));
    });

    // The second reading comes soon, and gives the CPU shares.
    wait_for_label(
        cx,
        handle,
        ("process", JAVA),
        "java · PID 2202 · 休眠 · root · 内存 468.75 MB · 24.0% · CPU 5.0%",
    )
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("process", NODE)).label(),
            Some("node · PID 1479 · 运行中 · ecs-user · 内存 117.19 MB · 6.0% · CPU 50.0%")
        );
    });
    assert_eq!(factory.exec_count(), 2);

    // Then every 15 seconds.
    cx.executor().advance_clock(Duration::from_secs(14));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), 2);
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), 3);
}

#[gpui_kit::test]
async fn the_processes_sort_by_cpu_and_back(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST, SECOND]));
    let handle = open_processes(cx, factory);
    wait_for_label(
        cx,
        handle,
        ("process", NODE),
        "node · PID 1479 · 运行中 · ecs-user · 内存 117.19 MB · 6.0% · CPU 50.0%",
    )
    .await;

    in_frame(cx, handle, |window, cx| {
        window.click("processes-sort-cpu", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("processes-sort-cpu").checked(), Some(true));
        assert_eq!(window.find("processes-sort-memory").checked(), Some(false));
        assert_eq!(listed(window), [NODE, JAVA, SYSTEMD]);
    });

    // Again: the least first.
    in_frame(cx, handle, |window, cx| {
        window.click("processes-sort-cpu", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(listed(window), [SYSTEMD, JAVA, NODE]);
    });

    in_frame(cx, handle, |window, cx| {
        window.click("processes-sort-memory", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(listed(window), [JAVA, NODE, SYSTEMD]);
    });
}

#[gpui_kit::test]
async fn the_search_finds_processes_by_name_pid_or_user(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST]));
    let handle = open_processes(cx, factory);
    wait_for_label(
        cx,
        handle,
        "processes-summary",
        "共 3 个进程 · 每 15 秒刷新",
    )
    .await;

    in_frame(cx, handle, |window, cx| {
        window.click("processes-search", cx);
        window.input("ecs", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(listed(window), [NODE]);
        assert_eq!(window.find("processes-count").label(), Some("1"));
        // The summary is still the host's.
        assert_eq!(
            window.find("processes-summary").label(),
            Some("共 3 个进程 · 每 15 秒刷新")
        );
    });

    in_frame(cx, handle, |window, cx| {
        window.press("cmd-a", cx);
        window.input("2202", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(listed(window), [JAVA]);
    });

    in_frame(cx, handle, |window, cx| {
        window.press("cmd-a", cx);
        window.input("nginx", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("processes-empty").label(),
            Some("没有符合条件的进程")
        );
    });
}

#[gpui_kit::test]
async fn ending_a_process_asks_first_then_reads_the_list_again(cx: &mut TestAppContext) {
    let after = SECOND
        .lines()
        .filter(|line| !line.contains("(java)"))
        .collect::<Vec<_>>()
        .join("\n");
    let factory = Arc::new(FakeTerminalFactory::answering(&[
        FIRST,
        SECOND,
        "@@status 0\n",
        &after,
    ]));
    let handle = open_processes(cx, factory.clone());
    wait_for_label(
        cx,
        handle,
        ("process", JAVA),
        "java · PID 2202 · 休眠 · root · 内存 468.75 MB · 24.0% · CPU 5.0%",
    )
    .await;

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(EndProcess {
                pid: 2202,
                force: false,
            }),
            cx,
        )
    });
    // Nothing is sent before the answer.
    assert_eq!(factory.exec_count(), 2);
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.try_find(("process", JAVA)).is_none()
    })
    .await;
    let commands = factory.exec_commands();
    assert_eq!(
        commands[2],
        "sh -c 'kill -TERM 2202 2>&1; echo @@status $?'"
    );
    assert_eq!(commands.len(), 4);
}

/// What `/proc/2202/cmdline` holds: the arguments, each ended by a NUL.
const JAVA_COMMAND: &str =
    "/usr/lib/jvm/java-21-openjdk-amd64/bin/java\0-Xms128m\0-jar\0opc-server-0.0.1.jar\0";

#[gpui_kit::test]
async fn a_click_on_a_process_shows_its_details_and_command_line(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[
        FIRST,
        SECOND,
        JAVA_COMMAND,
    ]));
    let handle = open_processes(cx, factory.clone());
    wait_for_label(
        cx,
        handle,
        ("process", JAVA),
        "java · PID 2202 · 休眠 · root · 内存 468.75 MB · 24.0% · CPU 5.0%",
    )
    .await;

    in_frame(cx, handle, |window, cx| window.click(("process", JAVA), cx));
    wait_for_label(
        cx,
        handle,
        "process-command",
        "/usr/lib/jvm/java-21-openjdk-amd64/bin/java -Xms128m -jar opc-server-0.0.1.jar",
    )
    .await;
    assert_eq!(factory.exec_commands()[2], "sh -c 'cat /proc/2202/cmdline'");
    in_frame(cx, handle, |window, _| {
        let field = |id: &str| {
            window
                .find(SharedString::from(format!("process-field:{id}")))
                .label()
                .map(str::to_owned)
        };
        for (id, value) in [
            ("pid", "2202"),
            ("parent", "systemd (1)"),
            ("user", "root"),
            // A session leader with threads.
            ("state", "休眠 (Ssl)"),
            // Up since tick 5000 of a host up 1002 s.
            ("running", "00:15:52"),
            ("terminal", "无"),
            ("priority", "20"),
            ("nice", "0"),
            ("cpu", "5.0%"),
            ("cpu_time", "00:00:10"),
            ("memory", "468.75 MB (24.0%)"),
            ("virtual_memory", "858.31 MB"),
            ("threads", "30"),
            ("children", "0"),
            ("descendants", "0"),
        ] {
            assert_eq!(field(id).as_deref(), Some(value), "{id}");
        }
    });

    in_frame(cx, handle, |window, cx| {
        window.click("process-copy-command", cx)
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(
            "/usr/lib/jvm/java-21-openjdk-amd64/bin/java -Xms128m -jar opc-server-0.0.1.jar".into()
        )
    );
    in_frame(cx, handle, |window, cx| {
        window.click("process-copy-pid", cx)
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("2202".into())
    );
}

#[gpui_kit::test]
async fn the_details_end_the_process_after_asking(cx: &mut TestAppContext) {
    let after = SECOND
        .lines()
        .filter(|line| !line.contains("(java)"))
        .collect::<Vec<_>>()
        .join("\n");
    let factory = Arc::new(FakeTerminalFactory::answering(&[
        FIRST,
        SECOND,
        JAVA_COMMAND,
        "@@status 0\n",
        &after,
    ]));
    let handle = open_processes(cx, factory.clone());
    wait_for_label(
        cx,
        handle,
        ("process", JAVA),
        "java · PID 2202 · 休眠 · root · 内存 468.75 MB · 24.0% · CPU 5.0%",
    )
    .await;
    in_frame(cx, handle, |window, cx| window.click(("process", JAVA), cx));
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.try_find("process-command").is_some()
    })
    .await;

    // The details give way to the question: one dialog at a time.
    in_frame(cx, handle, |window, cx| {
        window.click("process-force-end", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("process-details").is_none());
    });
    assert_eq!(factory.exec_count(), 3);
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.try_find(("process", JAVA)).is_none()
    })
    .await;
    assert_eq!(
        factory.exec_commands()[3],
        "sh -c 'kill -KILL 2202 2>&1; echo @@status $?'"
    );
}

#[gpui_kit::test]
async fn the_processes_are_read_only_while_they_show(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST, SECOND]));
    let handle = open_processes(cx, factory.clone());
    wait_for_label(
        cx,
        handle,
        ("process", JAVA),
        "java · PID 2202 · 休眠 · root · 内存 468.75 MB · 24.0% · CPU 5.0%",
    )
    .await;

    in_frame(cx, handle, |window, cx| window.click("tool-processes", cx));
    let hidden = factory.exec_count();
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), hidden);

    // Back, the list it had shows at once while it is read again.
    in_frame(cx, handle, |window, cx| window.click("tool-processes", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(listed(window), [JAVA, NODE, SYSTEMD]);
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() > hidden
    })
    .await;
}

#[gpui_kit::test]
async fn the_processes_wait_for_the_terminal_to_connect(cx: &mut TestAppContext) {
    // web-01's terminal is the first started, and exits at once.
    let handle = open_processes(cx, Arc::new(FakeTerminalFactory::exit_first()));
    wait_for_label(
        cx,
        handle,
        "processes-message",
        "终端没有连接。连接后这里显示主机的进程。",
    )
    .await;
}

#[gpui_kit::test]
async fn the_processes_say_they_read_linux_hosts_only(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&["@@unsupported\nDarwin\n"]));
    let handle = open_processes(cx, factory);
    wait_for_label(
        cx,
        handle,
        "processes-message",
        "暂不支持管理 Darwin 的进程，目前只支持 Linux 主机。",
    )
    .await;
}
