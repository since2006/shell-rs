//! 快速连接: the start page's palette for reaching saved hosts by typing a
//! few letters of them.
//!
//! The list is gpui-kit's `List` (its search box, ↑↓, Enter and a click
//! connect the highlighted host). Choosing several and connecting them
//! together is built but switched off, see [`MULTIPLE`]: the search box
//! would keep Tab, ⌘A and ⇧↵ for itself, so the palette takes those actions
//! on their way to it, in the capture phase: Tab chooses the highlighted
//! host, ⌘A every host shown, ⇧↵ connects the chosen ones.

use std::collections::HashSet;

use gpui_kit::component::{
    ActiveTheme as _, IconName, IndexPath, Sizable as _, WindowExt as _,
    dialog::DialogFooter,
    h_flex,
    input::{Enter, IndentInline, SelectAll},
    kbd::Kbd,
    list::{List, ListDelegate, ListItem, ListState},
    v_flex,
};
use gpui_kit::*;

use gpui_kit::prelude::FluentBuilder as _;

use crate::app::ConnectHost;
use crate::i18n::t;
use crate::shared::HostMark;

use super::{HostId, HostOs, HostStore};

/// Whether several hosts can be chosen (Tab, ⌘A, ⌘-click) and connected to
/// together (⇧↵), with the keys in the footer. Off for now (the user's
/// call); turning it on brings all of that back.
const MULTIPLE: bool = false;

/// A host as the palette lists it, as it was when the palette opened: the
/// dialog is modal, so nothing changes under it.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickHost {
    pub id: HostId,
    pub name: SharedString,
    pub user: SharedString,
    pub address: SharedString,
    /// `user@address:port`.
    pub endpoint: SharedString,
    /// The full group path (`公司 / MySQL 集群`), when it has a group.
    pub group: Option<SharedString>,
    pub notes: SharedString,
    pub os: Option<HostOs>,
}

impl QuickHost {
    /// Whether every word of `query` is somewhere in the name, address,
    /// user, notes or group path, whatever the case.
    pub fn matches(&self, query: &str) -> bool {
        let fields = [
            &self.name,
            &self.address,
            &self.user,
            &self.notes,
            self.group.as_ref().unwrap_or(&self.name),
        ]
        .map(|field| field.to_lowercase());
        query
            .to_lowercase()
            .split_whitespace()
            .all(|word| fields.iter().any(|field| field.contains(word)))
    }

    /// The line under the name: the group, then where it logs in.
    pub fn detail(&self) -> String {
        match &self.group {
            Some(group) => format!("{group} · {}", self.endpoint),
            None => self.endpoint.to_string(),
        }
    }
}

/// The saved hosts, those connected to lately first (the latest first), then
/// the rest by name.
pub fn quick_hosts(store: &HostStore) -> Vec<QuickHost> {
    let of = |host: &super::Host| QuickHost {
        id: host.id,
        name: host.name.clone(),
        user: host.user.clone(),
        address: host.address.clone(),
        endpoint: host.endpoint().into(),
        group: host
            .group
            .map(|group| SharedString::from(store.group_path(group))),
        notes: host.notes.clone(),
        os: host.os,
    };
    let mut hosts: Vec<QuickHost> = store
        .recent_hosts()
        .filter(|host| !store.is_temporary(host.id))
        .map(of)
        .collect();
    let recent: HashSet<HostId> = hosts.iter().map(|host| host.id).collect();
    let mut rest: Vec<QuickHost> = store
        .hosts()
        .iter()
        .filter(|host| !recent.contains(&host.id))
        .map(of)
        .collect();
    rest.sort_by_key(|host| host.name.to_lowercase());
    hosts.extend(rest);
    hosts
}

/// What the palette shows, which row is highlighted and which hosts have
/// been chosen: the list's state, apart from the list.
pub struct QuickChoice {
    hosts: Vec<QuickHost>,
    /// Indices into `hosts` of the ones the query matches, in order.
    shown: Vec<usize>,
    /// Chosen with Tab or ⌘A, for ⇧↵ to connect together. Kept while the
    /// query changes, so hosts found by different searches go together.
    chosen: HashSet<HostId>,
    /// The highlighted row of `shown`.
    selected: Option<usize>,
}

impl QuickChoice {
    pub fn new(hosts: Vec<QuickHost>) -> Self {
        Self {
            shown: (0..hosts.len()).collect(),
            hosts,
            chosen: HashSet::new(),
            selected: None,
        }
    }

    /// Show the hosts `query` matches.
    pub fn search(&mut self, query: &str) {
        self.shown = (0..self.hosts.len())
            .filter(|ix| self.hosts[*ix].matches(query))
            .collect();
    }

    /// Highlight a row of what is shown.
    pub fn highlight(&mut self, row: Option<usize>) {
        self.selected = row;
    }

    /// The host on a row of what is shown.
    pub fn shown(&self, row: usize) -> Option<&QuickHost> {
        self.shown.get(row).map(|ix| &self.hosts[*ix])
    }

    pub fn shown_count(&self) -> usize {
        self.shown.len()
    }

    pub fn is_chosen(&self, id: HostId) -> bool {
        self.chosen.contains(&id)
    }

    fn highlighted(&self) -> Option<HostId> {
        self.selected
            .and_then(|row| self.shown(row))
            .map(|host| host.id)
    }

    /// Tab: choose the highlighted host, or take it back.
    pub fn toggle_highlighted(&mut self) {
        if let Some(id) = self.highlighted()
            && !self.chosen.remove(&id)
        {
            self.chosen.insert(id);
        }
    }

    /// ⌘A: choose every host shown; when they all are already, take them
    /// back.
    pub fn choose_all(&mut self) {
        let shown: Vec<HostId> = self.shown.iter().map(|ix| self.hosts[*ix].id).collect();
        if shown.iter().all(|id| self.chosen.contains(id)) {
            for id in &shown {
                self.chosen.remove(id);
            }
        } else {
            self.chosen.extend(shown);
        }
    }

    /// What Enter connects: the highlighted host.
    pub fn to_connect_one(&self) -> Vec<HostId> {
        self.highlighted().into_iter().collect()
    }

    /// What ⇧↵ connects: the chosen hosts in the list's order, or the
    /// highlighted one when none is chosen.
    pub fn to_connect(&self) -> Vec<HostId> {
        let chosen: Vec<HostId> = self
            .hosts
            .iter()
            .map(|host| host.id)
            .filter(|id| self.chosen.contains(id))
            .collect();
        if chosen.is_empty() {
            self.to_connect_one()
        } else {
            chosen
        }
    }

    fn has_hosts(&self) -> bool {
        !self.hosts.is_empty()
    }
}

/// The palette's list: its choice, and where to ask for connections.
pub struct QuickConnectList {
    choice: QuickChoice,
    /// The workspace's focus handle, which connecting is asked of: once the
    /// dialog has closed, the focus is nowhere.
    dispatch: FocusHandle,
}

impl QuickConnectList {
    pub fn new(hosts: Vec<QuickHost>, dispatch: FocusHandle) -> Self {
        Self {
            choice: QuickChoice::new(hosts),
            dispatch,
        }
    }

    pub fn choice(&self) -> &QuickChoice {
        &self.choice
    }

    pub fn choice_mut(&mut self) -> &mut QuickChoice {
        &mut self.choice
    }

    /// Close the palette and connect to `hosts`, each in a terminal tab.
    pub fn connect(&self, hosts: Vec<HostId>, window: &mut Window, cx: &mut App) {
        if hosts.is_empty() {
            return;
        }
        window.close_dialog(cx);
        let dispatch = self.dispatch.clone();
        window.defer(cx, move |window, cx| {
            for host in hosts {
                dispatch.dispatch_action(&ConnectHost(host), window, cx);
            }
        });
    }
}

impl ListDelegate for QuickConnectList {
    type Item = ListItem;

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.choice.search(query);
        Task::ready(())
    }

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.choice.shown_count()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let host = self.choice.shown(ix.row)?;
        let chosen = self.choice.is_chosen(host.id);
        let detail = host.detail();
        let muted = cx.theme().muted_foreground;
        Some(
            ListItem::new(("quick-connect-row", host.id.0))
                .py_1p5()
                .rounded(cx.theme().radius)
                .when(MULTIPLE, |item| {
                    item.check_icon(IconName::Check).confirmed(chosen)
                })
                .child(
                    h_flex()
                        .id(("quick-connect-host", host.id.0))
                        .test_support()
                        .aria_label(SharedString::from(format!("{} · {detail}", host.name)))
                        .aria_selected(chosen)
                        .gap_3()
                        .child(
                            HostMark::new(
                                ("quick-connect-os", host.id.0),
                                host.name.clone(),
                                host.os,
                            )
                            .without_tooltip()
                            .small(),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(div().truncate().child(host.name.clone()))
                                .child(div().text_sm().text_color(muted).truncate().child(detail)),
                        ),
                ),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.choice.highlight(ix.map(|ix| ix.row));
    }

    /// Enter or a click connects the highlighted host; with ⌘ (Ctrl) and
    /// [`MULTIPLE`] on, it is chosen instead, as Tab does.
    fn confirm(&mut self, secondary: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        if secondary && MULTIPLE {
            self.choice.toggle_highlighted();
            cx.notify();
        } else {
            self.connect(self.choice.to_connect_one(), window, cx);
        }
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let text = if !self.choice.has_hosts() {
            t!("host.quick_connect.no_hosts")
        } else {
            t!("host.quick_connect.no_matches")
        };
        h_flex()
            .id("quick-connect-empty")
            .test_support()
            .aria_label(text.clone())
            .size_full()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }
}

/// One key and what it does, for the footer.
fn key_hint(keys: &str, label: SharedString) -> impl IntoElement {
    h_flex()
        .gap_1p5()
        .children(Keystroke::parse(keys).ok().map(Kbd::new))
        .child(label)
}

/// Open 快速连接: search the saved hosts and connect to one, or choose
/// several and connect to them together. `dispatch` is the workspace's
/// focus handle.
pub fn open_quick_connect_dialog(
    store: Entity<HostStore>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let hosts = quick_hosts(store.read(cx));
    // Nothing is highlighted until there is something to go by (the user's
    // call: a host picked in advance read as chosen for them). Searching
    // highlights the first match, as `List` does, and ↓ the first host.
    let list = cx.new(|cx| {
        ListState::new(QuickConnectList::new(hosts, dispatch), window, cx).searchable(true)
    });
    let select_all = if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    };
    window.open_dialog(cx, {
        let list = list.clone();
        move |dialog, _, cx| {
            let theme = cx.theme();
            dialog
                .title(t!("host.quick_connect.title"))
                .overlay_closable(false)
                .child(
                    div()
                        .id("quick-connect")
                        .test_support()
                        .w_full()
                        .h(rems(26.))
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius)
                        .overflow_hidden()
                        // The search box's own Tab, ⌘A and ⇧↵, taken on the
                        // way to it.
                        .when(MULTIPLE, |palette| {
                            palette
                                .capture_action({
                                    let list = list.clone();
                                    move |_: &IndentInline, _, cx| {
                                        list.update(cx, |list, cx| {
                                            list.delegate_mut().choice_mut().toggle_highlighted();
                                            cx.notify();
                                        });
                                        cx.stop_propagation();
                                    }
                                })
                                .capture_action({
                                    let list = list.clone();
                                    move |_: &SelectAll, _, cx| {
                                        list.update(cx, |list, cx| {
                                            list.delegate_mut().choice_mut().choose_all();
                                            cx.notify();
                                        });
                                        cx.stop_propagation();
                                    }
                                })
                                .capture_action({
                                    let list = list.clone();
                                    move |enter: &Enter, window, cx| {
                                        if enter.shift {
                                            list.update(cx, |list, cx| {
                                                let hosts = list.delegate().choice().to_connect();
                                                list.delegate().connect(hosts, window, cx);
                                            });
                                            cx.stop_propagation();
                                        }
                                    }
                                })
                        })
                        .child(
                            List::new(&list).search_placeholder(t!("host.quick_connect.search")),
                        ),
                )
                .when(MULTIPLE, |dialog| {
                    dialog.footer(
                        DialogFooter::new()
                            .w_full()
                            .justify_end()
                            .gap_4()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(key_hint("tab", t!("host.quick_connect.choose")))
                            .child(key_hint(select_all, t!("host.quick_connect.choose_all")))
                            .child(key_hint(
                                "shift-enter",
                                t!("host.quick_connect.connect_chosen"),
                            )),
                    )
                })
        }
    });
    // The search box takes the keys at once: focusing it later in another
    // update would lose to the dialog's own focus.
    list.update(cx, |list, cx| list.focus(window, cx));
}

#[cfg(test)]
mod tests {
    use super::super::{
        AuthKind, ConnectionState, GroupDraft, GroupId, HostDraft, HostId, HostStore,
    };
    use super::{QuickChoice, QuickHost, quick_hosts};

    fn host(name: &str, address: &str, group: Option<GroupId>) -> HostDraft {
        HostDraft::new(name, address, 22, "root", AuthKind::Password, group)
    }

    #[test]
    fn the_hosts_connected_to_lately_come_first_then_the_rest_by_name() {
        let mut store = HostStore::empty();
        let group = store.insert_group_unnotified(GroupDraft::new("公司", None));
        let zen = store.insert_unnotified(host("禅道", "8.138.95.125", None));
        let web = store.insert_unnotified(host("web", "10.0.0.1", Some(group)));
        let db = store.insert_unnotified(host("DB", "10.0.0.2", None));
        let api = store.insert_unnotified(host("api", "10.0.0.3", None));
        store.set_state_unnotified(web, ConnectionState::Connected);
        store.set_state_unnotified(zen, ConnectionState::Connected);

        let hosts = quick_hosts(&store);
        let ids: Vec<HostId> = hosts.iter().map(|host| host.id).collect();
        assert_eq!(ids, [zen, web, api, db]);
        assert_eq!(hosts[0].detail(), "root@8.138.95.125:22");
        assert_eq!(hosts[1].detail(), "公司 · root@10.0.0.1:22");
    }

    fn quick(id: u64, name: &str) -> QuickHost {
        QuickHost {
            id: HostId(id),
            name: name.into(),
            user: "deploy".into(),
            address: "172.16.1.166".into(),
            endpoint: "deploy@172.16.1.166:22".into(),
            group: Some("公司 / CI".into()),
            notes: "构建服务器".into(),
            os: None,
        }
    }

    #[test]
    fn every_word_is_looked_for_in_the_name_address_user_notes_and_group() {
        let host = quick(1, "teamcity");
        for query in ["", "TEAM", "172.16", "deploy", "构建", "公司 ci", "ci 166"] {
            assert!(host.matches(query), "{query:?}");
        }
        for query in ["jenkins", "公司 jenkins"] {
            assert!(!host.matches(query), "{query:?}");
        }
    }

    #[test]
    fn tab_and_select_all_choose_what_shift_enter_connects() {
        let mut choice =
            QuickChoice::new(vec![quick(1, "alpha"), quick(2, "beta"), quick(3, "gamma")]);
        // Nothing chosen: the highlighted host, Enter's as well.
        choice.highlight(Some(1));
        assert_eq!(choice.to_connect(), [HostId(2)]);
        assert_eq!(choice.to_connect_one(), [HostId(2)]);

        // Tab chooses and takes back; the list's order, not the choosing's.
        choice.toggle_highlighted();
        choice.highlight(Some(0));
        choice.toggle_highlighted();
        assert_eq!(choice.to_connect(), [HostId(1), HostId(2)]);
        choice.toggle_highlighted();
        assert_eq!(choice.to_connect(), [HostId(2)]);

        // ⌘A chooses what is shown, then takes it all back. A choice made
        // under another search stays.
        choice.search("gamma");
        assert_eq!(choice.shown_count(), 1);
        choice.choose_all();
        assert_eq!(choice.to_connect(), [HostId(2), HostId(3)]);
        choice.search("");
        choice.choose_all();
        assert_eq!(choice.to_connect(), [HostId(1), HostId(2), HostId(3)]);
        choice.choose_all();
        choice.highlight(Some(2));
        assert_eq!(choice.to_connect(), [HostId(3)]);
    }
}
