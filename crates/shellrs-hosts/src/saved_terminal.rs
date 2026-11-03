//! 项目: local terminals kept in groups, like hosts, each remembering the
//! folder its shell was last in. They live with the hosts because they are
//! stored in the same database; the `project` module that shows them
//! depends on this one.

use gpui_shared_string::SharedString;

/// Stable identity of a group of local terminals. Never reused within a
/// process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TerminalGroupId(pub u64);

/// A group of 项目. Groups do not nest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalGroup {
    pub id: TerminalGroupId,
    pub name: SharedString,
    /// Order among the groups.
    pub sort_order: i64,
    /// Whether its terminals show below it; a new group's do. Kept for the
    /// next launch, as a host group's is.
    pub expanded: bool,
}

/// Stable identity of a saved local terminal. Never reused within a
/// process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SavedTerminalId(pub u64);

/// A local terminal kept in 项目, in a group or at the top: there whether or
/// not its tab is open, and opened again in the folder its shell was last
/// in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedTerminal {
    pub id: SavedTerminalId,
    /// `None` at the top, below the groups.
    pub group: Option<TerminalGroupId>,
    pub name: SharedString,
    /// Named by hand: the name shows over any title its shell or a program
    /// gives its tab. A name it was given (`shellrs 2`) gives way to one.
    pub renamed: bool,
    /// The title its shell or a program gave its tab last, kept as the tab
    /// closed: shown for the name, as a title is, until another comes.
    pub title: Option<SharedString>,
    /// The folder its shell was last in, `None` before it said: the home
    /// folder then.
    pub directory: Option<String>,
    /// Order among the terminals of its group, or of the top.
    pub sort_order: i64,
}

/// What a row of 项目 stands for, encoded into the row's tree item id so
/// the renderer, the context menu and a drag can recover it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TerminalNode {
    Group(TerminalGroupId),
    Terminal(SavedTerminalId),
}

impl TerminalNode {
    pub fn id(self) -> SharedString {
        match self {
            TerminalNode::Group(TerminalGroupId(id)) => format!("g:{id}").into(),
            TerminalNode::Terminal(SavedTerminalId(id)) => format!("t:{id}").into(),
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        let (kind, number) = id.split_once(':')?;
        let number = number.parse().ok()?;
        match kind {
            "g" => Some(TerminalNode::Group(TerminalGroupId(number))),
            "t" => Some(TerminalNode::Terminal(SavedTerminalId(number))),
            _ => None,
        }
    }
}

/// Where a dragged row of 项目 lands: beside a row of its kind, or, for a
/// terminal, last in a group or last at the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalDrop {
    Before(TerminalNode),
    After(TerminalNode),
    Into(TerminalGroupId),
    Top,
}

/// A saved terminal's tab as it was open, for the next launch to open
/// again: listed in tab order, `front` for the one last in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedTerminalTab {
    pub terminal: SavedTerminalId,
    pub front: bool,
}

/// The name a new terminal gets: `prefix` (its group's name, or 终端 at
/// the top), numbered past the terminals beside it named so already, e.g.
/// `shellrs 3`.
pub fn next_terminal_name(prefix: &str, taken: &[&str]) -> String {
    (1..)
        .map(|number| format!("{prefix} {number}"))
        .find(|name| !taken.contains(&name.as_str()))
        .expect("an unused number")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ids_round_trip() {
        for node in [
            TerminalNode::Group(TerminalGroupId(7)),
            TerminalNode::Terminal(SavedTerminalId(42)),
        ] {
            assert_eq!(TerminalNode::parse(&node.id()), Some(node));
        }
        assert_eq!(TerminalNode::parse("p:1"), None);
        assert_eq!(TerminalNode::parse("t:x"), None);
    }

    #[test]
    fn a_new_terminal_takes_the_first_free_number() {
        assert_eq!(next_terminal_name("shellrs", &[]), "shellrs 1");
        assert_eq!(
            next_terminal_name("shellrs", &["shellrs 1", "claude", "shellrs 3"]),
            "shellrs 2"
        );
    }
}
