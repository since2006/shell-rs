//! A place in the host tree: a group or a host, and where one is dropped.

use gpui_shared_string::SharedString;

use super::{GroupId, HostId};

/// What a tree row stands for. Encoded into the row's `TreeItem` id so the
/// renderer and the context menu can recover the domain object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostNode {
    Group(GroupId),
    Host(HostId),
}

/// A drop location in the host tree. Groups and hosts each have their
/// own order within a parent; a drop into a group changes the parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeDrop {
    Before(HostNode),
    After(HostNode),
    Into(GroupId),
    Root,
}

impl HostNode {
    pub fn id(self) -> SharedString {
        match self {
            HostNode::Group(GroupId(id)) => format!("g:{id}").into(),
            HostNode::Host(HostId(id)) => format!("s:{id}").into(),
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        let (kind, number) = id.split_once(':')?;
        let number = number.parse().ok()?;
        match kind {
            "g" => Some(HostNode::Group(GroupId(number))),
            "s" => Some(HostNode::Host(HostId(number))),
            _ => None,
        }
    }

    pub fn host_id(self) -> Option<HostId> {
        match self {
            HostNode::Host(id) => Some(id),
            HostNode::Group(_) => None,
        }
    }

    pub fn group_id(self) -> Option<GroupId> {
        match self {
            HostNode::Group(id) => Some(id),
            HostNode::Host(_) => None,
        }
    }
}
