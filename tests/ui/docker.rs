//! Docker: the right sidebar's tool that lists the SSH terminal's host's
//! containers by compose project, with its volumes, images and networks,
//! over the terminal's own connection.

use gpui_kit::{SharedString, Window};

use crate::support::*;

/// A host with a running compose project, a stopped one, and two
/// containers of no project.
const READING: &str = r#"@@version
26.0.0
@@compose
v2.25.0
@@containers
{"ID":"aaa111","Image":"php:5.6-fpm","Labels":"com.docker.compose.project=php-56,com.docker.compose.project.working_dir=/srv/php-5.6,com.docker.compose.service=php56","Mounts":"","Names":"php56","Networks":"php-56_default","Ports":"0.0.0.0:56000->9000/tcp, :::56000->9000/tcp","State":"running","Status":"Up 3 days"}
{"ID":"bbb222","Image":"vaultwarden/server:latest","Labels":"com.docker.compose.project=vaultwarden,com.docker.compose.project.working_dir=/srv/vaultwarden","Mounts":"vw-data","Names":"vaultwarden","Networks":"vaultwarden_default","Ports":"","State":"exited","Status":"Exited (0) 2 days ago"}
{"ID":"ccc333","Image":"forex-web:latest","Labels":"","Mounts":"","Names":"forex-web","Networks":"bridge","Ports":"127.0.0.1:4321->4321/tcp","State":"running","Status":"Up 2 hours"}
{"ID":"ddd444","Image":"hello-world","Labels":"","Mounts":"","Names":"exciting_tesla","Networks":"bridge","Ports":"","State":"exited","Status":"Exited (0) 3 weeks ago"}
@@volumes
{"Driver":"local","Mountpoint":"/var/lib/docker/volumes/vw-data/_data","Name":"vw-data"}
{"Driver":"local","Mountpoint":"/var/lib/docker/volumes/old/_data","Name":"old"}
@@images
{"CreatedAt":"2026-09-07 10:59:16 +0800 CST","ID":"4a3b5c6d7e8f","Repository":"hello-world","Size":"13.3kB","Tag":"latest"}
@@networks
{"Driver":"bridge","ID":"n1","Name":"bridge","Scope":"local"}
"#;

const INSPECT: &str = r#"[{"Id":"ccc333","Name":"/forex-web","Created":"2026-09-07T10:59:16.371325789Z",
"State":{"Status":"running"},
"Config":{"Image":"forex-web:latest","Entrypoint":["docker-entrypoint.sh"],"Cmd":["node","./dist/server/entry.mjs"],
"Env":["TZ=Asia/Shanghai","PORT=4321"],"Labels":{}},
"NetworkSettings":{"Ports":{"4321/tcp":[{"HostIp":"127.0.0.1","HostPort":"4321"}]}},
"Mounts":[]}]"#;

const OUTPUT: &str = "2026-09-07T10:59:17Z Listening on http://0.0.0.0:4321\n";

fn open_docker(cx: &mut TestAppContext, factory: Arc<FakeTerminalFactory>) -> WindowHandle<Root> {
    let (handle, _) = open_workspace_with_remote_factory(cx, HostStore::seed(), factory);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-docker", cx));
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

async fn wait_for_reading(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    wait_for_label(
        cx,
        handle,
        "docker-summary",
        "Docker 26.0.0 · Compose v2.25.0",
    )
    .await;
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
    "docker-project:php-56",
    "docker-container:php56",
    "docker-project:vaultwarden",
    "docker-container:vaultwarden",
    "docker-container:exciting_tesla",
    "docker-container:forex-web",
];

#[gpui_kit::test]
async fn containers_show_by_project_and_the_rest_on_their_own(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[READING]));
    let handle = open_docker(cx, factory.clone());
    wait_for_reading(cx, handle).await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("tool-sidebar", INITIAL_WEB_TERMINAL)).label(),
            Some("Docker")
        );
        // The running project of one container is unfolded; the stopped one
        // is not.
        assert_eq!(
            listed(window, &LINES),
            [
                "docker-project:php-56",
                "docker-container:php56",
                "docker-project:vaultwarden",
                "docker-container:exciting_tesla",
                "docker-container:forex-web",
            ]
        );
        assert_eq!(
            window.find("docker-project:php-56").label(),
            Some("php-56 · 1/1 运行中")
        );
        assert_eq!(
            window.find("docker-container:php56").label(),
            Some("php56 · 运行中 · 56000→9000/tcp, :::56000→9000/tcp")
        );
        assert_eq!(
            window.find("docker-container:exciting_tesla").label(),
            Some("exciting_tesla · 已停止 · hello-world")
        );
        assert!(window.try_find("docker-stop:project:php-56").is_some());
        assert!(
            window
                .try_find("docker-start:project:vaultwarden")
                .is_some()
        );
        assert_eq!(
            window.within("docker-tabs").find(0usize).label(),
            Some("容器 4")
        );
        assert_tabs_share_the_width(window, "docker-tabs", 4);
    });

    // Unfold the stopped project, fold the running one.
    in_frame(cx, handle, |window, cx| {
        window.click("docker-project-line:vaultwarden", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("docker-project-line:php-56", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            listed(window, &LINES),
            [
                "docker-project:php-56",
                "docker-project:vaultwarden",
                "docker-container:vaultwarden",
                "docker-container:exciting_tesla",
                "docker-container:forex-web",
            ]
        );
    });

    // Read once: nothing changes on a timer.
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    assert_eq!(factory.exec_count(), 1);
}

#[gpui_kit::test]
async fn the_other_tabs_list_volumes_images_and_networks(cx: &mut TestAppContext) {
    let handle = open_docker(cx, Arc::new(FakeTerminalFactory::answering(&[READING])));
    wait_for_reading(cx, handle).await;

    in_frame(cx, handle, |window, cx| {
        window.within("docker-tabs").click(1usize, cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("docker-volume:vw-data").label(),
            Some("vw-data · local · /var/lib/docker/volumes/vw-data/_data · 使用中")
        );
        // What a container uses cannot be removed; what nothing uses can.
        assert!(
            window
                .find("docker-volume:vw-data-remove")
                .focused()
                .is_none()
        );
        assert!(window.find("docker-volume:old-remove").focused().is_some());
        assert!(window.try_find("docker-container:php56").is_none());
    });

    in_frame(cx, handle, |window, cx| {
        window.within("docker-tabs").click(2usize, cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("docker-image:4a3b5c6d7e8f").label(),
            Some("hello-world:latest · 4a3b5c6d7e8f · 13.3 kB · 2026-09-07 · 使用中")
        );
    });

    in_frame(cx, handle, |window, cx| {
        window.within("docker-tabs").click(3usize, cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("docker-network:bridge").label(),
            Some("bridge · bridge · local · 内置")
        );
    });
}

#[gpui_kit::test]
async fn stopping_a_project_asks_first_and_starting_a_container_does_not(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[
        READING,
        "@@status 0\n",
        READING,
        "@@status 0\n",
        READING,
    ]));
    let handle = open_docker(cx, factory.clone());
    wait_for_reading(cx, handle).await;

    in_frame(cx, handle, |window, cx| {
        window.click("docker-stop:project:php-56", cx)
    });
    assert_eq!(factory.exec_count(), 1);
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() == 3
    })
    .await;
    assert!(factory.exec_commands()[1].contains("$d stop \"aaa111\""));

    in_frame(cx, handle, |window, cx| {
        window.click("docker-start:exciting_tesla", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() == 5
    })
    .await;
    assert!(factory.exec_commands()[3].contains("$d start \"ddd444\""));
}

#[gpui_kit::test]
async fn removing_a_volume_asks_first(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[
        READING,
        "@@status 0\n",
        READING,
    ]));
    let handle = open_docker(cx, factory.clone());
    wait_for_reading(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.within("docker-tabs").click(1usize, cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("docker-volume:old-remove", cx)
    });
    assert_eq!(factory.exec_count(), 1);
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() == 3
    })
    .await;
    assert!(factory.exec_commands()[1].contains("$d volume rm \"old\""));
}

#[gpui_kit::test]
async fn a_click_on_a_container_shows_its_details_and_output(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[READING, INSPECT, OUTPUT]));
    let handle = open_docker(cx, factory.clone());
    wait_for_reading(cx, handle).await;

    in_frame(cx, handle, |window, cx| {
        window.click("docker-container:forex-web", cx)
    });
    wait_for_label(
        cx,
        handle,
        "container-basics:命令",
        "node ./dist/server/entry.mjs",
    )
    .await;
    in_frame(cx, handle, |window, _| {
        let row = |id: &str| {
            window
                .find(SharedString::from(id.to_owned()))
                .label()
                .map(str::to_owned)
        };
        assert_eq!(row("container-basics:名称").as_deref(), Some("forex-web"));
        assert_eq!(
            row("container-basics:入口").as_deref(),
            Some("docker-entrypoint.sh")
        );
        assert_eq!(
            row("container-ports:4321/tcp").as_deref(),
            Some("127.0.0.1:4321")
        );
        assert_eq!(
            row("container-environment").as_deref(),
            Some("TZ=Asia/Shanghai\nPORT=4321")
        );
        // Running: it stops, and is not removed.
        assert!(window.try_find("container-dialog-stop").is_some());
        assert!(window.try_find("container-dialog-remove").is_none());
    });
    assert!(factory.exec_commands()[1].contains("$d inspect \"ccc333\""));

    in_frame(cx, handle, |window, cx| {
        window.within("container-tabs").click(1usize, cx)
    });
    wait_for_label(cx, handle, "container-output", OUTPUT.trim_end()).await;
    assert!(factory.exec_commands()[2].contains("$d logs --tail 200"));
}

const VOLUME: &str = r#"[{"CreatedAt":"2026-09-01T08:00:00+08:00","Driver":"local","Labels":{},
"Mountpoint":"/var/lib/docker/volumes/vw-data/_data","Name":"vw-data","Options":null,"Scope":"local"}]"#;

const IMAGE: &str = r#"[{"Id":"sha256:4a3b5c6d7e8f","RepoTags":["hello-world:latest"],"RepoDigests":[],
"Created":"2025-08-09T10:00:00Z","Size":13312,"Os":"linux","Architecture":"amd64",
"Config":{"Cmd":["/hello"],"Env":["PATH=/usr/bin"]},"RootFS":{"Layers":["sha256:a"]}}]"#;

const NETWORK: &str = r#"[{"Name":"bridge","Id":"n1","Driver":"bridge","Scope":"local",
"IPAM":{"Config":[{"Subnet":"172.17.0.0/16","Gateway":"172.17.0.1"}]},
"Containers":{"ccc333":{"Name":"forex-web","IPv4Address":"172.17.0.2/16"}}}]"#;

#[gpui_kit::test]
async fn a_click_on_a_volume_an_image_or_a_network_shows_its_details(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[
        READING, VOLUME, IMAGE, NETWORK,
    ]));
    let handle = open_docker(cx, factory.clone());
    wait_for_reading(cx, handle).await;

    in_frame(cx, handle, |window, cx| {
        window.within("docker-tabs").click(1usize, cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("docker-volume:vw-data", cx)
    });
    wait_for_label(
        cx,
        handle,
        "volume-basics:挂载点",
        "/var/lib/docker/volumes/vw-data/_data",
    )
    .await;
    in_frame(cx, handle, |window, _| {
        // Who uses it comes from the list: Docker's inspect does not say.
        assert!(window.try_find("volume-users:vaultwarden").is_some());
        // In use, so not to be removed.
        assert!(window.find("docker-dialog-remove").focused().is_none());
    });
    assert!(factory.exec_commands()[1].contains("$d volume inspect \"vw-data\""));
    in_frame(cx, handle, |window, cx| window.press("escape", cx));

    in_frame(cx, handle, |window, cx| {
        window.within("docker-tabs").click(2usize, cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("docker-image:4a3b5c6d7e8f", cx)
    });
    wait_for_label(cx, handle, "image-basics:平台", "linux/amd64").await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("image-basics:大小").label(), Some("13.00 KB"));
        assert!(window.try_find("image-users:exciting_tesla").is_some());
        assert_eq!(
            window.find("image-environment").label(),
            Some("PATH=/usr/bin")
        );
    });
    assert!(factory.exec_commands()[2].contains("$d image inspect \"4a3b5c6d7e8f\""));
    in_frame(cx, handle, |window, cx| window.press("escape", cx));

    in_frame(cx, handle, |window, cx| {
        window.within("docker-tabs").click(3usize, cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("docker-network:bridge", cx)
    });
    wait_for_label(
        cx,
        handle,
        "network-subnets:172.17.0.0/16",
        "网关 172.17.0.1",
    )
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("network-containers:forex-web").label(),
            Some("172.17.0.2/16")
        );
    });
    assert!(factory.exec_commands()[3].contains("$d network inspect \"n1\""));
}

#[gpui_kit::test]
async fn an_unused_volume_is_removed_from_its_details_after_asking(cx: &mut TestAppContext) {
    let unused = VOLUME.replace("vw-data", "old");
    let factory = Arc::new(FakeTerminalFactory::answering(&[
        READING,
        &unused,
        "@@status 0\n",
        READING,
    ]));
    let handle = open_docker(cx, factory.clone());
    wait_for_reading(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.within("docker-tabs").click(1usize, cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("docker-volume:old", cx)
    });
    wait_for_label(cx, handle, "volume-basics:名称", "old").await;
    in_frame(cx, handle, |window, cx| {
        window.click("docker-dialog-remove", cx)
    });
    // The details give way to the question.
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("docker-details").is_none());
        window.click("ok", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
        factory.exec_count() == 4
    })
    .await;
    assert!(factory.exec_commands()[2].contains("$d volume rm \"old\""));
}

#[gpui_kit::test]
async fn docker_says_when_it_will_not_answer(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&[
        "@@version\nCannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?\n",
    ]));
    let handle = open_docker(cx, factory);
    wait_for_label(
        cx,
        handle,
        "docker-message",
        "无法读取 Docker：Docker 没有运行",
    )
    .await;
}

#[gpui_kit::test]
async fn docker_says_when_the_host_has_none(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::answering(&["@@missing\nLinux\n"]));
    let handle = open_docker(cx, factory);
    wait_for_label(cx, handle, "docker-message", "这台主机没有安装 Docker。").await;
}
