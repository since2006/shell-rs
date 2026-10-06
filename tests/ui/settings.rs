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
        // Back on the category it showed, 终端, not the first one.
        assert!(window.find("terminal-font-preview").visible());
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

#[gpui_kit::test]
fn terminal_themes_follow_the_appearance_and_a_card_chooses_one(cx: &mut TestAppContext) {
    use gpui_kit::component::ThemeMode;
    use shellrs::terminal::{TerminalColors, TerminalTheme};

    let (handle, workspace) = open_workspace(cx);
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    let in_effect = |cx: &mut TestAppContext| cx.update(|cx| TerminalColors::current(cx).key());
    let set_appearance = |appearance, cx: &mut TestAppContext| {
        cx.update(|cx| {
            settings.update(cx, |settings, cx| {
                settings.update(|settings| settings.appearance = appearance, cx)
            })
        });
        cx.run_until_parked();
    };
    let card = |theme: &TerminalTheme| {
        gpui_kit::SharedString::from(format!("terminal-theme-{}", theme.key()))
    };
    let light = TerminalTheme::for_mode(ThemeMode::Light).nth(1).unwrap();
    let dark = TerminalTheme::for_mode(ThemeMode::Dark).nth(1).unwrap();

    set_appearance(Appearance::Light, cx);
    assert_eq!(in_effect(cx), "shellrs-light");
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    // 外观 is the first category; its second group holds both columns.
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-0", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("terminal-theme-shellrs-light").checked(),
            Some(true)
        );
        assert_eq!(
            window.find("terminal-theme-shellrs-dark").checked(),
            Some(true)
        );
        assert_eq!(window.find(card(light)).checked(), Some(false));
        window.click(card(light), cx);
    });
    cx.update(|cx| {
        assert_eq!(
            settings.read(cx).settings().terminal_theme.light,
            light.key()
        )
    });
    // The light column is the one in effect, so the terminals change now.
    // The app stays neutral until 界面跟随主题 is on.
    assert_eq!(in_effect(cx), light.key());
    let neutral = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let name = cx.theme().theme_name().clone();
            name == "Default Light" || name == "Default Dark"
        })
    };
    assert!(neutral(cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find(card(light)).checked(), Some(true));
        assert_eq!(
            window.find("terminal-theme-shellrs-light").checked(),
            Some(false)
        );
        window.click(card(dark), cx);
    });
    // The dark one waits for the dark appearance.
    cx.update(|cx| assert_eq!(settings.read(cx).settings().terminal_theme.dark, dark.key()));
    assert_eq!(in_effect(cx), light.key());

    // 主题 is the second group, the switch its first item.
    let switch = |window: &mut gpui_kit::Window, cx: &mut App| {
        window
            .within("settings")
            .within("group-1")
            .within("item-0")
            .click("check", cx)
    };
    in_frame(cx, handle, |window, cx| {
        let on = window
            .within("settings")
            .within("group-1")
            .within("item-0")
            .find("check")
            .checked();
        assert_eq!(on, Some(false));
        switch(window, cx);
    });
    assert!(cx.update(|cx| settings.read(cx).settings().terminal_theme.app_follows));
    // On, the app takes the theme's colors, for either appearance.
    let background = |cx: &mut TestAppContext| cx.update(|cx| cx.theme().background.to_rgb());
    let near = |a: gpui_kit::Rgba, b: gpui_kit::Rgba| {
        (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs() < 4. / 255.
    };
    assert!(near(background(cx), light.background().to_rgb()));
    set_appearance(Appearance::Dark, cx);
    assert_eq!(in_effect(cx), dark.key());
    assert!(cx.update(|cx| cx.theme().is_dark()));
    assert!(near(background(cx), dark.background().to_rgb()));
    set_appearance(Appearance::Light, cx);
    assert_eq!(in_effect(cx), light.key());
    assert!(near(background(cx), light.background().to_rgb()));

    // Off again, the app is neutral and the terminals keep the theme.
    in_frame(cx, handle, |window, cx| switch(window, cx));
    assert_eq!(in_effect(cx), light.key());
    assert!(neutral(cx));
    in_frame(cx, handle, |window, cx| switch(window, cx));

    // Back on the default, the app is gpui-kit's own light theme again.
    in_frame(cx, handle, |window, cx| {
        window.click("terminal-theme-shellrs-light", cx)
    });
    assert_eq!(in_effect(cx), "shellrs-light");
    cx.update(|cx| assert_eq!(cx.theme().theme_name().as_ref(), "Default Light"));
}

/// The 关键字高亮 rules, as the settings hold them.
fn highlight_rules(
    workspace: &Entity<Workspace>,
    cx: &mut TestAppContext,
) -> Vec<shellrs::terminal::HighlightRule> {
    cx.update(|cx| {
        workspace
            .read(cx)
            .settings()
            .read(cx)
            .settings()
            .terminal_highlight
            .rules
    })
}

fn patterns(rules: &[shellrs::terminal::HighlightRule]) -> Vec<&str> {
    rules.iter().map(|rule| rule.pattern.as_str()).collect()
}

/// Scroll the settings page down to the rules, below the window's fold.
fn scroll_to_highlight_rules(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        let position = window.find("highlight-preview").bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-600.))),
                modifiers: gpui_kit::Modifiers::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
}

#[gpui_kit::test]
async fn highlight_rules_are_edited_in_place_on_their_page(cx: &mut TestAppContext) {
    use shellrs::terminal::{HighlightColor, TerminalHighlights};

    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    // 关键字高亮 comes right after 终端, with the examples a new
    // installation starts with: rows 0 to 2.
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-2", cx)
    });
    let ip = r"\b\d{1,3}(\.\d{1,3}){3}\b";
    assert_eq!(
        patterns(&highlight_rules(&workspace, cx)),
        ["ERROR", "WARN", ip]
    );
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("highlight-rule", 2usize)).visible());
        assert!(window.find("highlight-preview").visible());
    });

    // Off until turned on: the examples color nothing before.
    let colored = |cx: &mut TestAppContext| {
        cx.update(|cx| !TerminalHighlights::current(cx).spans("ERROR").is_empty())
    };
    assert!(!colored(cx));
    in_frame(cx, handle, |window, cx| {
        // 常规 › 启用关键字高亮.
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
    });
    cx.run_until_parked();
    assert!(colored(cx));

    // A new row takes the keyboard at once; what does not compile says so
    // under it, and the settings have it as typed.
    in_frame(cx, handle, |window, cx| {
        window.click("add-highlight-rule", cx)
    });
    scroll_to_highlight_rules(cx, handle);
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("highlight-rule-pattern", 3usize)).focused(),
            Some(true)
        );
        window.input("OutOf(Memory", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("highlight-rule-error", 3usize)).label(),
            Some("正则表达式写法有误")
        );
        window.press("cmd-a", cx);
        window.input(r"OutOfMemory\w*", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find(("highlight-rule-error", 3usize)).is_none());
        window.click(("highlight-rule-note", 3usize), cx);
        window.input("内存溢出", cx);
        window.click(("highlight-rule-color", 3usize), cx);
        window.press("cmd-a", cx);
        window.input("#D946EF", cx);
        window.click(("highlight-rule-notify", 3usize), cx);
        window.click(("highlight-rule-enabled", 0usize), cx);
    });
    cx.run_until_parked();
    let rules = highlight_rules(&workspace, cx);
    assert_eq!(rules[3].pattern, r"OutOfMemory\w*");
    assert_eq!(rules[3].note, "内存溢出");
    assert_eq!(rules[3].color, HighlightColor::from_hex("#d946ef").unwrap());
    assert!(rules[3].enabled && rules[3].notify);
    assert!(!rules[0].enabled);

    // The color picker opens on its palette. (Its swatches are keyed by
    // color; the a11y tree that would catch two of one color is not built
    // in tests, see FEATURED_COLORS.)
    in_frame(cx, handle, |window, cx| {
        window.click(("highlight-rule-picker", 3usize), cx)
    });
    in_frame(cx, handle, |window, cx| window.press("escape", cx));

    // Dragged by its handle, the new row goes first.
    in_frame(cx, handle, |window, cx| {
        window.drag_to(
            ("highlight-rule-handle", 3usize),
            ("highlight-rule", 0usize),
            cx,
        )
    });
    cx.run_until_parked();
    assert_eq!(
        patterns(&highlight_rules(&workspace, cx)),
        [r"OutOfMemory\w*", "ERROR", "WARN", ip]
    );

    // Deleted at once, WARN goes; the terminals follow every change.
    in_frame(cx, handle, |window, cx| {
        window.click(("delete-highlight-rule", 1usize), cx)
    });
    cx.run_until_parked();
    assert_eq!(
        patterns(&highlight_rules(&workspace, cx)),
        [r"OutOfMemory\w*", "ERROR", ip]
    );
    cx.update(|cx| {
        let colored = TerminalHighlights::current(cx).spans("OutOfMemoryError ERROR 10.0.0.1");
        // ERROR is off; the new rule and the address color.
        assert_eq!(
            colored
                .iter()
                .map(|(range, _)| range.clone())
                .collect::<Vec<_>>(),
            [0..16, 23..31]
        );
    });

    // 启用关键字高亮 off, nothing colors.
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    settings.update(cx, |settings, cx| {
        settings.update(|settings| settings.terminal_highlight.enabled = false, cx)
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            TerminalHighlights::current(cx)
                .spans("OutOfMemoryError")
                .is_empty()
        );
    });
}
