//! How a host is reached: the host form's 「协议」. A Telnet host also keeps
//! the prompts its 自动登录 looks for; a serial host, how its line is set.

use gpui_shared_string::SharedString;

use super::SerialSettings;

/// How a host is reached. Chosen when the host is created and never changed
/// afterwards, so nothing that names an SSH host (a jump host, a forward)
/// can find it turned into something else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Protocol {
    #[default]
    Ssh,
    /// A terminal over plain TCP, reached as an SSH host is.
    Telnet(TelnetSettings),
    /// A serial port of this computer. The device is the host's address;
    /// its port, user, login and route play no part.
    Serial(SerialSettings),
}

impl Protocol {
    pub fn kind(&self) -> ProtocolKind {
        match self {
            Protocol::Ssh => ProtocolKind::Ssh,
            Protocol::Telnet(_) => ProtocolKind::Telnet,
            Protocol::Serial(_) => ProtocolKind::Serial,
        }
    }

    pub fn is_ssh(&self) -> bool {
        matches!(self, Protocol::Ssh)
    }

    pub fn is_telnet(&self) -> bool {
        matches!(self, Protocol::Telnet(_))
    }

    pub fn is_serial(&self) -> bool {
        matches!(self, Protocol::Serial(_))
    }

    pub fn serial(&self) -> Option<SerialSettings> {
        match self {
            Protocol::Serial(settings) => Some(*settings),
            _ => None,
        }
    }

    pub fn telnet(&self) -> Option<&TelnetSettings> {
        match self {
            Protocol::Telnet(settings) => Some(settings),
            _ => None,
        }
    }

    /// The stored spelling, also the external CLI's.
    pub fn as_str(&self) -> &'static str {
        self.kind().as_str()
    }
}

/// Which protocol, without its settings: what the form's 「协议」 picks and
/// what tells the features a host has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProtocolKind {
    #[default]
    Ssh,
    Telnet,
    Serial,
}

impl ProtocolKind {
    /// In the order the form lists them.
    pub const ALL: [ProtocolKind; 3] = [
        ProtocolKind::Ssh,
        ProtocolKind::Telnet,
        ProtocolKind::Serial,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ProtocolKind::Ssh => "ssh",
            ProtocolKind::Telnet => "telnet",
            ProtocolKind::Serial => "serial",
        }
    }

    pub fn is_ssh(self) -> bool {
        self == ProtocolKind::Ssh
    }

    pub fn is_telnet(self) -> bool {
        self == ProtocolKind::Telnet
    }

    pub fn is_serial(self) -> bool {
        self == ProtocolKind::Serial
    }

    /// Whether it has a network address, for 复制 IP: not a serial port.
    pub fn has_address(self) -> bool {
        !self.is_serial()
    }

    /// Whether 发送 Break means something: a serial line holds it, Telnet
    /// sends its own break, and SSH has none.
    pub fn sends_break(self) -> bool {
        !self.is_ssh()
    }
}

/// What a Telnet host's 自动登录 waits for before it types the username and
/// the password. Empty means the usual prompts, such as `login:` and
/// `Password:`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TelnetSettings {
    pub user_prompt: SharedString,
    pub password_prompt: SharedString,
}
