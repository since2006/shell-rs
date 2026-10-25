//! The center's tabs as data: which tabs there are and in what order, which
//! panes each holds, which tab is in front and which pane within each is
//! current. How a tab lays its panes out is its dock area's business; this
//! keeps only the panes' reading order, which the view hands back after
//! every layout change.
//!
//! Every edit keeps these true, and `check` asserts them in the tests:
//!
//! - every pane is in exactly one tab, and no tab is empty;
//! - a pane that stands alone (设置) is the only pane of its tab;
//! - a tab is in front exactly when there is a tab;
//! - each tab's current pane, and its last terminal if any, are its own.

use gpui_kit::SharedString;

/// What the model needs to know of a pane.
pub trait PaneKind: Copy + Eq + std::fmt::Debug {
    /// A terminal, which the right sidebar and 撰写窗格 go with.
    fn is_terminal(&self) -> bool;
    /// A pane that is a tab of its own and never shares one (设置).
    fn stands_alone(&self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u64);

#[derive(Clone, Debug)]
struct Tab<P> {
    id: TabId,
    /// In reading order: left to right, then top to bottom.
    panes: Vec<P>,
    /// The name given by hand; without it the tab is named after its
    /// panes.
    name: Option<SharedString>,
    /// The pane last focused, which the focus goes back to.
    current: P,
    /// The terminal last focused, for when the current pane is not one.
    last_terminal: Option<P>,
}

impl<P: PaneKind> Tab<P> {
    fn new(id: TabId, pane: P) -> Self {
        Self {
            id,
            panes: vec![pane],
            name: None,
            current: pane,
            last_terminal: pane.is_terminal().then_some(pane),
        }
    }

    fn remove(&mut self, pane: P) {
        let Some(ix) = self.panes.iter().position(|candidate| *candidate == pane) else {
            return;
        };
        self.panes.remove(ix);
        if self.current == pane
            && let Some(next) = self.panes.get(ix).or(self.panes.last())
        {
            self.current = *next;
        }
        if self.last_terminal == Some(pane) {
            self.last_terminal = self
                .current
                .is_terminal()
                .then_some(self.current)
                .or_else(|| self.panes.iter().copied().find(PaneKind::is_terminal));
        }
    }

    fn focus(&mut self, pane: P) {
        self.current = pane;
        if pane.is_terminal() {
            self.last_terminal = Some(pane);
        }
    }
}

/// How a tab is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabTitle<'a, P> {
    /// By hand, whatever its panes.
    Named(SharedString),
    /// After its panes, in reading order: one pane's own name, or several
    /// joined.
    Panes(&'a [P]),
}

#[derive(Clone, Debug)]
pub struct Tabs<P> {
    tabs: Vec<Tab<P>>,
    active: Option<TabId>,
    next_id: u64,
}

impl<P> Default for Tabs<P> {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            next_id: 1,
        }
    }
}

impl<P: PaneKind> Tabs<P> {
    // Reading.

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// The tabs in order.
    pub fn ids(&self) -> impl Iterator<Item = TabId> + '_ {
        self.tabs.iter().map(|tab| tab.id)
    }

    pub fn active(&self) -> Option<TabId> {
        self.active
    }

    pub fn tab_of(&self, pane: P) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|tab| tab.panes.contains(&pane))
            .map(|tab| tab.id)
    }

    /// A tab's panes in reading order.
    pub fn panes(&self, tab: TabId) -> &[P] {
        self.tab(tab).map_or(&[], |tab| &tab.panes)
    }

    /// Every pane, tab by tab.
    pub fn all_panes(&self) -> impl Iterator<Item = P> + '_ {
        self.tabs.iter().flat_map(|tab| tab.panes.iter().copied())
    }

    pub fn current(&self, tab: TabId) -> Option<P> {
        self.tab(tab).map(|tab| tab.current)
    }

    /// The pane in front: the current pane of the tab in front.
    pub fn current_pane(&self) -> Option<P> {
        self.current(self.active?)
    }

    /// The terminal the right sidebar and 撰写窗格 go with: the current pane
    /// when it is a terminal, else the terminal last used in the tab in
    /// front.
    pub fn current_terminal(&self) -> Option<P> {
        let tab = self.tab(self.active?)?;
        if tab.current.is_terminal() {
            Some(tab.current)
        } else {
            tab.last_terminal
        }
    }

    pub fn title(&self, tab: TabId) -> Option<TabTitle<'_, P>> {
        let tab = self.tab(tab)?;
        Some(match &tab.name {
            Some(name) => TabTitle::Named(name.clone()),
            None => TabTitle::Panes(&tab.panes),
        })
    }

    pub fn is_named(&self, tab: TabId) -> bool {
        self.tab(tab).is_some_and(|tab| tab.name.is_some())
    }

    /// Whether `from` may be dropped into another tab: only a tab of one
    /// pane, and never 设置.
    pub fn can_merge(&self, from: TabId, into: TabId) -> bool {
        let (Some(source), Some(target)) = (self.tab(from), self.tab(into)) else {
            return false;
        };
        from != into
            && source.panes.len() == 1
            && !source.panes[0].stands_alone()
            && !target.panes.iter().any(PaneKind::stands_alone)
    }

    /// Whether a pane may be moved into `into`, from wherever it is.
    pub fn can_move_into(&self, pane: P, into: TabId) -> bool {
        let Some(target) = self.tab(into) else {
            return false;
        };
        !pane.stands_alone()
            && !target.panes.iter().any(PaneKind::stands_alone)
            && self.tab_of(pane) != Some(into)
    }

    // Editing.

    /// A new tab of its own for `pane`, last, in front.
    pub fn open(&mut self, pane: P) -> TabId {
        debug_assert!(self.tab_of(pane).is_none(), "{pane:?} is open already");
        let id = self.new_id();
        self.tabs.push(Tab::new(id, pane));
        self.active = Some(id);
        id
    }

    /// Bring `tab` to the front.
    pub fn activate(&mut self, tab: TabId) {
        if self.tab(tab).is_some() {
            self.active = Some(tab);
        }
    }

    /// The focus went into `pane`: its tab is in front and it is current.
    pub fn focus(&mut self, pane: P) {
        let Some(tab) = self.tab_mut_of(pane) else {
            return;
        };
        tab.focus(pane);
        self.active = Some(tab.id);
    }

    /// Take `pane` out for good. Its tab goes with its last pane; the tab
    /// in front going, the one to its right comes forward, else the one to
    /// its left. Returns the tab that went, if one did.
    pub fn close_pane(&mut self, pane: P) -> Option<TabId> {
        let tab = self.tab_mut_of(pane)?;
        tab.remove(pane);
        let id = tab.id;
        if tab.panes.is_empty() {
            self.remove_tab(id);
            return Some(id);
        }
        None
    }

    /// Take a whole tab out. Returns its panes, for their own closing.
    pub fn close_tab(&mut self, tab: TabId) -> Vec<P> {
        let panes = self.panes(tab).to_vec();
        self.remove_tab(tab);
        panes
    }

    /// Move a tab to `ix` among the tabs.
    pub fn move_tab(&mut self, tab: TabId, ix: usize) {
        let Some(from) = self.position(tab) else {
            return;
        };
        let moved = self.tabs.remove(from);
        let ix = ix.min(self.tabs.len());
        self.tabs.insert(ix, moved);
    }

    /// Move `pane` from its tab into `into`, where it is current and in
    /// front. Its tab goes with its last pane. Where in `into` it lands is
    /// the dock area's to say; `set_order` hands that back.
    pub fn move_pane(&mut self, pane: P, into: TabId) -> bool {
        if !self.can_move_into(pane, into) {
            return false;
        }
        let Some(source) = self.tab_mut_of(pane) else {
            return false;
        };
        source.remove(pane);
        let (source, emptied) = (source.id, source.panes.is_empty());
        if emptied {
            self.tabs.retain(|tab| tab.id != source);
        }
        let target = self.tab_mut(into).expect("checked above");
        target.panes.push(pane);
        target.focus(pane);
        self.active = Some(into);
        true
    }

    /// Drop the one pane of `from` into `into`. `from` goes.
    pub fn merge(&mut self, from: TabId, into: TabId) -> Option<P> {
        if !self.can_merge(from, into) {
            return None;
        }
        let pane = self.panes(from)[0];
        self.move_pane(pane, into).then_some(pane)
    }

    /// Give `pane` a tab of its own at `ix`, in front. A pane alone in its
    /// tab keeps that tab, which just moves.
    pub fn split_off(&mut self, pane: P, ix: usize) -> Option<TabId> {
        let source = self.tab_mut_of(pane)?;
        if source.panes.len() == 1 {
            let id = source.id;
            self.move_tab(id, ix);
            self.active = Some(id);
            return Some(id);
        }
        source.remove(pane);
        let id = self.new_id();
        let ix = ix.min(self.tabs.len());
        self.tabs.insert(ix, Tab::new(id, pane));
        self.active = Some(id);
        Some(id)
    }

    /// The panes of `tab` in reading order, as its dock area lays them out
    /// now. The same panes as before, in any order.
    pub fn set_order(&mut self, tab: TabId, order: Vec<P>) {
        let Some(tab) = self.tab_mut(tab) else {
            return;
        };
        let same =
            order.len() == tab.panes.len() && order.iter().all(|pane| tab.panes.contains(pane));
        debug_assert!(same, "the order {order:?} is not of {:?}", tab.panes);
        if same {
            tab.panes = order;
        }
    }

    /// Name a tab by hand, or with `None` after its panes again.
    pub fn rename(&mut self, tab: TabId, name: Option<SharedString>) {
        if let Some(tab) = self.tab_mut(tab) {
            tab.name = name.filter(|name| !name.trim().is_empty());
        }
    }

    // Inside.

    fn new_id(&mut self) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        id
    }

    fn position(&self, tab: TabId) -> Option<usize> {
        self.tabs.iter().position(|candidate| candidate.id == tab)
    }

    fn tab(&self, tab: TabId) -> Option<&Tab<P>> {
        self.tabs.iter().find(|candidate| candidate.id == tab)
    }

    fn tab_mut(&mut self, tab: TabId) -> Option<&mut Tab<P>> {
        self.tabs.iter_mut().find(|candidate| candidate.id == tab)
    }

    fn tab_mut_of(&mut self, pane: P) -> Option<&mut Tab<P>> {
        self.tabs.iter_mut().find(|tab| tab.panes.contains(&pane))
    }

    fn remove_tab(&mut self, tab: TabId) {
        let Some(ix) = self.position(tab) else {
            return;
        };
        self.tabs.remove(ix);
        if self.active == Some(tab) {
            self.active = self
                .tabs
                .get(ix)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id);
        }
    }

    /// Assert the invariants of the module comment.
    #[cfg(test)]
    fn check(&self) {
        let mut seen = Vec::new();
        for tab in &self.tabs {
            assert!(!tab.panes.is_empty(), "tab {:?} is empty", tab.id);
            for pane in &tab.panes {
                assert!(!seen.contains(pane), "{pane:?} is in two tabs");
                seen.push(*pane);
            }
            if tab.panes.iter().any(PaneKind::stands_alone) {
                assert_eq!(tab.panes.len(), 1, "{:?} shares its tab", tab.panes);
            }
            assert!(tab.panes.contains(&tab.current));
            if let Some(terminal) = tab.last_terminal {
                assert!(tab.panes.contains(&terminal) && terminal.is_terminal());
            }
        }
        assert_eq!(self.active.is_some(), !self.tabs.is_empty());
        if let Some(active) = self.active {
            assert!(self.tab(active).is_some());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Pane {
        Terminal(u8),
        Sftp(u8),
        Settings,
    }
    use Pane::*;

    impl PaneKind for Pane {
        fn is_terminal(&self) -> bool {
            matches!(self, Terminal(_))
        }
        fn stands_alone(&self) -> bool {
            matches!(self, Settings)
        }
    }

    fn tabs_of(panes: &[Pane]) -> (Tabs<Pane>, Vec<TabId>) {
        let mut tabs = Tabs::default();
        let ids = panes.iter().map(|pane| tabs.open(*pane)).collect();
        tabs.check();
        (tabs, ids)
    }

    fn layout(tabs: &Tabs<Pane>) -> Vec<Vec<Pane>> {
        tabs.ids().map(|tab| tabs.panes(tab).to_vec()).collect()
    }

    #[test]
    fn a_new_pane_opens_a_tab_last_and_in_front() {
        let (tabs, ids) = tabs_of(&[Terminal(1), Sftp(1)]);
        assert_eq!(layout(&tabs), [vec![Terminal(1)], vec![Sftp(1)]]);
        assert_eq!(tabs.active(), Some(ids[1]));
        assert_eq!(tabs.current_pane(), Some(Sftp(1)));
    }

    #[test]
    fn closing_the_tab_in_front_brings_its_right_neighbour_else_its_left() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2), Terminal(3)]);
        tabs.activate(ids[1]);
        assert_eq!(tabs.close_pane(Terminal(2)), Some(ids[1]));
        assert_eq!(tabs.active(), Some(ids[2]));
        assert_eq!(tabs.close_tab(ids[2]), vec![Terminal(3)]);
        assert_eq!(tabs.active(), Some(ids[0]));
        tabs.close_pane(Terminal(1));
        tabs.check();
        assert!(tabs.is_empty());
        assert_eq!(tabs.active(), None);
    }

    #[test]
    fn closing_a_tab_behind_leaves_the_one_in_front() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2)]);
        tabs.close_tab(ids[0]);
        tabs.check();
        assert_eq!(tabs.active(), Some(ids[1]));
    }

    #[test]
    fn a_tab_of_one_pane_merges_into_another_and_goes() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2)]);
        assert_eq!(tabs.merge(ids[1], ids[0]), Some(Terminal(2)));
        tabs.check();
        assert_eq!(layout(&tabs), [vec![Terminal(1), Terminal(2)]]);
        assert_eq!(tabs.active(), Some(ids[0]));
        assert_eq!(tabs.current_pane(), Some(Terminal(2)));
    }

    #[test]
    fn a_tab_of_several_panes_or_settings_does_not_merge() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2), Terminal(3), Settings]);
        tabs.merge(ids[1], ids[0]);
        // Several panes.
        assert!(!tabs.can_merge(ids[0], ids[2]));
        assert_eq!(tabs.merge(ids[0], ids[2]), None);
        // 设置, either way.
        assert!(!tabs.can_merge(ids[3], ids[2]));
        assert!(!tabs.can_merge(ids[2], ids[3]));
        // Into itself.
        assert!(!tabs.can_merge(ids[2], ids[2]));
        tabs.check();
        assert_eq!(
            layout(&tabs),
            [
                vec![Terminal(1), Terminal(2)],
                vec![Terminal(3)],
                vec![Settings]
            ]
        );
    }

    #[test]
    fn a_pane_moves_between_tabs_and_an_emptied_tab_goes() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2), Sftp(1)]);
        tabs.merge(ids[1], ids[0]);
        assert!(tabs.move_pane(Terminal(2), ids[2]));
        tabs.check();
        assert_eq!(
            layout(&tabs),
            [vec![Terminal(1)], vec![Sftp(1), Terminal(2)]]
        );
        assert_eq!(tabs.active(), Some(ids[2]));
        assert!(tabs.move_pane(Terminal(1), ids[2]));
        tabs.check();
        assert_eq!(layout(&tabs), [vec![Sftp(1), Terminal(2), Terminal(1)]]);
        // Not into its own tab, not into 设置's.
        assert!(!tabs.move_pane(Terminal(1), ids[2]));
        let settings = tabs.open(Settings);
        assert!(!tabs.move_pane(Terminal(1), settings));
        tabs.check();
    }

    #[test]
    fn a_pane_split_off_gets_a_tab_where_it_is_dropped() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2), Terminal(3)]);
        tabs.merge(ids[1], ids[0]);
        let new = tabs.split_off(Terminal(1), 1).unwrap();
        tabs.check();
        assert_eq!(
            layout(&tabs),
            [vec![Terminal(2)], vec![Terminal(1)], vec![Terminal(3)]]
        );
        assert_eq!(tabs.active(), Some(new));
        // A pane alone keeps its tab, which moves.
        assert_eq!(tabs.split_off(Terminal(3), 0), Some(ids[2]));
        tabs.check();
        assert_eq!(
            layout(&tabs),
            [vec![Terminal(3)], vec![Terminal(2)], vec![Terminal(1)]]
        );
    }

    #[test]
    fn tabs_move_among_themselves() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2), Terminal(3)]);
        tabs.move_tab(ids[0], 2);
        assert_eq!(tabs.ids().collect::<Vec<_>>(), [ids[1], ids[2], ids[0]]);
        tabs.move_tab(ids[0], 99);
        assert_eq!(tabs.ids().collect::<Vec<_>>(), [ids[1], ids[2], ids[0]]);
        tabs.move_tab(ids[2], 0);
        assert_eq!(tabs.ids().collect::<Vec<_>>(), [ids[2], ids[1], ids[0]]);
        tabs.check();
    }

    #[test]
    fn the_current_terminal_is_the_last_one_used_when_the_pane_is_not_one() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Sftp(1), Terminal(2)]);
        tabs.merge(ids[1], ids[0]);
        tabs.merge(ids[2], ids[0]);
        tabs.focus(Terminal(1));
        tabs.focus(Sftp(1));
        assert_eq!(tabs.current_pane(), Some(Sftp(1)));
        assert_eq!(tabs.current_terminal(), Some(Terminal(1)));
        tabs.focus(Terminal(2));
        tabs.focus(Sftp(1));
        assert_eq!(tabs.current_terminal(), Some(Terminal(2)));
        // That terminal closing, the tab's other one stands in.
        tabs.close_pane(Terminal(2));
        tabs.check();
        assert_eq!(tabs.current_terminal(), Some(Terminal(1)));
        tabs.close_pane(Terminal(1));
        tabs.check();
        assert_eq!(tabs.current_terminal(), None);
    }

    #[test]
    fn a_tab_without_a_terminal_has_no_current_terminal() {
        let (mut tabs, _) = tabs_of(&[Terminal(1), Sftp(1)]);
        assert_eq!(tabs.current_terminal(), None);
        tabs.focus(Terminal(1));
        assert_eq!(tabs.current_terminal(), Some(Terminal(1)));
    }

    #[test]
    fn closing_the_current_pane_makes_its_neighbour_current() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2), Terminal(3)]);
        tabs.merge(ids[1], ids[0]);
        tabs.merge(ids[2], ids[0]);
        tabs.focus(Terminal(2));
        tabs.close_pane(Terminal(2));
        assert_eq!(tabs.current_pane(), Some(Terminal(3)));
        tabs.close_pane(Terminal(3));
        assert_eq!(tabs.current_pane(), Some(Terminal(1)));
        tabs.check();
    }

    #[test]
    fn focus_brings_the_panes_tab_to_the_front() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2)]);
        tabs.focus(Terminal(1));
        assert_eq!(tabs.active(), Some(ids[0]));
    }

    #[test]
    fn a_tab_is_named_after_its_panes_until_it_is_named_by_hand() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1), Terminal(2)]);
        tabs.merge(ids[1], ids[0]);
        tabs.set_order(ids[0], vec![Terminal(2), Terminal(1)]);
        assert_eq!(
            tabs.title(ids[0]),
            Some(TabTitle::Panes(&[Terminal(2), Terminal(1)][..]))
        );
        tabs.rename(ids[0], Some("web".into()));
        assert_eq!(tabs.title(ids[0]), Some(TabTitle::Named("web".into())));
        assert!(tabs.is_named(ids[0]));
        // A blank name is no name.
        tabs.rename(ids[0], Some("  ".into()));
        assert!(!tabs.is_named(ids[0]));
    }

    #[test]
    fn an_order_of_other_panes_is_refused() {
        let (mut tabs, ids) = tabs_of(&[Terminal(1)]);
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tabs.set_order(ids[0], vec![Terminal(9)])
        }));
        // A debug build says so; a release build keeps the order it had.
        assert!(refused.is_err() || tabs.panes(ids[0]) == [Terminal(1)]);
    }

    /// Every edit, in a long deterministic mix, keeps the invariants.
    #[test]
    fn edits_keep_the_invariants() {
        let mut tabs = Tabs::default();
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move |below: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % below.max(1) as u64) as usize
        };
        let mut opened = 0u8;
        for _ in 0..2000 {
            let ids: Vec<TabId> = tabs.ids().collect();
            let panes: Vec<Pane> = tabs.all_panes().collect();
            match next(8) {
                0 | 1 => {
                    opened = opened.wrapping_add(1);
                    let pane = match next(10) {
                        0 if tabs.tab_of(Settings).is_none() => Settings,
                        1..=3 => Sftp(opened),
                        _ => Terminal(opened),
                    };
                    if tabs.tab_of(pane).is_none() {
                        tabs.open(pane);
                    }
                }
                2 if !panes.is_empty() => {
                    tabs.close_pane(panes[next(panes.len())]);
                }
                3 if !ids.is_empty() => {
                    tabs.merge(ids[next(ids.len())], ids[next(ids.len())]);
                }
                4 if !ids.is_empty() && !panes.is_empty() => {
                    tabs.move_pane(panes[next(panes.len())], ids[next(ids.len())]);
                }
                5 if !panes.is_empty() => {
                    tabs.split_off(panes[next(panes.len())], next(ids.len() + 2));
                }
                6 if !panes.is_empty() => tabs.focus(panes[next(panes.len())]),
                7 if !ids.is_empty() => tabs.move_tab(ids[next(ids.len())], next(ids.len())),
                _ => {}
            }
            tabs.check();
        }
    }
}
