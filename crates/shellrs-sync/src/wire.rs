//! The records the app sees, typed, and their fields as written.
//!
//! Field names and spellings are the snapshot format: they never change
//! once released, and the spellings are the app's own (`no-password`,
//! `rts-cts`). Records are read with the rules the app's database checks,
//! so whatever the app is given it can store; a record that breaks one is
//! parked rather than given.

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::record::Map;
use crate::secret::{self, EndpointSecret, Secret, endpoint_secret_id};
use crate::{Kind, RecordKey, hex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordData {
    Group(GroupData),
    Credential(CredentialData),
    Host(HostData),
    Bookmark(BookmarkData),
    Forward(ForwardData),
    SnippetCategory(SnippetCategoryData),
    Snippet(SnippetData),
    /// An endpoint's sign-in password; its id is `endpoint_secret_id`.
    Password(EndpointSecret),
    /// A proxy's password; its id is `endpoint_secret_id` of the proxy.
    ProxyPassword(EndpointSecret),
    /// The password of the password credential with the record's id.
    CredentialPassword(Secret),
    /// The passphrase of the key credential with the record's id.
    Passphrase(Secret),
    /// The private key file, as text, of the key credential with the
    /// record's id.
    PrivateKey(Secret),
    /// The secret of the one-time codes of the host with the record's id.
    Totp(Secret),
}

/// A host group; `parent` is another group's id, `None` at the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupData {
    pub name: String,
    pub parent: Option<String>,
    pub sort_order: i64,
}

/// A credential's public part: its secret stays in each device's keychain,
/// and a key credential's file path is each device's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialData {
    pub name: String,
    pub kind: CredentialKind,
    pub username: String,
    pub sort_order: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    Password,
    Key,
    Agent,
}

/// A saved host. `username` is `None` exactly when `auth` is `Credential`
/// (the credential's is used), and `credential` is set exactly then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostData {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub group: Option<String>,
    pub sort_order: i64,
    pub protocol: Protocol,
    pub auth: Auth,
    pub username: Option<String>,
    pub credential: Option<String>,
    /// The user a host using a credential signs in as instead of the
    /// credential's; `None` for the credential's, and without a credential.
    /// It came after ShellRS 0.1.18: a copy an older version wrote has none,
    /// which reads as `None`, and one an older version turned to another
    /// sign-in may still have it, which is not used.
    pub credential_username: Option<String>,
    pub route: Route,
    pub notes: String,
    /// The one-time codes an SSH host fills in, `None` for none. It came
    /// after ShellRS 0.1.18: a copy an older version wrote has none, which
    /// reads as `None`, and an older version keeps it as it is.
    pub totp: Option<TotpData>,
}

/// What the question for a one-time code says; `None` for the usual.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TotpData {
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Protocol {
    Ssh,
    Telnet(TelnetData),
}

/// The prompts Telnet's automatic login waits for; `None` for the usual.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TelnetData {
    pub user_prompt: Option<String>,
    pub pass_prompt: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    Password,
    NoPassword,
    Credential,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Direct,
    /// The jump hosts in order. `None` is a hop whose host was deleted: the
    /// connection fails there rather than skip it.
    Jump(Vec<Option<String>>),
    Proxy(ProxyData),
    /// A host reached through an external command whose stdin and stdout
    /// carry the connection's bytes (OpenSSH's `ProxyCommand`).
    ProxyCommand(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyData {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Socks5,
}

/// A remote folder bookmarked in a host's SFTP tab. Its id is
/// `bookmark_id(host, path)`, so two devices bookmarking the same folder
/// make one record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkData {
    pub host: String,
    pub path: String,
    pub sort_order: i64,
}

/// A port forwarding rule, through `host`. A dynamic one has no target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardData {
    pub host: String,
    pub name: String,
    pub kind: ForwardKind,
    pub bind_host: String,
    pub bind_port: u16,
    pub target: Option<Endpoint>,
    pub sort_order: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardKind {
    Local,
    Remote,
    Dynamic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

/// A folder of snippets; `parent` is another category's id, `None` at the
/// top. `parent` and `sort_order` came after the format was first released:
/// a copy an older version wrote has neither, which reads as the top level
/// and 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetCategoryData {
    pub name: String,
    pub parent: Option<String>,
    pub sort_order: i64,
}

/// `sort_order` came after the format was first released: a copy an older
/// version wrote has none, which reads as 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetData {
    pub category: Option<String>,
    pub name: String,
    pub command: String,
    pub scope: SnippetScope,
    pub run_on_click: bool,
    pub sort_order: i64,
}

/// Which hosts a snippet is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnippetScope {
    All,
    Group(String),
    Host(String),
}

/// What the app can store; records it cannot are parked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub telnet: bool,
}

impl Capabilities {
    pub const ALL: Self = Self { telnet: true };
}

/// The id of the bookmark of `path` on `host`.
pub fn bookmark_id(host: &str, path: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(host.as_bytes());
    hash.update([0]);
    hash.update(path.as_bytes());
    hex::encode(&hash.finalize()[..16])
}

macro_rules! spelled {
    ($ty:ty { $($variant:ident = $text:literal),+ $(,)? }) => {
        impl $ty {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

spelled!(CredentialKind { Password = "password", Key = "key", Agent = "agent" });
spelled!(Auth { Password = "password", NoPassword = "no-password", Credential = "credential" });
spelled!(ProxyKind { Http = "http", Socks5 = "socks5" });
spelled!(ForwardKind { Local = "local", Remote = "remote", Dynamic = "dynamic" });

impl RecordData {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Group(_) => Kind::Group,
            Self::Credential(_) => Kind::Credential,
            Self::Host(_) => Kind::Host,
            Self::Bookmark(_) => Kind::Bookmark,
            Self::Forward(_) => Kind::Forward,
            Self::SnippetCategory(_) => Kind::SnippetCategory,
            Self::Snippet(_) => Kind::Snippet,
            Self::Password(_) => Kind::SecretPassword,
            Self::ProxyPassword(_) => Kind::SecretProxy,
            Self::CredentialPassword(_) => Kind::SecretCredential,
            Self::Passphrase(_) => Kind::SecretPassphrase,
            Self::PrivateKey(_) => Kind::SecretPrivateKey,
            Self::Totp(_) => Kind::SecretTotp,
        }
    }

    /// Whether `caps` can store this record.
    pub(crate) fn fits(&self, caps: &Capabilities) -> Result<(), String> {
        match self {
            Self::Host(HostData {
                protocol: Protocol::Telnet(_),
                ..
            }) if !caps.telnet => Err("Telnet hosts are not supported here".into()),
            _ => Ok(()),
        }
    }
}

/// Reads `data`, the fields of the record `key`, with the app database's
/// rules. An error says why the record is parked.
pub(crate) fn parse(key: &RecordKey, data: &Map) -> Result<RecordData, String> {
    let r = Reader(data);
    Ok(match key.kind {
        Kind::Group => RecordData::Group(GroupData {
            name: r.name("name")?,
            parent: r.id("parent")?,
            sort_order: r.order()?,
        }),
        Kind::Credential => RecordData::Credential(CredentialData {
            name: r.name("name")?,
            kind: r.spelled("kind", CredentialKind::parse)?,
            username: r.string("username")?,
            sort_order: r.order()?,
        }),
        Kind::Host => RecordData::Host(parse_host(key, &r)?),
        Kind::Bookmark => RecordData::Bookmark(BookmarkData {
            host: r.id("host")?.ok_or("a bookmark has no host")?,
            path: r.string("path")?,
            sort_order: r.order()?,
        }),
        Kind::Forward => RecordData::Forward(parse_forward(&r)?),
        Kind::SnippetCategory => RecordData::SnippetCategory(SnippetCategoryData {
            name: r.name("name")?,
            parent: r.later(|r| r.id("parent"), "parent", None)?,
            sort_order: r.later(Reader::order, "sort_order", 0)?,
        }),
        Kind::Snippet => RecordData::Snippet(SnippetData {
            category: r.id("category")?,
            name: r.name("name")?,
            command: r.name("command")?,
            scope: match r.string("scope")?.as_str() {
                "all" => {
                    if r.id("scope_id")?.is_some() {
                        return Err("an all-hosts snippet has a scope".into());
                    }
                    SnippetScope::All
                }
                "group" => SnippetScope::Group(r.id("scope_id")?.ok_or("no scope group")?),
                "host" => SnippetScope::Host(r.id("scope_id")?.ok_or("no scope host")?),
                other => return Err(format!("unknown snippet scope {other}")),
            },
            run_on_click: r.flag("run_on_click")?,
            sort_order: r.later(Reader::order, "sort_order", 0)?,
        }),
        Kind::SecretPassword => RecordData::Password(parse_endpoint(key, &r)?),
        Kind::SecretProxy => RecordData::ProxyPassword(parse_endpoint(key, &r)?),
        Kind::SecretCredential => RecordData::CredentialPassword(r.secret()?),
        Kind::SecretPassphrase => RecordData::Passphrase(r.secret()?),
        Kind::SecretPrivateKey => RecordData::PrivateKey(r.secret()?),
        Kind::SecretTotp => RecordData::Totp(r.secret()?),
        Kind::Secret(_) | Kind::Other(_) => return Err(format!("unknown kind {}", key.kind)),
    })
}

/// A password for an endpoint, filed under that endpoint's id.
fn parse_endpoint(key: &RecordKey, r: &Reader) -> Result<EndpointSecret, String> {
    let secret = EndpointSecret {
        user: r.string("user")?,
        host: r.name("host")?,
        port: r.port("port")?,
        value: r.secret()?,
    };
    if key.id != endpoint_secret_id(&secret.user, &secret.host, secret.port) {
        return Err("the id is not its endpoint's".into());
    }
    Ok(secret)
}

fn parse_host(key: &RecordKey, r: &Reader) -> Result<HostData, String> {
    let protocol = match r.string("protocol")?.as_str() {
        "ssh" => Protocol::Ssh,
        "telnet" => {
            let telnet = r.object("telnet")?;
            let telnet = Reader(&telnet);
            Protocol::Telnet(TelnetData {
                user_prompt: telnet.prompt("user_prompt")?,
                pass_prompt: telnet.prompt("pass_prompt")?,
            })
        }
        other => return Err(format!("unknown protocol {other}")),
    };
    let auth = r.spelled("auth", Auth::parse)?;
    let username = r.optional_string("username")?;
    let credential = r.id("credential")?;
    if (auth == Auth::Credential) != credential.is_some() {
        return Err("a credential goes with credential sign-in".into());
    }
    if (auth == Auth::Credential) != username.is_none() {
        return Err("a user name goes with sign-in of the host's own".into());
    }
    let credential_username = r
        .later(
            |r| r.optional_string("credential_username"),
            "credential_username",
            None,
        )?
        .filter(|user| auth == Auth::Credential && !user.is_empty());
    let totp = r
        .later(
            |r| match r.0.get("totp") {
                Some(Value::Null) => Ok(None),
                _ => {
                    let totp = r.object("totp")?;
                    Ok(Some(TotpData {
                        prompt: Reader(&totp).prompt("prompt")?,
                    }))
                }
            },
            "totp",
            None,
        )?
        .filter(|_| protocol == Protocol::Ssh);
    let route = match r.string("route")?.as_str() {
        "direct" => Route::Direct,
        "jump" => {
            let Some(Value::Array(hops)) = r.0.get("jumps") else {
                return Err("a jump route has no jumps".into());
            };
            let hops = hops
                .iter()
                .map(|hop| match hop {
                    Value::Null => Ok(None),
                    Value::String(id) if RecordKey::is_valid_id(id) && *id != key.id => {
                        Ok(Some(id.clone()))
                    }
                    _ => Err("a jump is not another host".to_owned()),
                })
                .collect::<Result<_, _>>()?;
            Route::Jump(hops)
        }
        "proxy" => {
            let proxy = r.object("proxy")?;
            let proxy = Reader(&proxy);
            Route::Proxy(ProxyData {
                kind: proxy.spelled("kind", ProxyKind::parse)?,
                host: proxy.string("host")?,
                port: proxy.port("port")?,
                username: proxy.optional_string("username")?,
            })
        }
        "proxy-command" => Route::ProxyCommand(r.string("command")?),
        other => return Err(format!("unknown route {other}")),
    };
    Ok(HostData {
        name: r.name("name")?,
        address: r.name("address")?,
        port: r.port("port")?,
        group: r.id("group")?,
        sort_order: r.order()?,
        protocol,
        auth,
        username,
        credential,
        credential_username,
        route,
        notes: r.string("notes")?,
        totp,
    })
}

fn parse_forward(r: &Reader) -> Result<ForwardData, String> {
    let kind = r.spelled("kind", ForwardKind::parse)?;
    let target_host = r.optional_string("target_host")?;
    let target_port = match r.0.get("target_port") {
        Some(Value::Null) => None,
        _ => Some(r.port("target_port")?),
    };
    let target = match (kind, target_host, target_port) {
        (ForwardKind::Dynamic, None, None) => None,
        (ForwardKind::Local | ForwardKind::Remote, Some(host), Some(port)) => {
            Some(Endpoint { host, port })
        }
        _ => return Err("a dynamic forward has no target, the others one".into()),
    };
    Ok(ForwardData {
        host: r.id("host")?.ok_or("a forward has no host")?,
        name: r.string("name")?,
        kind,
        bind_host: r.string("bind_host")?,
        bind_port: r.port("bind_port")?,
        target,
        sort_order: r.order()?,
    })
}

/// The fields of `data`, written over `base` (the record's fields as they
/// were): fields this version does not know stay where they were, known
/// ones are replaced, and known ones that no longer apply (a Telnet host's
/// prompts once it is SSH) are dropped.
pub(crate) fn write_into(data: &RecordData, base: Option<&Map>) -> Map {
    let fresh = fields(data);
    let known = known_fields(&data.kind());
    let mut out = Map::new();
    if let Some(base) = base {
        for (key, value) in base {
            if !known.contains(&key.as_str()) {
                out.insert(key.clone(), value.clone());
            } else if let Some((_, new)) = fresh.iter().find(|(name, _)| name == key) {
                out.insert(key.clone(), new.clone());
            }
        }
    }
    for (name, value) in fresh {
        if !out.contains_key(name) {
            out.insert(name.to_owned(), value);
        }
    }
    out
}

fn known_fields(kind: &Kind) -> &'static [&'static str] {
    match kind {
        Kind::Group => &["name", "parent", "sort_order"],
        Kind::Credential => &["name", "kind", "username", "sort_order"],
        Kind::Host => &[
            "name",
            "address",
            "port",
            "group",
            "sort_order",
            "protocol",
            "telnet",
            "auth",
            "username",
            "credential",
            "credential_username",
            "route",
            "jumps",
            "proxy",
            "notes",
            "totp",
        ],
        Kind::Bookmark => &["host", "path", "sort_order"],
        Kind::Forward => &[
            "host",
            "name",
            "kind",
            "bind_host",
            "bind_port",
            "target_host",
            "target_port",
            "sort_order",
        ],
        Kind::SnippetCategory => &["name", "parent", "sort_order"],
        Kind::Snippet => &[
            "category",
            "name",
            "command",
            "scope",
            "scope_id",
            "run_on_click",
            "sort_order",
        ],
        Kind::SecretPassword | Kind::SecretProxy => &["user", "host", "port", "value"],
        Kind::SecretCredential
        | Kind::SecretPassphrase
        | Kind::SecretPrivateKey
        | Kind::SecretTotp => &["value"],
        Kind::Secret(_) | Kind::Other(_) => &[],
    }
}

fn fields(data: &RecordData) -> Vec<(&'static str, Value)> {
    match data {
        RecordData::Group(group) => vec![
            ("name", json!(group.name)),
            ("parent", json!(group.parent)),
            ("sort_order", json!(group.sort_order)),
        ],
        RecordData::Credential(credential) => vec![
            ("name", json!(credential.name)),
            ("kind", json!(credential.kind.as_str())),
            ("username", json!(credential.username)),
            ("sort_order", json!(credential.sort_order)),
        ],
        RecordData::Host(host) => {
            let mut out = vec![
                ("name", json!(host.name)),
                ("address", json!(host.address)),
                ("port", json!(host.port)),
                ("group", json!(host.group)),
                ("sort_order", json!(host.sort_order)),
            ];
            match &host.protocol {
                Protocol::Ssh => out.push(("protocol", json!("ssh"))),
                Protocol::Telnet(telnet) => {
                    out.push(("protocol", json!("telnet")));
                    out.push((
                        "telnet",
                        json!({"user_prompt": telnet.user_prompt, "pass_prompt": telnet.pass_prompt}),
                    ));
                }
            }
            out.push(("auth", json!(host.auth.as_str())));
            out.push(("username", json!(host.username)));
            out.push(("credential", json!(host.credential)));
            out.push(("credential_username", json!(host.credential_username)));
            match &host.route {
                Route::Direct => out.push(("route", json!("direct"))),
                Route::Jump(hops) => {
                    out.push(("route", json!("jump")));
                    out.push(("jumps", json!(hops)));
                }
                Route::Proxy(proxy) => {
                    out.push(("route", json!("proxy")));
                    out.push((
                        "proxy",
                        json!({
                            "kind": proxy.kind.as_str(),
                            "host": proxy.host,
                            "port": proxy.port,
                            "username": proxy.username,
                        }),
                    ));
                }
                Route::ProxyCommand(command) => {
                    out.push(("route", json!("proxy-command")));
                    out.push(("command", json!(command)));
                }
            }
            out.push(("notes", json!(host.notes)));
            out.push((
                "totp",
                match &host.totp {
                    Some(totp) => json!({"prompt": totp.prompt}),
                    None => Value::Null,
                },
            ));
            out
        }
        RecordData::Bookmark(bookmark) => vec![
            ("host", json!(bookmark.host)),
            ("path", json!(bookmark.path)),
            ("sort_order", json!(bookmark.sort_order)),
        ],
        RecordData::Forward(forward) => vec![
            ("host", json!(forward.host)),
            ("name", json!(forward.name)),
            ("kind", json!(forward.kind.as_str())),
            ("bind_host", json!(forward.bind_host)),
            ("bind_port", json!(forward.bind_port)),
            (
                "target_host",
                json!(forward.target.as_ref().map(|target| &target.host)),
            ),
            (
                "target_port",
                json!(forward.target.as_ref().map(|target| target.port)),
            ),
            ("sort_order", json!(forward.sort_order)),
        ],
        RecordData::SnippetCategory(category) => vec![
            ("name", json!(category.name)),
            ("parent", json!(category.parent)),
            ("sort_order", json!(category.sort_order)),
        ],
        RecordData::Snippet(snippet) => {
            let (scope, scope_id) = match &snippet.scope {
                SnippetScope::All => ("all", None),
                SnippetScope::Group(id) => ("group", Some(id)),
                SnippetScope::Host(id) => ("host", Some(id)),
            };
            vec![
                ("category", json!(snippet.category)),
                ("name", json!(snippet.name)),
                ("command", json!(snippet.command)),
                ("scope", json!(scope)),
                ("scope_id", json!(scope_id)),
                ("run_on_click", json!(snippet.run_on_click)),
                ("sort_order", json!(snippet.sort_order)),
            ]
        }
        RecordData::Password(endpoint) | RecordData::ProxyPassword(endpoint) => vec![
            ("user", json!(endpoint.user)),
            ("host", json!(endpoint.host)),
            ("port", json!(endpoint.port)),
            ("value", json!(endpoint.value.expose())),
        ],
        RecordData::CredentialPassword(value)
        | RecordData::Passphrase(value)
        | RecordData::PrivateKey(value)
        | RecordData::Totp(value) => vec![("value", json!(value.expose()))],
    }
}

/// Field readers that say what is wrong.
struct Reader<'a>(&'a Map);

impl Reader<'_> {
    fn string(&self, key: &str) -> Result<String, String> {
        match self.0.get(key) {
            Some(Value::String(text)) => Ok(text.clone()),
            _ => Err(format!("{key} is not text")),
        }
    }

    /// Text, or `null` for none.
    fn optional_string(&self, key: &str) -> Result<Option<String>, String> {
        match self.0.get(key) {
            Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            _ => Err(format!("{key} is not text")),
        }
    }

    /// Text that may not be empty.
    fn name(&self, key: &str) -> Result<String, String> {
        let text = self.string(key)?;
        if text.is_empty() {
            return Err(format!("{key} is empty"));
        }
        Ok(text)
    }

    /// A prompt: `null`, or text that is not empty.
    fn prompt(&self, key: &str) -> Result<Option<String>, String> {
        match self.optional_string(key)? {
            Some(text) if text.is_empty() => Err(format!("{key} is empty")),
            prompt => Ok(prompt),
        }
    }

    /// Another record's id, or `None`.
    fn id(&self, key: &str) -> Result<Option<String>, String> {
        match self.optional_string(key)? {
            Some(id) if !RecordKey::is_valid_id(&id) => Err(format!("{key} is not an id")),
            id => Ok(id),
        }
    }

    fn number(&self, key: &str, low: u64, high: u64) -> Result<u64, String> {
        self.0
            .get(key)
            .and_then(Value::as_u64)
            .filter(|n| (low..=high).contains(n))
            .ok_or_else(|| format!("{key} is not a number from {low} to {high}"))
    }

    fn port(&self, key: &str) -> Result<u16, String> {
        Ok(self.number(key, 1, 65535)? as u16)
    }

    /// A position among siblings; `0` when missing.
    fn order(&self) -> Result<i64, String> {
        self.0
            .get("sort_order")
            .and_then(Value::as_i64)
            .ok_or_else(|| "sort_order is not a number".into())
    }

    /// A field added after the format was first released, read with
    /// `read`: a copy an older version wrote has none, which is `missing`.
    /// One that is there must be right, as every other field must.
    fn later<T>(
        &self,
        read: impl Fn(&Self) -> Result<T, String>,
        key: &str,
        missing: T,
    ) -> Result<T, String> {
        if self.0.contains_key(key) {
            read(self)
        } else {
            Ok(missing)
        }
    }

    fn flag(&self, key: &str) -> Result<bool, String> {
        match self.0.get(key) {
            Some(Value::Bool(flag)) => Ok(*flag),
            _ => Err(format!("{key} is not true or false")),
        }
    }

    /// A secret's `value`: not empty, and no longer than a key file.
    fn secret(&self) -> Result<Secret, String> {
        let value = self.name("value")?;
        if value.len() > secret::MAX_VALUE {
            return Err("value is too long".into());
        }
        Ok(Secret::new(value))
    }

    fn spelled<T>(&self, key: &str, parse: fn(&str) -> Option<T>) -> Result<T, String> {
        let text = self.string(key)?;
        parse(&text).ok_or_else(|| format!("unknown {key} {text}"))
    }

    fn object(&self, key: &str) -> Result<Map, String> {
        match self.0.get(key) {
            Some(Value::Object(fields)) => Ok(fields.clone()),
            _ => Err(format!("{key} is missing")),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn ssh_host(name: &str) -> HostData {
        HostData {
            name: name.into(),
            address: "10.0.0.1".into(),
            port: 22,
            group: None,
            sort_order: 0,
            protocol: Protocol::Ssh,
            auth: Auth::Password,
            username: Some("root".into()),
            credential: None,
            credential_username: None,
            route: Route::Direct,
            notes: String::new(),
            totp: None,
        }
    }

    pub(crate) fn samples() -> Vec<(RecordKey, RecordData)> {
        let key = |kind, id: &str| RecordKey::new(kind, id);
        vec![
            (
                key(Kind::Group, "g1"),
                RecordData::Group(GroupData {
                    name: "Production".into(),
                    parent: Some("g0".into()),
                    sort_order: 3,
                }),
            ),
            (
                key(Kind::Credential, "c1"),
                RecordData::Credential(CredentialData {
                    name: "deploy key".into(),
                    kind: CredentialKind::Key,
                    username: "deploy".into(),
                    sort_order: 1,
                }),
            ),
            (key(Kind::Host, "h1"), RecordData::Host(ssh_host("web"))),
            (
                key(Kind::Host, "h2"),
                RecordData::Host(HostData {
                    protocol: Protocol::Telnet(TelnetData {
                        user_prompt: Some("login:".into()),
                        pass_prompt: None,
                    }),
                    auth: Auth::Credential,
                    username: None,
                    credential: Some("c1".into()),
                    route: Route::Jump(vec![Some("h1".into()), None]),
                    notes: "behind the bastion".into(),
                    ..ssh_host("switch")
                }),
            ),
            (
                key(Kind::Host, "h3"),
                RecordData::Host(HostData {
                    address: "10.0.0.3".into(),
                    route: Route::Proxy(ProxyData {
                        kind: ProxyKind::Socks5,
                        host: "proxy".into(),
                        port: 1080,
                        username: Some("me".into()),
                    }),
                    ..ssh_host("console")
                }),
            ),
            (
                key(Kind::Bookmark, &bookmark_id("h1", "/var/log")),
                RecordData::Bookmark(BookmarkData {
                    host: "h1".into(),
                    path: "/var/log".into(),
                    sort_order: 0,
                }),
            ),
            (
                key(Kind::Forward, "f1"),
                RecordData::Forward(ForwardData {
                    host: "h1".into(),
                    name: "db".into(),
                    kind: ForwardKind::Local,
                    bind_host: "127.0.0.1".into(),
                    bind_port: 5432,
                    target: Some(Endpoint {
                        host: "db.internal".into(),
                        port: 5432,
                    }),
                    sort_order: 0,
                }),
            ),
            (
                key(Kind::Forward, "f2"),
                RecordData::Forward(ForwardData {
                    host: "h1".into(),
                    name: String::new(),
                    kind: ForwardKind::Dynamic,
                    bind_host: "127.0.0.1".into(),
                    bind_port: 1080,
                    target: None,
                    sort_order: 1,
                }),
            ),
            (
                key(Kind::SnippetCategory, "s0"),
                // As the files of format 1 that `golden` keeps have them:
                // from before the fields added since.
                RecordData::SnippetCategory(SnippetCategoryData {
                    name: "Ops".into(),
                    parent: None,
                    sort_order: 0,
                }),
            ),
            (
                key(Kind::Snippet, "s1"),
                RecordData::Snippet(SnippetData {
                    category: Some("s0".into()),
                    name: "disk".into(),
                    command: "df -h".into(),
                    scope: SnippetScope::Host("h1".into()),
                    run_on_click: true,
                    sort_order: 0,
                }),
            ),
            (
                key(Kind::Snippet, "s2"),
                RecordData::Snippet(SnippetData {
                    category: None,
                    name: "uptime".into(),
                    command: "uptime".into(),
                    scope: SnippetScope::All,
                    run_on_click: false,
                    sort_order: 0,
                }),
            ),
            (
                key(
                    Kind::SecretPassword,
                    &endpoint_secret_id("root", "10.0.0.1", 22),
                ),
                RecordData::Password(EndpointSecret {
                    user: "root".into(),
                    host: "10.0.0.1".into(),
                    port: 22,
                    value: Secret::new("test password"),
                }),
            ),
            (
                key(Kind::SecretProxy, &endpoint_secret_id("me", "proxy", 1080)),
                RecordData::ProxyPassword(EndpointSecret {
                    user: "me".into(),
                    host: "proxy".into(),
                    port: 1080,
                    value: Secret::new("test proxy password"),
                }),
            ),
            (
                key(Kind::SecretCredential, "c2"),
                RecordData::CredentialPassword(Secret::new("test credential password")),
            ),
            (
                key(Kind::SecretPassphrase, "c1"),
                RecordData::Passphrase(Secret::new("test passphrase")),
            ),
            (
                key(Kind::SecretPrivateKey, "c1"),
                RecordData::PrivateKey(Secret::new(
                    "-----BEGIN OPENSSH PRIVATE KEY-----\ntest key\n-----END OPENSSH PRIVATE KEY-----\n",
                )),
            ),
        ]
    }

    #[test]
    fn secrets_are_refused_unless_whole() {
        let password = |user: &str, value: &str| {
            let mut fields = Map::new();
            fields.insert("user".into(), json!(user));
            fields.insert("host".into(), json!("10.0.0.1"));
            fields.insert("port".into(), json!(22));
            fields.insert("value".into(), json!(value));
            fields
        };
        let id = endpoint_secret_id("root", "10.0.0.1", 22);
        let key = RecordKey::new(Kind::SecretPassword, id.as_str());
        assert!(parse(&key, &password("root", "x")).is_ok());
        assert!(parse(&key, &password("root", "")).is_err());
        assert!(
            parse(&key, &password("admin", "x")).is_err(),
            "another endpoint's id"
        );
        let long = "k".repeat(secret::MAX_VALUE + 1);
        let mut fields = Map::new();
        fields.insert("value".into(), json!(long));
        assert!(parse(&RecordKey::new(Kind::SecretPrivateKey, "c1"), &fields).is_err());
        // Telnet sign-in may have no user name.
        let id = endpoint_secret_id("", "10.0.0.1", 22);
        assert!(
            parse(
                &RecordKey::new(Kind::SecretPassword, id),
                &password("", "x")
            )
            .is_ok()
        );
    }

    #[test]
    fn a_record_holding_a_secret_does_not_show_it() {
        for (_, data) in samples() {
            assert!(!format!("{data:?}").contains("test "), "{data:?}");
        }
    }

    #[test]
    fn every_kind_reads_back_what_it_wrote() {
        for (key, data) in samples() {
            let written = write_into(&data, None);
            assert_eq!(parse(&key, &written), Ok(data), "{key}");
        }
    }

    #[test]
    fn the_written_fields_are_the_format() {
        let written = write_into(&RecordData::Host(ssh_host("web")), None);
        assert_eq!(
            Value::Object(written),
            json!({
                "name": "web", "address": "10.0.0.1", "port": 22, "group": null,
                "sort_order": 0, "protocol": "ssh", "auth": "password",
                "username": "root", "credential": null, "credential_username": null,
                "route": "direct", "notes": "", "totp": null
            })
        );
    }

    #[test]
    fn fields_this_version_does_not_know_stay_in_place() {
        let base =
            json!({"name": "old", "color": "red", "address": "a", "telnet": {"user_prompt": null}});
        let Value::Object(base) = base else {
            unreachable!()
        };
        let written = write_into(&RecordData::Host(ssh_host("web")), Some(&base));
        let keys: Vec<_> = written.keys().map(String::as_str).collect();
        assert_eq!(&keys[..3], ["name", "color", "address"]);
        assert_eq!(written["name"], json!("web"));
        assert_eq!(written["color"], json!("red"));
        assert!(!written.contains_key("telnet"));
    }

    #[test]
    fn records_the_app_could_not_store_are_refused() {
        let host = |change: fn(&mut Map)| {
            let mut fields = write_into(&RecordData::Host(ssh_host("web")), None);
            change(&mut fields);
            parse(&RecordKey::new(Kind::Host, "h1"), &fields)
        };
        assert!(host(|f| drop(f.insert("port".into(), json!(0)))).is_err());
        assert!(host(|f| drop(f.insert("port".into(), json!(70000)))).is_err());
        assert!(host(|f| drop(f.insert("name".into(), json!("")))).is_err());
        assert!(host(|f| drop(f.insert("protocol".into(), json!("mosh")))).is_err());
        assert!(host(|f| drop(f.insert("auth".into(), json!("credential")))).is_err());
        assert!(host(|f| drop(f.insert("credential".into(), json!("c1")))).is_err());
        assert!(
            host(|f| {
                f.insert("route".into(), json!("jump"));
                f.insert("jumps".into(), json!(["h1"]));
            })
            .is_err()
        );
        assert!(host(|f| drop(f.insert("group".into(), json!("not an id!")))).is_err());
        // Every field is written, so one missing is a damaged record.
        assert!(host(|f| drop(f.remove("sort_order"))).is_err());
        assert!(host(|f| drop(f.remove("notes"))).is_err());

        let forward = json!({"host": "h1", "name": "", "kind": "dynamic", "bind_host": "l",
            "bind_port": 1, "target_host": "x", "target_port": 1});
        let Value::Object(forward) = forward else {
            unreachable!()
        };
        assert!(parse(&RecordKey::new(Kind::Forward, "f"), &forward).is_err());
        assert!(parse(&RecordKey::new(Kind::Other("tag".into()), "t"), &Map::new()).is_err());
    }

    #[test]
    fn fields_added_since_the_first_release_read_as_before_when_missing() {
        let category = RecordKey::new(Kind::SnippetCategory, "s9");
        let nested = RecordData::SnippetCategory(SnippetCategoryData {
            name: "logs".into(),
            parent: Some("s0".into()),
            sort_order: 3,
        });
        let written = write_into(&nested, None);
        assert_eq!(parse(&category, &written), Ok(nested));

        // A copy an older version wrote: at the top, in the first place.
        let Value::Object(old) = json!({"name": "logs"}) else {
            unreachable!()
        };
        assert_eq!(
            parse(&category, &old),
            Ok(RecordData::SnippetCategory(SnippetCategoryData {
                name: "logs".into(),
                parent: None,
                sort_order: 0,
            }))
        );
        let snippet = RecordKey::new(Kind::Snippet, "s1");
        let Value::Object(old) = json!({"category": null, "name": "df", "command": "df",
            "scope": "all", "scope_id": null, "run_on_click": false})
        else {
            unreachable!()
        };
        let Ok(RecordData::Snippet(read)) = parse(&snippet, &old) else {
            panic!("an older copy is damaged");
        };
        assert_eq!(read.sort_order, 0);

        // There, they must be right like any other field.
        let mut wrong = written.clone();
        wrong.insert("parent".into(), json!(7));
        assert!(parse(&category, &wrong).is_err());
        let mut wrong = written;
        wrong.insert("sort_order".into(), json!("first"));
        assert!(parse(&category, &wrong).is_err());
    }

    #[test]
    fn a_host_signs_in_with_its_credential_as_another_user() {
        let key = RecordKey::new(Kind::Host, "h9");
        let using = HostData {
            auth: Auth::Credential,
            username: None,
            credential: Some("c1".into()),
            credential_username: Some("deploy".into()),
            ..ssh_host("web")
        };
        let written = write_into(&RecordData::Host(using.clone()), None);
        assert_eq!(written["credential_username"], json!("deploy"));
        assert_eq!(parse(&key, &written), Ok(RecordData::Host(using.clone())));

        // A copy an older version wrote: the credential's user.
        let mut old = written.clone();
        old.remove("credential_username");
        let as_credential = HostData {
            credential_username: None,
            ..using.clone()
        };
        assert_eq!(
            parse(&key, &old),
            Ok(RecordData::Host(as_credential.clone()))
        );
        let mut empty = written.clone();
        empty.insert("credential_username".into(), json!(""));
        assert_eq!(parse(&key, &empty), Ok(RecordData::Host(as_credential)));

        // An older version that turned it to its own password kept the
        // field, which then means nothing.
        let mut own = write_into(&RecordData::Host(ssh_host("web")), None);
        own.insert("credential_username".into(), json!("deploy"));
        assert_eq!(parse(&key, &own), Ok(RecordData::Host(ssh_host("web"))));

        let mut wrong = written;
        wrong.insert("credential_username".into(), json!(7));
        assert!(parse(&key, &wrong).is_err());
    }

    #[test]
    fn a_host_fills_in_one_time_codes_and_keeps_their_secret_apart() {
        let key = RecordKey::new(Kind::Host, "h9");
        let coded = HostData {
            totp: Some(TotpData {
                prompt: Some("OTP verification code".into()),
            }),
            ..ssh_host("web")
        };
        let written = write_into(&RecordData::Host(coded.clone()), None);
        assert_eq!(written["totp"], json!({"prompt": "OTP verification code"}));
        assert_eq!(parse(&key, &written), Ok(RecordData::Host(coded.clone())));
        let usual = HostData {
            totp: Some(TotpData::default()),
            ..ssh_host("web")
        };
        let written_usual = write_into(&RecordData::Host(usual.clone()), None);
        assert_eq!(written_usual["totp"], json!({"prompt": null}));
        assert_eq!(parse(&key, &written_usual), Ok(RecordData::Host(usual)));

        // A copy an older version wrote fills in none.
        let mut old = written.clone();
        old.remove("totp");
        assert_eq!(parse(&key, &old), Ok(RecordData::Host(ssh_host("web"))));

        // Only an SSH host asks for codes.
        let telnet = samples()
            .into_iter()
            .find(|(key, _)| key.id == "h2")
            .unwrap()
            .1;
        let mut telnet = write_into(&telnet, None);
        telnet.insert("totp".into(), json!({"prompt": null}));
        let Ok(RecordData::Host(telnet)) = parse(&key, &telnet) else {
            panic!("a Telnet host with codes is damaged");
        };
        assert_eq!(telnet.totp, None);

        for wrong in [
            json!(true),
            json!({}),
            json!({"prompt": ""}),
            json!({"prompt": 7}),
        ] {
            let mut fields = written.clone();
            fields.insert("totp".into(), wrong.clone());
            assert!(parse(&key, &fields).is_err(), "{wrong}");
        }

        // The secret is a record of its own, under the host's id.
        let secret = RecordKey::new(Kind::SecretTotp, "h9");
        let mut fields = Map::new();
        fields.insert("value".into(), json!("JBSWY3DPEHPK3PXP"));
        let Ok(RecordData::Totp(value)) = parse(&secret, &fields) else {
            panic!("the secret does not read");
        };
        assert_eq!(value.expose(), "JBSWY3DPEHPK3PXP");
        assert!(!format!("{value:?}").contains("JBSWY3DP"));
        assert_eq!(Kind::parse("secret.totp"), Some(Kind::SecretTotp));
        assert!(Kind::SecretTotp.is_secret());
    }

    #[test]
    fn hosts_some_devices_cannot_use_do_not_fit_them() {
        let telnet = samples()
            .into_iter()
            .find(|(key, _)| key.id == "h2")
            .unwrap()
            .1;
        assert!(telnet.fits(&Capabilities::ALL).is_ok());
        assert!(telnet.fits(&Capabilities { telnet: false }).is_err());
    }

    #[test]
    fn a_bookmark_id_names_the_folder_on_the_host() {
        assert_eq!(bookmark_id("h1", "/var"), bookmark_id("h1", "/var"));
        assert_ne!(bookmark_id("h1", "/var"), bookmark_id("h2", "/var"));
        assert_ne!(bookmark_id("h", "1/var"), bookmark_id("h1", "/var"));
        assert!(RecordKey::is_valid_id(&bookmark_id("h1", "/var")));
    }
}
