//! 网络连接: the right sidebar's tool that lists the sockets of the SSH
//! terminal's host, read over the terminal's own connection.

use crate::support::*;

/// What `ss` says about a small web server, read as root.
const FIRST: &str = "\
@@uid
0
@@users
0 root
33 www-data
@@ss
udp   UNCONN 0 0 127.0.0.53%lo:53 0.0.0.0:* users:((\"systemd-resolve\",pid=600,fd=13)) uid:101 ino:20123 sk:1 <->
tcp   LISTEN 0 4096 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=891,fd=3)) ino:21233 sk:5 <->
tcp   LISTEN 0 511 0.0.0.0:80 0.0.0.0:* users:((\"nginx\",pid=913756,fd=6)) uid:33 ino:30001 sk:6 <->
tcp   ESTAB 0 0 10.0.0.5:22 203.0.113.7:51234 users:((\"sshd\",pid=1234,fd=4)) ino:123456 sk:9 <->
tcp   TIME-WAIT 0 0 10.0.0.5:80 198.51.100.2:6000 timer:(timewait,50sec,0) ino:0 sk:a
@@counts
1 udp UNCONN
2 tcp LISTEN
1 tcp ESTAB
1 tcp TIME-WAIT
";

/// The same server a moment later: the visit to port 80 has gone, another
/// login has come.
const SECOND: &str = "\
@@uid
0
@@users
0 root
@@ss
tcp   LISTEN 0 4096 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=891,fd=3)) ino:21233 sk:5 <->
tcp   ESTAB 0 0 10.0.0.5:22 203.0.113.7:51234 users:((\"sshd\",pid=1234,fd=4)) ino:123456 sk:9 <->
tcp   ESTAB 0 0 10.0.0.5:22 203.0.113.8:40000 users:((\"sshd\",pid=1300,fd=4)) ino:123457 sk:b <->
@@counts
1 tcp LISTEN
2 tcp ESTAB
";

fn open_connections(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
) -> WindowHandle<Root> {
    let (handle, _) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("tool-connections", cx)
    });
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

#[gpui_kit::test]
async fn the_connections_list_the_hosts_sockets_with_their_processes(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST]));
    let handle = open_connections(cx, factory.clone());
    wait_for_label(
        cx,
        handle,
        "netstat-summary",
        "共 5 条 · 3 个监听端口 · 1 条已连接",
    )
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("tool-sidebar", INITIAL_WEB_TERMINAL)).label(),
            Some("网络连接")
        );
        assert_eq!(window.find("tool-connections").checked(), Some(true));
        assert_eq!(
            window
                .find("netstat-socket:tcp 0.0.0.0:22 0.0.0.0:*")
                .label(),
            Some("TCP 监听 · 本地 0.0.0.0:22 · 远端 0.0.0.0:* · sshd (PID 891) · root")
        );
        assert_eq!(
            window
                .find("netstat-socket:tcp 0.0.0.0:80 0.0.0.0:*")
                .label(),
            Some("TCP 监听 · 本地 0.0.0.0:80 · 远端 0.0.0.0:* · nginx (PID 913756) · www-data")
        );
        // No process holds a socket in TIME_WAIT.
        assert_eq!(
            window
                .find("netstat-socket:tcp 10.0.0.5:80 198.51.100.2:6000")
                .label(),
            Some("TCP TIME_WAIT · 本地 10.0.0.5:80 · 远端 198.51.100.2:6000 · — · —")
        );
        // Root sees everything, and the list is whole: nothing to say.
        assert!(window.try_find("netstat-truncated").is_none());
        assert!(window.try_find("netstat-processes").is_none());
        assert!(window.try_find("netstat-matches").is_none());
    });
    let command = &factory.exec_commands()[0];
    assert!(command.starts_with("sh -c '") && command.contains("ss -tuanpe"));

    // A list holds still: nothing more runs until it is asked to.
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), 1);
}

#[gpui_kit::test]
async fn the_search_narrows_the_list(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST]));
    let handle = open_connections(cx, factory);
    wait_for_label(
        cx,
        handle,
        "netstat-summary",
        "共 5 条 · 3 个监听端口 · 1 条已连接",
    )
    .await;

    in_frame(cx, handle, |window, cx| {
        window.click("netstat-search", cx);
        window.input("sshd", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("netstat-matches").label(), Some("筛选出 2 条"));
        assert!(
            window
                .try_find("netstat-socket:tcp 0.0.0.0:22 0.0.0.0:*")
                .is_some()
        );
        assert!(
            window
                .try_find("netstat-socket:tcp 10.0.0.5:22 203.0.113.7:51234")
                .is_some()
        );
        assert!(
            window
                .try_find("netstat-socket:tcp 0.0.0.0:80 0.0.0.0:*")
                .is_none()
        );
        // The summary is still the host's.
        assert_eq!(
            window.find("netstat-summary").label(),
            Some("共 5 条 · 3 个监听端口 · 1 条已连接")
        );
    });

    // A PID finds its process.
    in_frame(cx, handle, |window, cx| {
        window.press("cmd-a", cx);
        window.input("913756", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("netstat-matches").label(), Some("筛选出 1 条"));
        assert!(
            window
                .try_find("netstat-socket:tcp 0.0.0.0:80 0.0.0.0:*")
                .is_some()
        );
    });

    in_frame(cx, handle, |window, cx| {
        window.press("cmd-a", cx);
        window.input("mysql", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("netstat-empty").label(),
            Some("没有符合条件的连接")
        );
    });
}

#[gpui_kit::test]
async fn refresh_reads_the_host_again(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST, SECOND]));
    let handle = open_connections(cx, factory.clone());
    wait_for_label(
        cx,
        handle,
        "netstat-summary",
        "共 5 条 · 3 个监听端口 · 1 条已连接",
    )
    .await;

    in_frame(cx, handle, |window, cx| window.click("netstat-refresh", cx));
    wait_for_label(
        cx,
        handle,
        "netstat-summary",
        "共 3 条 · 1 个监听端口 · 2 条已连接",
    )
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .try_find("netstat-socket:tcp 10.0.0.5:22 203.0.113.8:40000")
                .is_some()
        );
        assert!(
            window
                .try_find("netstat-socket:tcp 0.0.0.0:80 0.0.0.0:*")
                .is_none()
        );
    });
    assert_eq!(factory.exec_count(), 2);
}

#[gpui_kit::test]
async fn the_connections_are_read_each_time_they_come_on_screen_and_only_then(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST, SECOND]));
    let handle = open_connections(cx, factory.clone());
    wait_for_label(
        cx,
        handle,
        "netstat-summary",
        "共 5 条 · 3 个监听端口 · 1 条已连接",
    )
    .await;

    // Another tool: nothing runs.
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), 1);

    // Back, the list it had shows at once while it is read again.
    in_frame(cx, handle, |window, cx| {
        window.click("tool-connections", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .try_find("netstat-socket:tcp 0.0.0.0:80 0.0.0.0:*")
                .is_some()
        );
    });
    wait_for_label(
        cx,
        handle,
        "netstat-summary",
        "共 3 条 · 1 个监听端口 · 2 条已连接",
    )
    .await;
    assert_eq!(factory.exec_count(), 2);
}

#[gpui_kit::test]
async fn the_connections_say_what_the_login_cannot_see_and_when_there_are_too_many(
    cx: &mut TestAppContext,
) {
    let output = FIRST
        .replacen("@@uid\n0\n", "@@uid\n1000\n", 1)
        .replace("1 tcp TIME-WAIT", "50000 tcp TIME-WAIT");
    let handle = open_connections(cx, Arc::new(FakeTerminalFactory::answering(&[&output])));
    wait_for_label(
        cx,
        handle,
        "netstat-summary",
        "共 50004 条 · 3 个监听端口 · 1 条已连接",
    )
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("netstat-truncated").label(),
            Some("连接太多，只列出了前 3000 条。")
        );
        assert_eq!(
            window.find("netstat-processes").label(),
            Some("当前用户不是 root，只能看到自己的进程。")
        );
    });
}

#[gpui_kit::test]
async fn the_connections_wait_for_the_terminal_to_connect(cx: &mut TestAppContext) {
    // web-01's terminal is the first started, and exits at once.
    let handle = open_connections(cx, Arc::new(FakeTerminalFactory::exit_first()));
    wait_for_label(
        cx,
        handle,
        "netstat-message",
        "终端没有连接。连接后这里显示主机的网络连接。",
    )
    .await;
}

#[gpui_kit::test]
async fn the_connections_say_they_read_linux_hosts_only(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&["@@unsupported\nDarwin\n"]));
    let handle = open_connections(cx, factory);
    wait_for_label(
        cx,
        handle,
        "netstat-message",
        "暂不支持查看 Darwin 的网络连接，目前只支持 Linux 主机。",
    )
    .await;
}
