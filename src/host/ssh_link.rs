//! `ssh://user[:password]@host[:port]`: how a bastion host opens Xshell, and
//! so ShellRS, on a host the user has not saved. ShellRS connects to it as an
//! 外部连接: not saved, gone with its tabs, the terminal alone.

use std::fmt;

use percent_encoding::percent_decode_str;
use url::{Host as UrlHost, ParseError, Url};
use zeroize::Zeroizing;

use super::{AuthKind, HostDraft};

/// The port when the link names none.
const SSH_PORT: u16 = 22;

/// What a link says to connect to.
pub struct SshLink {
    /// The tab's name: what the bastion host called it (Xshell's
    /// `-newtab`), or else the address.
    pub name: String,
    /// Without brackets, for an IPv6 address.
    pub address: String,
    pub port: u16,
    pub user: String,
    /// Kept in memory only, never in the keychain or the database.
    pub password: Option<Zeroizing<String>>,
}

impl SshLink {
    /// Read `link`, named `tab` when the caller gave a name. The error says
    /// what is wrong without repeating the link, which may hold a password.
    pub fn parse(link: &str, tab: Option<&str>) -> Result<Self, String> {
        let link = link.trim();
        if link.is_empty() {
            return Err("启动参数里没有要连接的地址，应为 ssh://用户@地址[:端口]。".into());
        }
        let malformed = || "链接的格式不对，应为 ssh://用户[:密码]@地址[:端口]。".to_string();
        // Without `://`, `user:password@host` would read as the scheme
        // `user`, and the error would show it.
        if !link.contains("://") {
            return Err(malformed());
        }
        let url = Url::parse(link).map_err(|error| match error {
            ParseError::InvalidPort => "链接里的端口无效。".to_string(),
            ParseError::EmptyHost => "链接里没有地址。".to_string(),
            _ => malformed(),
        })?;
        if url.scheme() != "ssh" {
            return Err(format!("只支持 ssh:// 链接，不支持 {}://。", url.scheme()));
        }
        // `ssh` is not one of the schemes the URL standard knows, so an IPv4
        // address and a domain both come back as an opaque domain, which is
        // percent-encoded when it is not ASCII.
        let address = match url.host() {
            Some(UrlHost::Domain(domain)) => decode(domain)?,
            Some(UrlHost::Ipv4(address)) => address.to_string(),
            Some(UrlHost::Ipv6(address)) => address.to_string(),
            None => String::new(),
        };
        if address.is_empty() {
            return Err("链接里没有地址。".into());
        }
        let port = match url.port() {
            Some(0) => return Err("链接里的端口无效。".into()),
            Some(port) => port,
            None => SSH_PORT,
        };
        let user = decode(url.username())?;
        if user.is_empty() {
            return Err("链接里没有用户名，应为 ssh://用户@地址[:端口]。".into());
        }
        let password = match url.password() {
            Some(password) => Some(Zeroizing::new(decode(password)?)),
            None => None,
        }
        .filter(|password| !password.is_empty());
        let name = tab
            .map(str::trim)
            .filter(|tab| !tab.is_empty())
            .map_or_else(|| address.clone(), str::to_string);
        Ok(Self {
            name,
            address,
            port,
            user,
            password,
        })
    }

    /// The host to connect to. It logs in by password: the link's, or else
    /// asked for when the server wants one.
    pub fn draft(&self) -> HostDraft {
        HostDraft::new(
            self.name.clone(),
            self.address.clone(),
            self.port,
            self.user.clone(),
            AuthKind::Password,
            None,
        )
    }
}

impl fmt::Debug for SshLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SshLink")
            .field("name", &self.name)
            .field("address", &self.address)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}

/// The text a link percent-encodes: `p%40ss` is `p@ss`.
fn decode(text: &str) -> Result<String, String> {
    percent_decode_str(text)
        .decode_utf8()
        .map(|text| text.into_owned())
        .map_err(|_| "链接里有无法识别的字符。".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(link: &str) -> SshLink {
        SshLink::parse(link, None).unwrap()
    }

    #[test]
    fn a_bastion_token_logs_in_as_the_user() {
        let link = parse("ssh://b478e26f-811b-4a90-81c3-74929127898a@172.16.0.28:12024");
        assert_eq!(link.user, "b478e26f-811b-4a90-81c3-74929127898a");
        assert_eq!(link.address, "172.16.0.28");
        assert_eq!(link.port, 12024);
        assert!(link.password.is_none());
        assert_eq!(link.name, "172.16.0.28");

        let draft = link.draft();
        assert_eq!(draft.address.as_ref(), "172.16.0.28");
        assert_eq!(draft.user.as_ref(), "b478e26f-811b-4a90-81c3-74929127898a");
        assert_eq!(draft.port, 12024);
        assert_eq!(draft.auth, AuthKind::Password);
        assert_eq!(draft.group, None);
    }

    #[test]
    fn the_tab_name_and_an_encoded_password_come_through() {
        let link = SshLink::parse(
            "SSH://deploy:p%40ss%3Aword@db.example.com",
            Some(" 生产库 "),
        )
        .unwrap();
        assert_eq!(link.name, "生产库");
        assert_eq!(link.user, "deploy");
        assert_eq!(link.address, "db.example.com");
        assert_eq!(link.port, 22);
        assert_eq!(
            link.password.as_deref().map(String::as_str),
            Some("p@ss:word")
        );
        // An empty password is none.
        assert!(parse("ssh://deploy:@db.example.com").password.is_none());
        // An empty tab name is no name.
        assert_eq!(
            SshLink::parse("ssh://deploy@db", Some("  ")).unwrap().name,
            "db"
        );
    }

    #[test]
    fn the_user_ends_at_the_last_at_sign() {
        // How JumpServer names the account and the asset in one user name.
        let link = parse("ssh://admin@root@10.1.1.1@jms.example.com:2222");
        assert_eq!(link.user, "admin@root@10.1.1.1");
        assert_eq!(link.address, "jms.example.com");
        assert_eq!(link.port, 2222);
    }

    #[test]
    fn an_ipv6_address_loses_its_brackets() {
        let link = parse("ssh://root@[::1]:2200/");
        assert_eq!(link.address, "::1");
        assert_eq!(link.port, 2200);
        assert_eq!(link.name, "::1");
    }

    #[test]
    fn a_non_ascii_domain_is_decoded() {
        assert_eq!(parse("ssh://root@测试.example").address, "测试.example");
    }

    #[test]
    fn what_is_wrong_is_said_without_repeating_the_link() {
        for (link, expected) in [
            ("", "没有要连接的地址"),
            ("   ", "没有要连接的地址"),
            ("sftp://root:secret@h", "只支持 ssh://"),
            ("ssh://h:22", "没有用户名"),
            ("ssh://:secret@h", "没有用户名"),
            ("ssh://root:secret@", "没有地址"),
            ("ssh://root:secret@h:0", "端口无效"),
            ("ssh://root:secret@h:65536", "端口无效"),
            ("ssh://root:secret@h:port", "端口无效"),
            ("root:secret@h", "格式不对"),
        ] {
            let error = SshLink::parse(link, None).unwrap_err();
            assert!(error.contains(expected), "{link:?}: {error}");
            assert!(!error.contains("secret"), "{link:?}: {error}");
        }
    }

    #[test]
    fn debug_never_shows_the_password() {
        let link = parse("ssh://root:hunter2@h");
        let shown = format!("{link:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("***"), "{shown}");
    }
}
