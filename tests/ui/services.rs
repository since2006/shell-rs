//! 系统服务: the right sidebar's tool that lists the systemd services of the
//! SSH terminal's host, runs commands on them and shows their details,
//! over the terminal's own connection.

use gpui_kit::{SharedString, Window};

use crate::support::*;

/// A host with one service of the administrator's, two of the
/// distribution's and one that failed.
const LIST: &str = "\
@@version
systemd 249 (249.11-0ubuntu3.12)
@@state
degraded
@@units
Id=socialc.service
Description=Social Callback Proxy Service
ActiveState=active
UnitFileState=enabled
FragmentPath=/etc/systemd/system/socialc.service
LoadState=loaded

Id=nginx.service
Description=A high performance web server
ActiveState=active
UnitFileState=enabled
FragmentPath=/lib/systemd/system/nginx.service
LoadState=loaded

Id=ModemManager.service
Description=Modem Manager
ActiveState=inactive
UnitFileState=disabled
FragmentPath=/lib/systemd/system/ModemManager.service
LoadState=loaded

Id=certbot.service
Description=Certbot
ActiveState=failed
UnitFileState=static
FragmentPath=/lib/systemd/system/certbot.service
LoadState=loaded
";

/// nginx in full, as `systemctl show` gives it.
const NGINX: &str = "\
Id=nginx.service
Description=A high performance web server
LoadState=loaded
ActiveState=active
SubState=running
UnitFileState=enabled
FragmentPath=/lib/systemd/system/nginx.service
MainPID=781
MemoryCurrent=1990656
TasksCurrent=5
NRestarts=0
ExecMainStatus=0
ActiveEnterTimestamp=Tue 2026-09-15 17:49:07 CST
InactiveEnterTimestamp=
";

const JOURNAL: &str = "\
Sep 15 17:49:07 web-01 systemd[1]: Starting A high performance web server...
Sep 15 17:49:07 web-01 systemd[1]: Started A high performance web server.
";

fn open_services(cx: &mut TestAppContext, factory: Arc<FakeTerminalFactory>) -> WindowHandle<Root> {
    let (handle, _) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-services", cx));
    handle
}

async fn wait_for_label(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    id: &str,
    label: &str,
) {
    let id = SharedString::from(id.to_owned());
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find(id.clone())
            .is_some_and(|element| element.label() == Some(label))
    })
    .await;
}

async fn wait_for_list(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    wait_for_label(cx, handle, "services-summary", "systemd 249 · 降级运行").await;
}

/// The lines from the top of the list down, of those named.
fn listed(window: &mut Window, ids: &[&str]) -> Vec<String> {
    let mut lines: Vec<(f32, String)> = ids
        .iter()
        .filter_map(|id| {
            let element = window.try_find(SharedString::from(id.to_string()))?;
            Some((f32::from(element.bounds().origin.y), id.to_string()))
        })
        .collect();
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    lines.into_iter().map(|(_, id)| id).collect()
}

const LINES: [&str; 6] = [
    "services-group:custom",
    "service:socialc.service",
    "services-group:system",
    "service:ModemManager.service",
    "service:certbot.service",
    "service:nginx.service",
];

#[gpui_kit::test]
async fn the_administrators_services_come_first(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[LIST]));
    let handle = open_services(cx, factory.clone());
    wait_for_list(cx, handle).await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("tool-sidebar", INITIAL_WEB_TERMINAL)).label(),
            Some("系统服务")
        );
        assert_eq!(listed(window, &LINES), LINES);
        assert_eq!(
            window.find("services-group:custom").label(),
            Some("自定义服务 1")
        );
        assert_eq!(
            window.find("services-group:system").label(),
            Some("系统服务 3")
        );
        assert_eq!(
            window.find("service:socialc.service").label(),
            Some("socialc.service · Social Callback Proxy Service · 运行中 · 已启用")
        );
        // A running service can be stopped and restarted, a stopped one
        // started.
        assert!(window.try_find("service-stop:nginx.service").is_some());
        assert!(window.try_find("service-restart:nginx.service").is_some());
        assert!(window.try_find("service-start:nginx.service").is_none());
        assert!(
            window
                .try_find("service-start:ModemManager.service")
                .is_some()
        );
    });

    // Read once: services change seldom.
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), 1);
}

#[gpui_kit::test]
async fn the_tabs_and_the_search_narrow_the_list(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[LIST]));
    let handle = open_services(cx, factory);
    wait_for_list(cx, handle).await;

    // 失败.
    in_frame(cx, handle, |window, cx| {
        window.within("services-tabs").click(3usize, cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            listed(window, &LINES),
            ["services-group:system", "service:certbot.service"]
        );
    });

    // 全部, then a search.
    in_frame(cx, handle, |window, cx| {
        window.within("services-tabs").click(0usize, cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("services-search", cx);
        window.input("modem", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            listed(window, &LINES),
            ["services-group:system", "service:ModemManager.service"]
        );
    });

    in_frame(cx, handle, |window, cx| {
        window.press("cmd-a", cx);
        window.input("nothing-like-it", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("services-empty").label(),
            Some("没有符合条件的服务")
        );
    });
}

#[gpui_kit::test]
async fn stopping_a_service_asks_first_and_starting_one_does_not(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[
        LIST,
        "@@status 0\n",
        LIST,
        "@@status 0\n",
        LIST,
    ]));
    let handle = open_services(cx, factory.clone());
    wait_for_list(cx, handle).await;

    in_frame(cx, handle, |window, cx| {
        window.click("service-stop:nginx.service", cx)
    });
    // Nothing runs before the answer, and the card's own click does not
    // open the details.
    assert_eq!(factory.exec_count(), 1);
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("service-details").is_none());
        window.click("ok", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() == 3
    })
    .await;
    let commands = factory.exec_commands();
    assert!(
        commands[1].contains("systemctl stop -- \"nginx.service\""),
        "{}",
        commands[1]
    );

    in_frame(cx, handle, |window, cx| {
        window.click("service-start:ModemManager.service", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() == 5
    })
    .await;
    assert!(factory.exec_commands()[3].contains("systemctl start -- \"ModemManager.service\""));
}

#[gpui_kit::test]
async fn a_click_on_a_service_shows_its_state_and_journal(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[LIST, NGINX, JOURNAL]));
    let handle = open_services(cx, factory.clone());
    wait_for_list(cx, handle).await;

    in_frame(cx, handle, |window, cx| {
        window.click("service:nginx.service", cx)
    });
    wait_for_label(cx, handle, "service-field:main_pid", "781").await;
    in_frame(cx, handle, |window, _| {
        let field = |id: &str| {
            window
                .find(SharedString::from(format!("service-field:{id}")))
                .label()
                .map(str::to_owned)
        };
        for (id, value) in [
            ("load", "已加载"),
            ("active", "运行中 / 运行中"),
            ("boot", "已启用"),
            ("memory", "1.90 MB"),
            ("tasks", "5"),
            ("restarts", "0"),
            ("exit_status", "0"),
            ("started", "Tue 2026-09-15 17:49:07 CST"),
            ("stopped", "—"),
            ("unit_file", "/lib/systemd/system/nginx.service"),
        ] {
            assert_eq!(field(id).as_deref(), Some(value), "{id}");
        }
        assert!(window.try_find("service-dialog-stop").is_some());
        assert!(window.try_find("service-dialog-disable").is_some());
    });
    assert!(factory.exec_commands()[1].contains("systemctl show"));

    // The journal is read when its tab opens.
    assert_eq!(factory.exec_count(), 2);
    in_frame(cx, handle, |window, cx| {
        window.within("service-tabs").click(1usize, cx)
    });
    wait_for_label(cx, handle, "service-journal", JOURNAL.trim_end()).await;
    assert!(factory.exec_commands()[2].contains("journalctl -u \"nginx.service\""));
}

#[gpui_kit::test]
async fn the_services_say_when_systemd_does_not_run_the_host(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&["@@unsupported\nLinux\n"]));
    let handle = open_services(cx, factory);
    wait_for_label(
        cx,
        handle,
        "services-message",
        "这台主机没有使用 systemd，暂不支持管理它的服务。",
    )
    .await;
}

#[gpui_kit::test]
async fn the_services_wait_for_the_terminal_to_connect(cx: &mut TestAppContext) {
    // web-01's terminal is the first started, and exits at once.
    let handle = open_services(cx, Arc::new(FakeTerminalFactory::exit_first()));
    wait_for_label(
        cx,
        handle,
        "services-message",
        "终端没有连接。连接后这里显示主机的系统服务。",
    )
    .await;
}
