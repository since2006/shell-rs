//! 系统监控: the right sidebar's tool that reads the SSH terminal's host
//! over the terminal's own connection.

use crate::support::*;

/// A two-core Debian host, then the same host a moment later: 100 more busy
/// ticks of 200, and some traffic on eth0.
const FIRST: &str = "\
@@host
web-01-vm
@@arch
x86_64
@@os
Debian GNU/Linux 12 (bookworm)
@@uptime
5359162.33 1.00
@@cpu
model name\t: AMD EPYC-Milan Processor
@@stat
cpu  100 0 100 800 0 0 0 0
cpu0 50 0 50 400 0 0 0 0
cpu1 50 0 50 400 0 0 0 0
@@memory
MemTotal:         996608 kB
MemAvailable:     565480 kB
SwapTotal:             0 kB
SwapFree:              0 kB
@@net
    lo: 10 0 0 0 0 0 0 0 10 0 0 0 0 0 0 0
  eth0: 1000 0 0 0 0 0 0 0 500 0 0 0 0 0 0 0
docker0: 50000 0 0 0 0 0 0 0 50000 0 0 0 0 0 0 0
@@route
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000
@@df
Filesystem     1024-blocks    Used Available Capacity Mounted on
/dev/vda1          9656904 3240180   5994708      36% /
tmpfs                99664     560     99104       1% /run
";

const SECOND: &str = "\
@@host
web-01-vm
@@arch
x86_64
@@os
Debian GNU/Linux 12 (bookworm)
@@uptime
5359164.33 1.00
@@cpu
model name\t: AMD EPYC-Milan Processor
@@stat
cpu  150 0 150 900 0 0 0 0
cpu0 100 0 50 400 0 0 0 0
cpu1 50 0 100 500 0 0 0 0
@@memory
MemTotal:         996608 kB
MemAvailable:     565480 kB
SwapTotal:             0 kB
SwapFree:              0 kB
@@net
    lo: 10 0 0 0 0 0 0 0 10 0 0 0 0 0 0 0
  eth0: 9192 0 0 0 0 0 0 0 4596 0 0 0 0 0 0 0
docker0: 60000 0 0 0 0 0 0 0 60000 0 0 0 0 0 0 0
@@route
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000
@@df
Filesystem     1024-blocks    Used Available Capacity Mounted on
/dev/vda1          9656904 3240180   5994708      36% /
tmpfs                99664     560     99104       1% /run
";

/// The seeded workspace with web-01's terminal in front and the monitor
/// open; every terminal's commands are answered by `factory`.
fn open_monitor(cx: &mut TestAppContext, factory: Arc<FakeTerminalFactory>) -> WindowHandle<Root> {
    let (handle, _) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
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
async fn the_monitor_shows_the_hosts_cpu_memory_network_and_disks(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST, SECOND]));
    let handle = open_monitor(cx, factory.clone());

    // The load is the second reading's against the first.
    wait_for_label(cx, handle, "monitor-cpu-usage", "50.0%").await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("monitor-host").label(), Some("web-01-vm"));
        assert_eq!(window.find("monitor-arch").label(), Some("x86_64"));
        assert_eq!(
            window.find("monitor-os").label(),
            Some("Debian GNU/Linux 12 (bookworm)")
        );
        // The second reading's uptime: 62 days and 2364 seconds.
        assert_eq!(
            window.find("monitor-uptime").label(),
            Some("62 天 00:39:24")
        );
        // Two cores fit the row shown at once: nothing to unfold.
        assert_eq!(
            window.find("monitor-cpu-cores").label(),
            Some("核 0：100.0%，核 1：33.3%")
        );
        assert!(window.try_find("monitor-cores-toggle").is_none());
        assert_eq!(
            window.find("monitor-memory-amount").label(),
            Some("421.02 MB / 973.25 MB")
        );
        // No swap, no swap row.
        assert!(window.try_find("monitor-swap-usage").is_none());
        // The main interface only, though docker0 carried more: the
        // default route goes out of eth0.
        let eth0 = window.find("monitor-interface:eth0");
        assert!(
            eth0.label()
                .is_some_and(|label| label.starts_with("上传 ") && !label.contains('—')),
            "{eth0:?}"
        );
        assert!(window.try_find("monitor-interface:docker0").is_none());
        assert_eq!(
            window.find("monitor-interfaces-toggle").checked(),
            Some(false)
        );
        // Loopback is not worth a row, nor is a memory file system a disk.
        assert!(window.try_find("monitor-interface:lo").is_none());
        assert_eq!(
            window.find("monitor-disk:/").label(),
            Some("35.1%，3.09 GB / 9.21 GB")
        );
        assert!(window.try_find("monitor-disk:/run").is_none());
    });

    // Unfolded, every interface; and folded away again.
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-interfaces-toggle", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("monitor-interfaces-toggle").checked(),
            Some(true)
        );
        assert!(window.try_find("monitor-interface:eth0").is_some());
        assert!(window.try_find("monitor-interface:docker0").is_some());
    });
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-interfaces-toggle", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("monitor-interface:docker0").is_none());
    });

    // The first reading asked for everything; the next one for the load
    // only, which starts no process on the host but its `sh`.
    let commands = factory.exec_commands();
    assert!(commands[0].contains("@@host") && commands[0].contains("@@df"));
    assert!(!commands[1].contains("@@host") && !commands[1].contains("@@df"));
}

#[gpui_kit::test]
async fn the_monitor_reads_the_host_only_while_it_shows(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[FIRST]));
    let handle = open_monitor(cx, factory.clone());
    wait_for_label(cx, handle, "monitor-host", "web-01-vm").await;

    // Hidden: nothing more runs on the host.
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
    let hidden = factory.exec_count();
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), hidden);

    // Back on screen, it reads again, and shows what it had meanwhile.
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("monitor-host").label(), Some("web-01-vm"));
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() > hidden
    })
    .await;

    // Nor while another tool is showing.
    in_frame(cx, handle, |window, cx| window.click("tool-history", cx));
    let other = factory.exec_count();
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), other);
}

#[gpui_kit::test]
async fn the_monitor_says_when_the_terminal_is_not_connected(cx: &mut TestAppContext) {
    // web-01's terminal is the first started, and exits at once.
    let handle = open_monitor(cx, Arc::new(FakeTerminalFactory::exit_first()));
    wait_for_label(
        cx,
        handle,
        "monitor-message",
        "终端没有连接。连接后这里显示主机的 CPU、内存、网络和磁盘。",
    )
    .await;
}

#[gpui_kit::test]
async fn the_monitor_says_it_reads_linux_hosts_only(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&["@@unsupported\nDarwin\n"]));
    let handle = open_monitor(cx, factory);
    wait_for_label(
        cx,
        handle,
        "monitor-message",
        "暂不支持 Darwin 的监控，目前只支持 Linux 主机。",
    )
    .await;
}

#[gpui_kit::test]
fn the_monitor_is_not_offered_on_a_host_known_not_to_run_linux(cx: &mut TestAppContext) {
    let mut store = HostStore::seed();
    store.set_host_os_unnotified(HostId(WEB_01), Some(HostOs::Debian));
    store.set_host_os_unnotified(HostId(STAGING_API), Some(HostOs::MacOs));
    let (handle, _) = open_workspace_with_store(cx, store);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-monitor", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("tool-sidebar", INITIAL_WEB_TERMINAL)).label(),
            Some("系统监控")
        );
    });

    // On the Mac the monitor has no button, and the sidebar showing it
    // goes; the other tools stay on offer.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("tool-monitor").is_none());
        assert!(window.try_find("tool-connections").is_none());
        assert!(window.try_find("tool-processes").is_none());
        assert!(window.find("tool-history").visible());
        assert!(
            window
                .try_find(("tool-sidebar", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
    });

    // Back on the Linux host it is as it was.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("tool-monitor").checked(), Some(true));
        assert!(
            window
                .try_find(("tool-sidebar", INITIAL_WEB_TERMINAL))
                .is_some()
        );
    });

    // The shortcut on the Mac shows the first tool it has instead.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ToggleToolSidebar), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window
                .find(("tool-sidebar", INITIAL_STAGING_TERMINAL))
                .label(),
            Some("命令片段")
        );
    });
}

#[gpui_kit::test]
async fn the_monitor_goes_once_the_host_turns_out_not_to_run_linux(cx: &mut TestAppContext) {
    // Every terminal reports a Mac as soon as it connects.
    let (handle, workspace) = open_workspace_with_remote_factory(
        cx,
        HostStore::seed(),
        Arc::new(FakeTerminalFactory::reports_os(HostOs::MacOs)),
    );
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("tool-monitor").is_none()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.find("tool-snippets").visible());
    });
    let os = workspace.read_with(cx, |workspace, cx| {
        workspace
            .store()
            .read(cx)
            .host(HostId(WEB_01))
            .and_then(|host| host.os)
    });
    assert_eq!(os, Some(HostOs::MacOs));
}

/// A host with `cores` cores, every one a little busier than the last.
fn many_cores(cores: u64, busy: u64) -> String {
    let lines: String = (0..cores)
        .map(|core| format!("cpu{core} {} 0 0 1000 0 0 0 0\n", busy * core))
        .collect();
    FIRST.replace(
        "cpu  100 0 100 800 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0\ncpu1 50 0 50 400 0 0 0 0\n",
        &format!(
            "cpu  {} 0 0 {} 0 0 0 0\n{lines}",
            busy * cores * cores / 2,
            1000 * cores
        ),
    )
}

#[gpui_kit::test]
async fn sixty_four_cores_show_one_row_until_unfolded(cx: &mut TestAppContext) {
    let (first, second) = (many_cores(64, 1), many_cores(64, 2));
    let factory = Arc::new(FakeTerminalFactory::answering(&[&first, &second]));
    let handle = open_monitor(cx, factory);
    // A reading against the one before, so there are loads to draw.
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window
            .try_find("monitor-cpu-usage")
            .is_some_and(|usage| usage.label() != Some("—"))
    })
    .await;
    // The first frame measures the row; the next one fills it.
    in_frame(cx, handle, |window, cx| window.simulate_next_frame(cx));
    let bars = |cx: &mut TestAppContext| {
        in_frame(cx, handle, |window, _| {
            (0..64_u64)
                .filter_map(|core| window.try_find(("monitor-core", core)))
                .map(|bar| bar.bounds())
                .collect::<Vec<_>>()
        })
    };
    let rows = |bars: &[gpui_kit::Bounds<gpui_kit::Pixels>]| {
        let mut tops: Vec<_> = bars.iter().map(|bar| bar.top()).collect();
        tops.dedup();
        tops.len()
    };

    // One row at first: as many as fit beside the gauge.
    let first_row = bars(cx);
    assert_eq!(first_row.len(), 23);
    assert_eq!(rows(&first_row), 1);
    in_frame(cx, handle, |window, _| {
        let toggle = window.find("monitor-cores-toggle");
        assert_eq!(toggle.label(), Some("64 核"));
        assert_eq!(toggle.checked(), Some(false));
    });

    // Unfolded, all of them, wrapping onto three rows inside the column;
    // the share sits at the column's right edge.
    in_frame(cx, handle, |window, cx| {
        window.click("monitor-cores-toggle", cx)
    });
    let all = bars(cx);
    assert_eq!(all.len(), 64);
    assert_eq!(rows(&all), 3);
    in_frame(cx, handle, |window, _| {
        let column_right = window.find("monitor-cpu-usage").bounds().right();
        assert!(all.iter().all(|bar| bar.right() <= column_right));
    });
}
