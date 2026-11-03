//! 设置 › 云同步: off, the three ways to turn it on (a folder, a WebDAV
//! server, an S3 bucket); on, where this computer stands, the other
//! devices, and the sync password.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    label::Label,
    setting::{SettingField, SettingGroup, SettingItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use shellrs_sync::RemoteDevice;

use super::settings_panel::keywords;
use crate::app::{
    CatalogIcon, ChangeSyncLogin, ChangeSyncPassword, RemoveSyncDevice, ReviewSync, SetUpS3Sync,
    SetUpSync, SetUpWebDavSync, SyncNow, TurnOffSync,
};
use crate::host::ErrorMessage as _;
use crate::i18n::{t, tn};
use crate::settings::SyncLocation;
use crate::sync::{Attention, JobError, SyncPhase, SyncService, SyncSnapshot};

pub(super) fn sync_groups(sync: &Entity<SyncService>, cx: &App) -> Vec<SettingGroup> {
    let snapshot = sync.read(cx).snapshot(cx);
    let Some(location) = snapshot.location.clone() else {
        return vec![set_up_group(snapshot.available)];
    };
    let name = snapshot
        .device
        .as_ref()
        .map(|device| device.name.clone())
        .unwrap_or_default();
    let busy = snapshot.busy;
    vec![
        SettingGroup::new()
            .title(t!("settings.sync.this_computer"))
            .description(t!("settings.sync.this_computer_description", name = name))
            .items(
                [status_item(snapshot.clone())]
                    .into_iter()
                    .chain(location_items(&location, busy))
                    .chain([command_item(
                        t!("settings.sync.turn_off"),
                        t!("settings.sync.turn_off_description"),
                        ("turn-off-sync", t!("settings.sync.turn_off_button")),
                        busy,
                        || Box::new(TurnOffSync),
                    )]),
            ),
        devices_group(&snapshot),
        SettingGroup::new()
            .title(t!("settings.sync.password"))
            .items([command_item(
                t!("settings.sync.change_password"),
                t!("settings.sync.change_password_description"),
                ("change-sync-password", t!("settings.sync.change")),
                busy,
                || Box::new(ChangeSyncPassword),
            )]),
    ]
}

/// Sync is off: what it does, and the two places it can go through.
fn set_up_group(available: bool) -> SettingGroup {
    let description = if available {
        t!("settings.sync.description")
    } else {
        JobError::Unavailable.message()
    };
    SettingGroup::new()
        .title(t!("settings.sync.set_up"))
        .description(description)
        .items([
            command_item(
                t!("settings.sync.folder_option"),
                t!("settings.sync.folder_option_description"),
                ("sync-set-up", t!("settings.sync.choose_folder")),
                !available,
                || Box::new(SetUpSync),
            )
            .keywords(keywords(t!("settings.sync.keywords"))),
            command_item(
                t!("settings.sync.webdav_option"),
                t!("settings.sync.webdav_option_description"),
                ("sync-set-up-webdav", t!("settings.sync.connect_webdav")),
                !available,
                || Box::new(SetUpWebDavSync),
            ),
            command_item(
                t!("settings.sync.s3_option"),
                t!("settings.sync.s3_option_description"),
                ("sync-set-up-s3", t!("settings.sync.connect_s3")),
                !available,
                || Box::new(SetUpS3Sync),
            ),
        ])
}

/// Where sync goes through: a folder's path, or a WebDAV server's
/// address and the login to change.
fn location_items(location: &SyncLocation, busy: bool) -> Vec<SettingItem> {
    match location {
        SyncLocation::Folder { path } => vec![path_item(
            "sync-folder",
            t!("settings.sync.folder"),
            path.display().to_string().into(),
        )],
        SyncLocation::WebDav { url, username } => vec![
            path_item(
                "sync-webdav-url",
                t!("settings.sync.webdav"),
                url.clone().into(),
            ),
            command_item(
                t!("settings.sync.webdav_login"),
                username.clone().into(),
                ("change-sync-login", t!("settings.sync.change")),
                busy,
                || Box::new(ChangeSyncLogin),
            ),
        ],
        SyncLocation::S3 {
            endpoint,
            bucket,
            prefix,
            access_key,
            ..
        } => {
            let place = if prefix.is_empty() {
                format!("{bucket} · {endpoint}")
            } else {
                format!("{bucket}/{prefix} · {endpoint}")
            };
            vec![
                path_item("sync-s3-location", t!("settings.sync.s3"), place.into()),
                command_item(
                    t!("settings.sync.s3_login"),
                    access_key.clone().into(),
                    ("change-sync-login", t!("settings.sync.change")),
                    busy,
                    || Box::new(ChangeSyncLogin),
                ),
            ]
        }
    }
}

/// A row whose button opens what it names.
fn command_item(
    title: SharedString,
    description: SharedString,
    (id, label): (&'static str, SharedString),
    busy: bool,
    action: fn() -> Box<dyn Action>,
) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::render(move |options, _, _| {
            Button::new(id)
                .outline()
                .with_size(options.size())
                .label(label.clone())
                .disabled(busy)
                .on_click(move |_, window, cx| window.dispatch_action(action(), cx))
        }),
    )
    .description(description)
}

/// 状态: where sync stands, or what it waits for, with the button for it.
fn status_item(snapshot: SyncSnapshot) -> SettingItem {
    SettingItem::render(move |options, _, cx| {
        let (text, color) = match (snapshot.attention, &snapshot.phase) {
            (Some(attention), _) => (attention_text(attention), cx.theme().warning),
            (None, SyncPhase::Failed(error)) => (
                t!("settings.sync.failed", reason = error.message()),
                cx.theme().danger,
            ),
            (None, phase) => (phase_text(phase), cx.theme().muted_foreground),
        };
        let button = match snapshot.attention {
            Some(attention) => Button::new("review-sync")
                .primary()
                .label(review_label(attention))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ReviewSync), cx)),
            None => Button::new("sync-now")
                .outline()
                .icon(Icon::new(CatalogIcon::RefreshCw))
                .label(t!("settings.sync.sync_now"))
                .loading(snapshot.phase == SyncPhase::Syncing)
                .disabled(snapshot.busy || !snapshot.available)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(SyncNow), cx)),
        }
        .with_size(options.size());
        // A narrow page stacks each row, the controls under the text.
        let stacked = options.layout() == Axis::Vertical;
        let row = if stacked {
            v_flex()
        } else {
            h_flex().justify_between().items_center()
        };
        row.w_full()
            .gap_3()
            .child(
                v_flex()
                    .map(|column| {
                        if stacked {
                            column.w_full()
                        } else {
                            column.flex_1().max_w_3_5()
                        }
                    })
                    .child(Label::new(t!("settings.sync.status")).text_sm())
                    .child(
                        div()
                            .id("sync-status")
                            .test_support()
                            .aria_label(text.clone())
                            .text_sm()
                            .text_color(color)
                            .child(text),
                    ),
            )
            .child(div().flex_shrink_0().child(button))
    })
    .keywords(keywords(t!("settings.sync.keywords")))
}

fn phase_text(phase: &SyncPhase) -> SharedString {
    match phase {
        SyncPhase::Off | SyncPhase::Waiting => t!("settings.sync.waiting"),
        SyncPhase::Syncing => t!("settings.sync.syncing"),
        SyncPhase::Synced { at_ms } => t!("settings.sync.synced", time = format_time(*at_ms)),
        SyncPhase::Failed(error) => error.message(),
    }
}

fn attention_text(attention: Attention) -> SharedString {
    match attention {
        Attention::NeedsPassword => t!("settings.sync.attention.needs_password"),
        Attention::VaultReplaced => t!("settings.sync.attention.vault_replaced"),
        Attention::Removed => t!("settings.sync.attention.removed"),
        Attention::MassDelete { count, .. } => {
            tn!("settings.sync.attention.mass_delete", count as usize)
        }
        Attention::LoginRefused => t!("settings.sync.attention.login_refused"),
    }
}

fn review_label(attention: Attention) -> SharedString {
    match attention {
        Attention::NeedsPassword => t!("settings.sync.enter_password"),
        Attention::VaultReplaced | Attention::Removed => t!("settings.sync.rejoin"),
        Attention::MassDelete { .. } => t!("settings.sync.review"),
        Attention::LoginRefused => t!("settings.sync.sign_in"),
    }
}

/// A row titled `title` showing where, as `path`.
fn path_item(id: &'static str, title: SharedString, path: SharedString) -> SettingItem {
    SettingItem::render(move |_, _, cx| {
        v_flex()
            .w_full()
            .min_w_0()
            .child(Label::new(title.clone()).text_sm())
            .child(
                div()
                    .id(id)
                    .test_support()
                    .aria_label(path.clone())
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(path.clone()),
            )
    })
}

/// 其他设备: each with its system and version, when it last wrote, and 移除.
fn devices_group(snapshot: &SyncSnapshot) -> SettingGroup {
    let group = SettingGroup::new().title(t!("settings.sync.devices"));
    if snapshot.devices.is_empty() {
        return group.description(t!("settings.sync.devices_empty"));
    }
    let mut devices = snapshot.devices.clone();
    devices.sort_by(|a, b| a.info.name.cmp(&b.info.name).then(a.id.cmp(&b.id)));
    let busy = snapshot.busy;
    group
        .description(t!("settings.sync.devices_description"))
        .items(devices.into_iter().map(|device| device_item(device, busy)))
}

fn device_item(device: RemoteDevice, busy: bool) -> SettingItem {
    let detail = t!(
        "settings.sync.device_detail",
        platform = platform_name(&device.info.platform),
        version = device.info.app_version,
        time = format_time(device.last_write_ms)
    );
    let id = device.id.clone();
    SettingItem::new(
        device.info.name.clone(),
        SettingField::render(move |options, _, _| {
            let id = id.clone();
            Button::new(SharedString::from(format!("remove-sync-device-{id}")))
                .outline()
                .with_size(options.size())
                .label(t!("settings.sync.remove"))
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(RemoveSyncDevice(id.clone())), cx)
                })
        }),
    )
    .description(detail)
}

/// The system a device runs, as people call it.
fn platform_name(platform: &str) -> SharedString {
    match platform {
        "macos" => "macOS".into(),
        "windows" => "Windows".into(),
        "linux" => "Linux".into(),
        other => other.to_owned().into(),
    }
}

/// A time in local time, to the minute: `2026/10/25 14:32`.
fn format_time(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y/%-m/%-d %-H:%M")
                .to_string()
        })
        .unwrap_or_default()
}
