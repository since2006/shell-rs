//! 设置: the tab, 外观, 外部 CLI and the title bar's theme switch.

use crate::support::*;

#[gpui_kit::test]
fn theme_toggle_flips_mode(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    let before = cx.update(|cx| {
        let theme = cx.theme();
        assert_eq!(theme.list_hover, theme.tokens.list_hover.color);
        assert!(theme.list_hover.a > 0.9);
        theme.is_dark()
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("theme-toggle", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let after = cx.update(|cx| {
        let theme = cx.theme();
        assert_eq!(theme.list_hover, theme.tokens.list_hover.color);
        assert!(theme.list_hover.a > 0.9);
        theme.is_dark()
    });
    assert_ne!(before, after);
}

#[gpui_kit::test]
fn settings_open_from_the_host_list_as_one_tab(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings").visible());
        assert_eq!(window.find("settings").focused(), Some(true));
        assert_eq!(window.find("settings-tab").label(), Some("设置"));
        // Two columns: the first category, 外观, is shown until another is
        // picked. `Settings` names its category rows by position.
        assert_eq!(appearance_dropdown(window, 0).as_deref(), Some("简体中文"));
        assert!(window.try_find("terminal-font-preview").is_none());
        window.within("settings").click("0-1", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // 终端 is the second.
        assert!(window.find("terminal-font-preview").visible());
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("terminal", INITIAL_WEB_TERMINAL)).visible());
        assert!(window.try_find("settings").is_none());
        // Opening settings again brings the same tab forward.
        #[cfg(target_os = "macos")]
        window.press("cmd-,", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-,", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings").visible());
        assert_eq!(window.find("settings").focused(), Some(true));
    })
    .unwrap();
    cx.update(|cx| {
        let settings = workspace.read(cx).settings_tab().expect("settings open");
        let group = settings.read(cx).tab_group().unwrap().upgrade().unwrap();
        // The two host terminals and a single settings tab.
        assert_eq!(group.read(cx).panels().len(), 3);
    });

    // Opening it while it is displayed changes nothing.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings").visible());
        assert_eq!(window.find("settings").focused(), Some(true));
        // ⌘W closes it like any other tab.
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-tab").is_none());
        assert!(window.try_find("settings").is_none());
    })
    .unwrap();
    cx.update(|cx| assert!(workspace.read(cx).settings_tab().is_none()));
}

/// The label of one dropdown in the 外观 page's 常规 group. `Settings` names
/// its groups, items and dropdown buttons by position.
/// Open the settings tab on 外部 CLI, the third category.
fn open_external_cli_settings(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("settings").click("0-3", cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_external_cli_switch_is_off_until_turned_on(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let enabled = |cx: &mut TestAppContext| {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .settings()
                .read(cx)
                .settings()
                .external_cli
                .enabled
        })
    };
    open_external_cli_settings(cx, handle);
    assert!(!enabled(cx));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // 访问控制 › 启用外部 CLI.
        let switch = window
            .within("settings")
            .within("group-0")
            .within("item-0")
            .find("check");
        assert_eq!(switch.checked(), Some(false));
        window
            .within("settings")
            .within("group-0")
            .within("item-0")
            .click("check", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(enabled(cx));
}

#[gpui_kit::test]
fn without_a_home_to_install_into_the_external_cli_offers_nothing(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    open_external_cli_settings(cx, handle);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("skill-status-codex").label(),
            Some("此系统暂不支持安装")
        );
        assert!(window.try_find("skill-codex").is_none());
        assert!(window.try_find("cli-binary").is_none());
        // Copying the skill needs no home directory.
        window.click("copy-agent-skill", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(shellrs::cli::SKILL.to_string())
    );
}

#[gpui_kit::test]
fn agent_skills_install_where_each_agent_looks_and_come_off_again(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("app").join("shellrs");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "binary").unwrap();
    let paths = IntegrationPaths {
        home: root.path().join("home"),
        bin_link: root.path().join("bin").join("shellrs"),
        exe,
        user_path: None,
    };
    let codex = paths.skill_file(AgentKind::Codex);
    workspace.update(cx, |workspace, cx| {
        workspace.cli_integration().update(cx, |integration, cx| {
            integration.set_paths(Some(paths.clone()), cx)
        })
    });
    open_external_cli_settings(cx, handle);

    let status = |cx: &mut TestAppContext, id: &'static str| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.find(id).label().map(str::to_string)
        })
        .unwrap()
    };
    assert_eq!(
        status(cx, "skill-status-codex").as_deref(),
        Some(format!("未安装（{}）", codex.display()).as_str())
    );

    // The row's button dispatches the install.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("skill-codex", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(&codex).unwrap(),
        shellrs::cli::SKILL
    );
    assert_eq!(
        status(cx, "skill-status-codex").as_deref(),
        Some(format!("已安装于 {}", codex.display()).as_str())
    );
    // Only Codex's.
    assert!(!paths.skill_file(AgentKind::ClaudeCode).exists());

    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(RemoveAgentSkill(AgentKind::Codex)), cx);
        window.dispatch_action(Box::new(InstallCliCommand), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!codex.exists());
    assert_eq!(
        status(cx, "cli-binary-status").as_deref(),
        Some(format!("已安装于 {}", paths.bin_link.display()).as_str())
    );
}

/// Menus are not driven here: a dropdown's item writes the settings store,
/// so the test writes it the same way and checks what follows.
#[gpui_kit::test]
fn the_appearance_setting_drives_the_theme_and_the_title_bar_switch(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // 外观 is the first category.
        window.within("settings").click("0-0", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(appearance_dropdown(window, 0).as_deref(), Some("简体中文"));
        assert_eq!(appearance_dropdown(window, 1).as_deref(), Some("跟随系统"));
    })
    .unwrap();

    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(|settings| settings.appearance = Appearance::Dark, cx)
        })
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let theme = cx.theme();
        assert!(theme.is_dark());
        // Applied like the title bar switch always did it.
        assert_eq!(theme.list_hover, theme.tokens.list_hover.color);
        assert!(theme.list_hover.a > 0.9);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(appearance_dropdown(window, 1).as_deref(), Some("深色"));
        // The title bar's switch picks the other appearance outright.
        window.click("theme-toggle", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        assert!(!cx.theme().is_dark());
        let chosen = settings.read(cx).settings();
        assert_eq!(chosen.appearance, Appearance::Light);
        assert_eq!(chosen.language, InterfaceLanguage::SimplifiedChinese);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(appearance_dropdown(window, 1).as_deref(), Some("浅色"));
    })
    .unwrap();
}

/// The patterns of the 关键字高亮 rules, in order.
fn highlight_patterns(workspace: &Entity<Workspace>, cx: &App) -> Vec<String> {
    workspace
        .read(cx)
        .settings()
        .read(cx)
        .settings()
        .terminal_highlight
        .rules
        .iter()
        .map(|rule| rule.pattern.clone())
        .collect()
}

#[gpui_kit::test]
async fn highlight_rules_are_added_edited_and_deleted_on_their_page(cx: &mut TestAppContext) {
    use shellrs::terminal::{HighlightColor, HighlightRule, PatternKind, TerminalHighlights};

    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    // 关键字高亮 comes right after 终端, with the examples a new
    // installation starts with.
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-2", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("highlight-rule", 2usize)).visible());
        assert!(window.try_find(("highlight-rule", 3usize)).is_none());
    });
    let examples = [
        "ERROR".to_string(),
        "WARN".into(),
        r"\b\d{1,3}(\.\d{1,3}){3}\b".into(),
    ];
    assert_eq!(cx.update(|cx| highlight_patterns(&workspace, cx)), examples);

    // A new rule takes the keyboard at once; a regex that does not compile
    // is refused, the dialog staying open.
    in_frame(cx, handle, |window, cx| {
        window.click("add-highlight-rule", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("highlight-pattern").focused(), Some(true));
        window.input("OutOf(Memory", cx);
        window.within("highlight-kind").click(1usize, cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("form-error")
            .is_some_and(|error| error.label() == Some("正则表达式写法有误"))
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click("highlight-pattern", cx);
        window.press("cmd-a", cx);
        window.input(r"OutOfMemory\w*", cx);
        window.within("highlight-color").click(5usize, cx);
        window.click("highlight-bold", cx);
        window.click("highlight-notify", cx);
        window.click("commit", cx);
    });
    cx.run_until_parked();
    let rules = cx.update(|cx| {
        workspace
            .read(cx)
            .settings()
            .read(cx)
            .settings()
            .terminal_highlight
            .rules
    });
    assert_eq!(
        rules.last(),
        Some(&HighlightRule {
            pattern: r"OutOfMemory\w*".into(),
            kind: PatternKind::Regex,
            color: HighlightColor::Magenta,
            bold: true,
            notify: true,
        })
    );

    // With the dialog closed nothing has focus, and the row's button still
    // reaches the workspace.
    in_frame(cx, handle, |window, cx| {
        window.click(("edit-highlight-rule", 3usize), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("highlight-pattern").focused(), Some(true));
        window.press("cmd-a", cx);
        window.input("OOM", cx);
        window.within("highlight-kind").click(0usize, cx);
        window.click("commit", cx);
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| highlight_patterns(&workspace, cx))[3], "OOM");

    // Deleted once confirmed; the terminals follow.
    in_frame(cx, handle, |window, cx| {
        window.click(("delete-highlight-rule", 0usize), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    cx.update(|cx| {
        let patterns = highlight_patterns(&workspace, cx);
        assert_eq!(
            patterns,
            [examples[1].clone(), examples[2].clone(), "OOM".into()]
        );
        let in_effect: Vec<_> = TerminalHighlights::current(cx)
            .rules()
            .iter()
            .map(|rule| rule.pattern.clone())
            .collect();
        assert_eq!(in_effect, patterns);
    });
}
