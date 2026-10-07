//! 在线升级: checking, downloading, the update dialog, restarting and
//! 设置 › 关于.

use crate::support::*;

/// The version the update tests run as, whatever `Cargo.toml` says.
const RUNNING_VERSION: &str = "0.1.0";

/// Where the fake installer says the restart goes.
const INSTALLED_BUNDLE: &str = "/Applications/ShellRS.app";

/// Serves each channel's manifest, signed when asked for, and one package,
/// and counts the requests.
struct FakeUpdateFeed {
    pair: minisign::KeyPair,
    /// What each channel offers, or the error asking for it gives. A
    /// channel not listed answers 404.
    offers: Mutex<Vec<(Channel, Result<String, UpdateError>)>>,
    package: Vec<u8>,
    fetches: AtomicUsize,
    /// The channels asked for, in order.
    channels: Mutex<Vec<Channel>>,
    downloads: AtomicUsize,
}

impl UpdateFeed for FakeUpdateFeed {
    fn fetch(&self, channel: Channel) -> Result<Vec<u8>, UpdateError> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.channels.lock().unwrap().push(channel);
        let offer = self
            .offers
            .lock()
            .unwrap()
            .iter()
            .find(|(offered, _)| *offered == channel)
            .map_or(Err(UpdateError::Http(404)), |(_, offer)| offer.clone());
        offer.map(|version| signed_manifest(&self.pair, channel, &version, &self.package))
    }

    fn download(
        &self,
        _: &[String],
        size: u64,
        dest: &std::path::Path,
        progress: &mut dyn FnMut(u64),
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), UpdateError> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        if cancel.load(Ordering::SeqCst) {
            return Err(UpdateError::Cancelled);
        }
        progress(size / 2);
        std::fs::write(dest, &self.package)?;
        progress(size);
        Ok(())
    }
}

/// Records what it was asked to stage and apply; installs nothing.
struct FakeInstaller {
    kind: InstallKind,
    staged: Mutex<Vec<Version>>,
    applied: Mutex<Vec<bool>>,
}

impl FakeInstaller {
    fn new(kind: InstallKind) -> Arc<Self> {
        Arc::new(Self {
            kind,
            staged: Mutex::default(),
            applied: Mutex::default(),
        })
    }

    fn applied(&self) -> Vec<bool> {
        self.applied.lock().unwrap().clone()
    }
}

impl Installer for FakeInstaller {
    fn kind(&self) -> &InstallKind {
        &self.kind
    }

    fn stage(&self, package: &std::path::Path, release: &Release) -> Result<Staged, UpdateError> {
        self.staged.lock().unwrap().push(release.version.clone());
        Ok(Staged {
            version: release.version.clone(),
            path: package.to_path_buf(),
        })
    }

    fn apply(&self, _: &Staged, relaunch: bool) -> Result<Relaunch, UpdateError> {
        self.applied.lock().unwrap().push(relaunch);
        Ok(if relaunch {
            Relaunch::Restart(INSTALLED_BUNDLE.into())
        } else {
            Relaunch::Nothing
        })
    }

    fn clean_up(&self) {}
}

/// A copy of ShellRS that can install updates itself.
fn installable() -> InstallKind {
    InstallKind::MacBundle {
        bundle: INSTALLED_BUNDLE.into(),
    }
}

/// The update server's side of a test: what it serves, and the fakes the
/// updater was given.
struct UpdateFixture {
    feed: Arc<FakeUpdateFeed>,
    installer: Arc<FakeInstaller>,
    folder: tempfile::TempDir,
}

impl UpdateFixture {
    fn fetches(&self) -> usize {
        self.feed.fetches.load(Ordering::SeqCst)
    }

    fn downloads(&self) -> usize {
        self.feed.downloads.load(Ordering::SeqCst)
    }

    fn channels(&self) -> Vec<Channel> {
        self.feed.channels.lock().unwrap().clone()
    }

    /// Have `channel` offer `version` from now on.
    fn offer(&self, channel: Channel, version: &str) {
        let mut offers = self.feed.offers.lock().unwrap();
        offers.retain(|(offered, _)| *offered != channel);
        offers.push((channel, Ok(version.to_string())));
    }
}

/// `channel`'s manifest offering `version` for this platform, signed by
/// `pair`.
fn signed_manifest(
    pair: &minisign::KeyPair,
    channel: Channel,
    version: &str,
    package: &[u8],
) -> Vec<u8> {
    use sha2::Digest as _;
    let platform = shellrs::update::platform::platform_key();
    let sha256: String = sha2::Sha256::digest(package)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let manifest = serde_json::json!({
        "schema": 1,
        "channel": channel.key(),
        "version": version,
        "published_at": "2026-10-20T08:00:00Z",
        "notes": "### 新增\n\n- 在线升级：新版本在后台下载好后，标题栏会提示。",
        "assets": {
            platform: {
                "urls": ["https://dl.shellrs.com/releases/package"],
                "size": package.len(),
                "sha256": sha256,
            }
        },
        "installers": { platform: "https://dl.shellrs.com/releases/installer" },
    })
    .to_string();
    let signature = minisign::sign(
        Some(&pair.pk),
        &pair.sk,
        std::io::Cursor::new(manifest.as_bytes()),
        Some(&format!("shellrs-manifest {} {version}", channel.key())),
        None,
    )
    .unwrap()
    .into_string();
    serde_json::to_vec(&serde_json::json!({ "manifest": manifest, "signature": signature }))
        .unwrap()
}

/// Give the workspace's updater a fake server whose stable channel offers
/// `offered` (or answers `error`) and a fake installer of `kind`, as a
/// release build of 0.1.0.
fn serve_updates(
    cx: &mut TestAppContext,
    workspace: &Entity<Workspace>,
    offered: Result<&str, UpdateError>,
    kind: InstallKind,
) -> UpdateFixture {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let keys = TrustedKeys::new([pair.pk.to_base64().as_str()]);
    let feed = Arc::new(FakeUpdateFeed {
        pair,
        offers: Mutex::new(vec![(
            Channel::Stable,
            offered.map(|version| version.to_string()),
        )]),
        package: b"the new ShellRS".to_vec(),
        fetches: AtomicUsize::new(0),
        channels: Mutex::default(),
        downloads: AtomicUsize::new(0),
    });
    let installer = FakeInstaller::new(kind);
    let folder = tempfile::tempdir().unwrap();
    let services = UpdateServices {
        feed: feed.clone(),
        installer: installer.clone(),
        keys,
        release_build: true,
        current: Version::parse(RUNNING_VERSION).unwrap(),
        folder: folder.path().to_path_buf(),
        draw: 0.5,
    };
    workspace.update(cx, |workspace, cx| {
        workspace
            .updater()
            .update(cx, |updater, cx| updater.set_services(services, cx));
    });
    UpdateFixture {
        feed,
        installer,
        folder,
    }
}

#[gpui_kit::test]
fn the_about_page_opens_the_website(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    open_about_settings(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("open-website", cx));
    assert_eq!(cx.opened_url().as_deref(), Some("https://shellrs.com"));
}

/// Open 设置 › 关于.
fn open_about_settings(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-6", cx)
    });
}

async fn wait_for_update_status(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    wanted: impl Fn(&str) -> bool,
) {
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("update-status")
            .and_then(|status| status.label().map(&wanted))
            .unwrap_or(false)
    })
    .await;
}

fn update_status(cx: &mut TestAppContext, handle: WindowHandle<Root>) -> String {
    in_frame(cx, handle, |window, _| {
        window
            .find("update-status")
            .label()
            .unwrap_or_default()
            .to_string()
    })
}

fn set_automatic_updates(cx: &mut TestAppContext, workspace: &Entity<Workspace>, on: bool) {
    workspace.update(cx, |workspace, cx| {
        workspace.settings().update(cx, |settings, cx| {
            settings.update(|settings| settings.update.automatic = on, cx)
        });
    });
    cx.run_until_parked();
}

/// Write 更新渠道 the way its dropdown does (menus are not driven here, see
/// `appearance_dropdown`).
fn set_update_channel(cx: &mut TestAppContext, workspace: &Entity<Workspace>, channel: Channel) {
    workspace.update(cx, |workspace, cx| {
        workspace.settings().update(cx, |settings, cx| {
            settings.update(|settings| settings.update.channel = channel, cx)
        });
    });
    cx.run_until_parked();
}

/// The 更新渠道 dropdown's label on 设置 › 关于.
fn update_channel_label(cx: &mut TestAppContext, handle: WindowHandle<Root>) -> Option<String> {
    in_frame(cx, handle, |window, _| appearance_dropdown(window, 1))
}

#[gpui_kit::test]
fn a_development_build_does_not_check(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    open_about_settings(cx, handle);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("update-status").label(),
            Some("开发构建，不检查更新。")
        );
        assert!(window.try_find("check-for-updates").is_none());
        assert_eq!(
            window.find("about-version").label(),
            Some(format!("v{}", env!("CARGO_PKG_VERSION")).as_str())
        );
        assert!(window.try_find("update-available").is_none());
    });
}

#[gpui_kit::test]
async fn checking_by_hand_says_when_shellrs_is_up_to_date(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let fixture = serve_updates(cx, &workspace, Ok(RUNNING_VERSION), installable());
    open_about_settings(cx, handle);
    assert_eq!(update_status(cx, handle), "尚未检查更新。");

    in_frame(cx, handle, |window, cx| {
        window.click("check-for-updates", cx)
    });
    wait_for_update_status(cx, handle, |status| status == "当前已是最新版本。").await;
    assert_eq!(fixture.fetches(), 1);
    assert_eq!(fixture.downloads(), 0);
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("update-available").is_none());
    });
}

#[gpui_kit::test]
async fn a_found_update_downloads_by_itself_and_the_title_bar_offers_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let fixture = serve_updates(cx, &workspace, Ok("0.2.0"), installable());
    open_about_settings(cx, handle);

    in_frame(cx, handle, |window, cx| {
        window.click("check-for-updates", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("update-available")
            .is_some_and(|button| button.label() == Some("新版本 0.2.0 已就绪"))
    })
    .await;
    assert_eq!(fixture.downloads(), 1);
    assert_eq!(
        *fixture.installer.staged.lock().unwrap(),
        [Version::new(0, 2, 0)]
    );
    assert_eq!(
        update_status(cx, handle),
        "新版本 0.2.0 已下载，重启 ShellRS 即可完成更新。"
    );
    in_frame(cx, handle, |window, _| {
        // One button: restarting takes the place of 检查更新.
        assert!(window.try_find("show-update").is_some());
        assert!(window.try_find("check-for-updates").is_none());
        assert!(window.try_find("download-update").is_none());
    });
}

#[gpui_kit::test]
async fn with_automatic_updates_off_a_found_update_waits_for_download(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    set_automatic_updates(cx, &workspace, false);
    let fixture = serve_updates(cx, &workspace, Ok("0.2.0"), installable());
    open_about_settings(cx, handle);

    in_frame(cx, handle, |window, cx| {
        window.click("check-for-updates", cx)
    });
    wait_for_update_status(cx, handle, |status| status == "发现新版本 0.2.0。").await;
    assert_eq!(fixture.downloads(), 0);
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("update-available").is_none());
    });

    in_frame(cx, handle, |window, cx| window.click("download-update", cx));
    wait_for_update_status(cx, handle, |status| {
        status.starts_with("新版本 0.2.0 已下载")
    })
    .await;
    assert_eq!(fixture.downloads(), 1);
}

#[gpui_kit::test]
async fn the_update_dialog_links_the_changelog_and_restarts_into_the_new_version(
    cx: &mut TestAppContext,
) {
    let (handle, workspace) = open_workspace(cx);
    let fixture = serve_updates(cx, &workspace, Ok("0.2.0"), installable());
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CheckForUpdates), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("update-available").is_some()
    })
    .await;

    in_frame(cx, handle, |window, cx| {
        window.click("update-available", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("update-ready").is_some());
        assert!(window.try_find("restart-to-update").is_some());
    });

    // What changed is on the website's changelog; the dialog stays.
    in_frame(cx, handle, |window, cx| window.click("open-changelog", cx));
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://shellrs.com/changelog")
    );
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("update-dialog").is_some());
    });

    let restart = cx.expect_restart();
    in_frame(cx, handle, |window, cx| {
        window.click("restart-to-update", cx)
    });
    let (path, _) = restart.await.expect("restarted");
    assert_eq!(path, Some(INSTALLED_BUNDLE.into()));
    assert_eq!(fixture.installer.applied(), [true]);
    // The note the new version reads to say it was updated.
    let note = std::fs::read_to_string(fixture.folder.path().join("applied.json")).unwrap();
    assert!(
        note.contains("0.2.0") && note.contains(RUNNING_VERSION),
        "{note}"
    );

    // Already installed: quitting does not install it again.
    cx.update(|cx| cx.shutdown());
    assert_eq!(fixture.installer.applied(), [true]);
}

#[gpui_kit::test]
async fn the_update_dialog_says_what_restarting_interrupts(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let _fixture = serve_updates(cx, &workspace, Ok("0.2.0"), installable());
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CheckForUpdates), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("update-available").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("update-available", cx)
    });
    // web-01 and staging-api start out connected, each with a terminal.
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window
            .try_find("update-restart-note")
            .and_then(|note| note.label().map(|label| label.contains("2 个远程终端")))
            .unwrap_or(false)
    })
    .await;

    // The status bar is outside the dialog, under its backdrop.
    in_frame(cx, handle, |window, cx| {
        window.click("status-connection", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window.try_find("update-dialog").is_some(),
            "a click beside the update dialog closed it"
        );
    });
}

#[gpui_kit::test]
async fn a_ready_update_is_installed_on_quit_once(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let fixture = serve_updates(cx, &workspace, Ok("0.2.0"), installable());
    workspace.update(cx, |workspace, cx| {
        workspace.updater().update(cx, |updater, cx| {
            updater.start(cx);
            updater.check(cx);
        });
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("update-available").is_some()
    })
    .await;
    assert!(fixture.installer.applied().is_empty());

    cx.update(|cx| cx.shutdown());
    assert_eq!(fixture.installer.applied(), [false]);
    cx.update(|cx| cx.shutdown());
    assert_eq!(fixture.installer.applied(), [false]);
}

#[gpui_kit::test]
async fn an_unsupported_install_offers_the_download_page(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let fixture = serve_updates(
        cx,
        &workspace,
        Ok("0.2.0"),
        InstallKind::Unsupported(Unsupported::NotAppImage),
    );
    open_about_settings(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.click("check-for-updates", cx)
    });
    wait_for_update_status(cx, handle, |status| {
        status == "发现新版本 0.2.0。这份 ShellRS 不是 AppImage，请用安装它的方式更新。"
    })
    .await;
    assert_eq!(fixture.downloads(), 0);
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("open-download-page").is_some());
        assert!(window.try_find("download-update").is_none());
        assert_eq!(
            window.find("update-available").label(),
            Some("新版本 0.2.0 可用")
        );
    });
}

#[gpui_kit::test]
async fn a_failed_check_says_why_on_the_about_page(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let fixture = serve_updates(cx, &workspace, Err(UpdateError::Http(503)), installable());
    open_about_settings(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.click("check-for-updates", cx)
    });
    wait_for_update_status(cx, handle, |status| {
        status == "检查更新失败：更新服务器返回 HTTP 503"
    })
    .await;
    assert_eq!(fixture.fetches(), 1);
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("update-available").is_none());
        assert!(window.notifications(cx).is_empty());
    });
}

#[gpui_kit::test]
async fn turning_automatic_updates_off_stops_the_schedule(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let fixture = serve_updates(cx, &workspace, Ok(RUNNING_VERSION), installable());
    workspace.update(cx, |workspace, cx| {
        workspace
            .updater()
            .update(cx, |updater, cx| updater.start(cx));
    });
    cx.run_until_parked();
    assert_eq!(fixture.fetches(), 0, "nothing before the first wait");

    cx.executor().advance_clock(Duration::from_secs(31));
    open_about_settings(cx, handle);
    wait_for_update_status(cx, handle, |status| status == "当前已是最新版本。").await;
    assert_eq!(fixture.fetches(), 1);

    set_automatic_updates(cx, &workspace, false);
    cx.executor()
        .advance_clock(Duration::from_secs(7 * 60 * 60));
    cx.run_until_parked();
    assert_eq!(fixture.fetches(), 1);

    set_automatic_updates(cx, &workspace, true);
    cx.executor()
        .advance_clock(Duration::from_secs(7 * 60 * 60));
    cx.wait_for(handle.into(), Duration::from_secs(3), |_, _| {
        fixture.fetches() == 2
    })
    .await;
}

#[gpui_kit::test]
async fn switching_the_update_channel_looks_at_that_channel(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let fixture = serve_updates(cx, &workspace, Ok(RUNNING_VERSION), installable());
    fixture.offer(Channel::Beta, "0.2.0-beta.1");
    open_about_settings(cx, handle);
    assert_eq!(update_channel_label(cx, handle).as_deref(), Some("稳定版"));
    in_frame(cx, handle, |window, cx| {
        window.click("check-for-updates", cx)
    });
    wait_for_update_status(cx, handle, |status| status == "当前已是最新版本。").await;

    // With 自动升级 on, the other channel is looked at right away.
    set_update_channel(cx, &workspace, Channel::Beta);
    wait_for_update_status(cx, handle, |status| {
        status == "新版本 0.2.0-beta.1 已下载，重启 ShellRS 即可完成更新。"
    })
    .await;
    assert_eq!(fixture.channels(), [Channel::Stable, Channel::Beta]);
    assert_eq!(update_channel_label(cx, handle).as_deref(), Some("Beta"));
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("update-available").is_some());
    });

    // Back on stable, the beta downloaded is no longer offered.
    set_update_channel(cx, &workspace, Channel::Stable);
    wait_for_update_status(cx, handle, |status| status == "当前已是最新版本。").await;
    assert_eq!(
        fixture.channels(),
        [Channel::Stable, Channel::Beta, Channel::Stable]
    );
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("update-available").is_none());
    });
}

#[gpui_kit::test]
async fn the_first_start_after_an_update_says_so(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let fixture = serve_updates(cx, &workspace, Ok(RUNNING_VERSION), installable());
    std::fs::write(
        fixture.folder.path().join("applied.json"),
        r#"{"from":"0.0.9","to":"0.1.0"}"#,
    )
    .unwrap();
    workspace.update(cx, |workspace, cx| {
        workspace
            .updater()
            .update(cx, |updater, cx| updater.start(cx));
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.notifications(cx).len() == 1
    })
    .await;
    assert!(!fixture.folder.path().join("applied.json").exists());
}
