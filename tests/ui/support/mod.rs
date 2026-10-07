//! What the tests share: the imports, the seeded ids, opening a workspace
//! and driving it frame by frame.

// Every test module starts with `use crate::support::*;`, which brings in
// these along with the fixtures and fakes.
pub use gpui_kit::component::{ActiveTheme as _, Root, WindowExt as _, notification::Notification};
pub use gpui_kit::test::{TestAppContextExt, TestWindowExt};
pub use gpui_kit::{
    App, AppContext as _, ClipboardItem, ElementId, Entity, InputEvent as _, MouseButton,
    MouseMoveEvent, TestAppContext, WindowHandle, point, px, size,
};
pub use semver::Version;
pub use shellrs::app::{
    CenterTab, CheckForUpdates, ClearTerminal, CloseScope, CloseTabs, CollapseAllGroups,
    ConnectGroup, ConnectHost, CopyCredentialPublicKey, CopyHostAddress, CopyHostId,
    DeleteCredential, DeleteForward, DeleteGroup, DeleteHost, DisconnectHost, DisconnectTerminal,
    EditCredential, EditForward, EditHost, ExpandAllGroups, FindInTerminal, FindNextInTerminal,
    FindPreviousInTerminal, FocusSearch, InstallCliCommand, NewHostInGroup, NewLocalTerminal,
    OpenExplorer, OpenSettings, ReconnectTerminal, RemoveAgentSkill, RenameGroup, RenameTerminal,
    StartForward, StopForward, ToggleHostPanel, ToggleToolSidebar,
};
pub use shellrs::cli::{AgentKind, IntegrationPaths};
pub use shellrs::connection::{
    ConnectionPrompt, ConnectionPromptField, ConnectionPromptKind, ConnectionPromptReply,
    ConnectionTester, Latency, LoginTest, TrustCallback,
};
pub use shellrs::explorer::ExplorerId;
pub use shellrs::forward::{
    ForwardCommand, ForwardEvent, ForwardStatus, ForwardTransport, ForwardTransportProvider,
};
pub use shellrs::host::{
    AuthKind, ConnectionState, CredentialDraft, CredentialId, CredentialKind, ForwardDraft,
    ForwardEndpoint, ForwardId, ForwardKind, ForwardRule, GeneratedKey, GroupDraft, GroupId,
    HostDatabase, HostDraft, HostId, HostLogin, HostOs, HostStore, JumpLogin, KeyAlgorithm,
    LoginMethod, LoginRoute, PastedKey, ProxyKind, ProxySettings, Route, read_public_key,
};
pub use shellrs::secrets::{InMemorySecretStore, SecretRef, SecretStore as _};
pub use shellrs::settings::{Appearance, InterfaceLanguage, SettingsStore};
pub use shellrs::sftp::{
    DirectoryEntry, DirectoryListing, EDIT_LIMIT, EntryKind, FileBytes, FileMetadata, FileStamp,
    LocalDirectoryProvider, ReadFailure, RemotePath, SaveFailure, SftpCommand, SftpEvent,
    SftpTransport, SftpTransportProvider, TextFile, UploadRequest,
};
pub use shellrs::terminal::{
    FixedRemoteTerminalTransportProvider, LocalTerminalId, RemoteTerminalId,
    RemoteTerminalTransportProvider, SharedTerminalTransportFactory, TerminalFont,
    TerminalLifecycle, TerminalSize, TerminalTransport, TerminalTransportCommand,
    TerminalTransportEvent, TerminalTransportFactory,
};
pub use shellrs::update::{
    Channel, InstallKind, Installer, Relaunch, Release, Staged, TrustedKeys, Unsupported,
    UpdateError, UpdateFeed, UpdateServices,
};
pub use shellrs::workspace::Workspace;
pub use std::sync::atomic::{AtomicUsize, Ordering};
pub use std::sync::{Arc, Mutex, mpsc};
pub use std::time::Duration;

mod connection;
mod credential;
mod forward;
mod sftp;
mod terminal;

pub use connection::*;
pub use credential::*;
pub use forward::*;
pub use sftp::*;
pub use terminal::*;

/// Seeded host ids, in insertion order (see `HostStore::seed`).
pub const WEB_01: u64 = 1;
pub const WEB_02: u64 = 2;
pub const DB_01: u64 = 3;
/// The first SFTP tab a test opens; tab ids count up from 1.
pub const SFTP_TAB: u64 = 1;
pub const STAGING_API: u64 = 4;
pub const DEV_BOX: u64 = 6;
pub const INITIAL_WEB_TERMINAL: u64 = 1;
pub const INITIAL_STAGING_TERMINAL: u64 = 2;
pub const FIRST_NEW_TERMINAL: u64 = 3;
/// Seeded group ids, in insertion order: 生产, 测试, 开发.
pub const PRODUCTION: u64 = 1;
pub const DEVELOPMENT: u64 = 3;

pub fn open_workspace(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_store(cx, HostStore::seed())
}

/// Production loads the store from the database; the tests hand one in
/// directly so they get the fixed shape `HostStore::seed` describes.
pub fn open_workspace_with_store(
    cx: &mut TestAppContext,
    store: HostStore,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_tester(cx, store, Arc::new(FakeConnectionTester::default()))
}

pub fn one_host_store(auth: AuthKind) -> (HostStore, HostId) {
    let mut store = HostStore::empty();
    let id = store.insert_unnotified(HostDraft::new(
        "prompt-host",
        "example.test",
        22,
        "tester",
        auth,
        None,
    ));
    (store, id)
}

/// What every fixture starts with: ShellRS's globals, in Chinese, the
/// language the assertions are written in. The test's interface language is
/// its own, so one that switches to English switches no other test.
pub fn init_app(cx: &mut TestAppContext) {
    shellrs::i18n::isolate_thread();
    shellrs::i18n::set_locale("zh-CN");
    cx.update(shellrs::init);
}

/// Settings kept in memory, in Chinese: by default the interface follows the
/// system, and CI's is English.
pub fn settings_store() -> SettingsStore {
    let mut store = SettingsStore::in_memory();
    store.update_unnotified(|settings| settings.language = InterfaceLanguage::SimplifiedChinese);
    store
}

pub fn appearance_dropdown(window: &mut gpui_kit::Window, item: usize) -> Option<String> {
    window
        .within("settings")
        .within("group-0")
        .within(format!("item-{item}"))
        .find("btn")
        .label()
        .map(str::to_string)
}

/// `TestWindowExt::click` sends no modifiers, and file lists need ⌘ and
/// Shift clicks.
pub fn modified_click(
    window: &mut gpui_kit::Window,
    pane: (&'static str, u64),
    row: &str,
    modifiers: gpui_kit::Modifiers,
    cx: &mut App,
) {
    let row = ElementId::Name(row.to_string().into());
    let position = window.within(pane).find(row).bounds().center();
    click_at(window, position, modifiers, cx);
}

/// A left click at a point, for places without an element of their own.
pub fn click_at(
    window: &mut gpui_kit::Window,
    position: gpui_kit::Point<gpui_kit::Pixels>,
    modifiers: gpui_kit::Modifiers,
    cx: &mut App,
) {
    use gpui_kit::{MouseDownEvent, MouseUpEvent};
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
    window.dispatch_event(
        MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.dispatch_event(
        MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
}

/// Whether a button accepts input. gpui-base does not report `disabled` for
/// buttons, but a disabled one also leaves the focus order.
pub fn enabled(button: &gpui_kit::test::ElementSnapshot) -> bool {
    button.focused().is_some()
}

/// Draw a frame, run `body` against it, and let what it started settle.
pub fn in_frame<R>(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    body: impl FnOnce(&mut gpui_kit::Window, &mut App) -> R,
) -> R {
    let result = cx
        .update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            body(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    result
}

/// That the tabs of a tool's segmented bar, in the box `id`, share its
/// whole width evenly: `shared::count_tabs`.
pub fn assert_tabs_share_the_width(window: &mut gpui_kit::Window, id: &'static str, tabs: usize) {
    let bar = window.find(id).bounds();
    let widths: Vec<f32> = (0..tabs)
        .map(|index| f32::from(window.within(id).find(index).bounds().size.width))
        .collect();
    let first = window.within(id).find(0usize).bounds();
    let last = window.within(id).find(tabs - 1).bounds();
    assert!(
        f32::from(first.left() - bar.left()) < 8. && f32::from(bar.right() - last.right()) < 8.,
        "the tabs do not span the bar: {first:?} … {last:?} in {bar:?}"
    );
    assert!(
        widths.iter().all(|width| (width - widths[0]).abs() < 1.),
        "the tabs are not as wide as each other: {widths:?}"
    );
}
