//! Hosts and credentials as the CLI sees them: the details it prints, and
//! the changes it asks for, checked and resolved against the store before
//! the workspace makes them. Everything here reads the store on the UI
//! thread; nothing here changes it.

use std::path::Path;

use crate::host::{
    AuthKind, Credential, CredentialDraft, CredentialId, CredentialKind, GroupId, Host, HostDraft,
    HostId, HostStore, PastedKey, ProxyKind, ProxySettings, Route,
};
use crate::secrets::SecretRef;

use super::protocol::{
    AuthChoice, CliError, CredentialDetails, CredentialFields, CredentialKindChoice, ErrorCode,
    HostDetails, HostFields, HostInfo, ProxyChoice, RouteDetails, RouteFields, Secret,
};

/// A change a CLI request asks for, made on the UI thread one at a time.
/// Hosts and credentials are named by the IDs the CLI prints.
#[derive(Debug)]
pub enum CliChange {
    CreateHost(HostFields),
    UpdateHost {
        host: String,
        fields: HostFields,
    },
    DeleteHost {
        host: String,
        force: bool,
    },
    CreateCredential(CredentialFields),
    UpdateCredential {
        credential: String,
        fields: CredentialFields,
    },
    DeleteCredential {
        credential: String,
    },
}

/// The keychain entries a host's details say are saved or not: its own
/// password, and its proxy's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostSecrets {
    pub password: Option<SecretRef>,
    pub proxy: Option<SecretRef>,
}

/// The keychain entries a credential's details say are saved or not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialSecrets {
    pub password: Option<SecretRef>,
    pub passphrase: Option<SecretRef>,
}

/// A host as `shellrs hosts list` lists it.
pub fn host_info(host: &Host, store: &HostStore) -> HostInfo {
    HostInfo {
        id: host.public_id.to_string(),
        name: host.name.to_string(),
        group: group_path(host.group, store),
        user: host.user.to_string(),
        host: host.address.to_string(),
        port: host.port,
        os: host.os.map(|os| os.as_str().to_string()),
        temporary: store.is_temporary(host.id),
    }
}

/// A group's path as the CLI writes it, `生产/数据库`; `None` at the root.
fn group_path(group: Option<GroupId>, store: &HostStore) -> Option<String> {
    let names = group.map(|id| store.group_names(id)).unwrap_or_default();
    (!names.is_empty()).then(|| names.join("/"))
}

/// `host` as it is in `store`, without saying which passwords are saved:
/// that takes the keychain, which [`host_secrets`] names.
pub fn host_details(host: &Host, store: &HostStore) -> HostDetails {
    let credential = host
        .credential
        .and_then(|id| store.credential(id))
        .map(|credential| credential.keychain_id.to_string());
    let auth = match (&credential, host.auth) {
        (Some(_), _) => AuthChoice::Credential,
        (None, AuthKind::Password) => AuthChoice::Password,
        (None, AuthKind::NoPassword) => AuthChoice::NoPassword,
    };
    let route = match &host.route {
        Route::Direct => RouteDetails::Direct,
        Route::Jump(hops) => RouteDetails::Jump {
            hosts: hops
                .iter()
                .map(|hop| {
                    hop.and_then(|id| store.saved_host(id))
                        .map(|hop| hop.public_id.to_string())
                })
                .collect(),
        },
        Route::Proxy(proxy) => RouteDetails::Proxy {
            kind: match proxy.kind {
                ProxyKind::Http => ProxyChoice::Http,
                ProxyKind::Socks5 => ProxyChoice::Socks5,
            },
            host: proxy.host.to_string(),
            port: proxy.port,
            user: proxy.user.as_ref().map(ToString::to_string),
            password_saved: None,
        },
    };
    HostDetails {
        id: host.public_id.to_string(),
        name: host.name.to_string(),
        group: group_path(host.group, store),
        host: host.address.to_string(),
        port: host.port,
        user: host.user.to_string(),
        auth,
        credential,
        password_saved: None,
        route,
        notes: host.notes.to_string(),
        os: host.os.map(|os| os.as_str().to_string()),
        temporary: store.is_temporary(host.id),
    }
}

/// The entries behind a host's `password_saved` fields: its own password
/// when it logs in with one (a temporary host's is in memory), and its
/// proxy's when the proxy has a user.
pub fn host_secrets(host: &Host, store: &HostStore) -> HostSecrets {
    let login = store.login_of(host);
    HostSecrets {
        password: (host.credential.is_none() && host.auth == AuthKind::Password)
            .then_some(login.password),
        proxy: login.route.proxy_password(),
    }
}

/// `details` saying which of `secrets` are saved, as `saved` finds them.
pub fn with_saved_passwords(
    mut details: HostDetails,
    secrets: &HostSecrets,
    saved: impl Fn(&SecretRef) -> bool,
) -> HostDetails {
    details.password_saved = secrets.password.as_ref().map(&saved);
    if let RouteDetails::Proxy { password_saved, .. } = &mut details.route {
        *password_saved = secrets.proxy.as_ref().map(&saved);
    }
    details
}

pub fn credential_details(credential: &Credential, store: &HostStore) -> CredentialDetails {
    let kept = credential.keeps_key_in(store.key_dir());
    CredentialDetails {
        id: credential.keychain_id.to_string(),
        name: credential.name.to_string(),
        kind: match credential.kind {
            CredentialKind::Password => CredentialKindChoice::Password,
            CredentialKind::Key => CredentialKindChoice::Key,
            CredentialKind::Agent => CredentialKindChoice::Agent,
        },
        user: credential.user.to_string(),
        // Where a key ShellRS keeps lies is as good as the key itself.
        key_path: credential
            .key_path
            .as_ref()
            .filter(|_| !kept)
            .map(ToString::to_string),
        kept,
        hosts: store
            .hosts_using(credential.id)
            .map(|host| host.public_id.to_string())
            .collect(),
        password_saved: None,
        passphrase_saved: None,
    }
}

pub fn credential_secrets(credential: &Credential) -> CredentialSecrets {
    CredentialSecrets {
        password: (credential.kind == CredentialKind::Password)
            .then(|| credential.password_secret()),
        passphrase: credential
            .key_path
            .as_deref()
            .filter(|_| credential.kind == CredentialKind::Key)
            .map(SecretRef::passphrase),
    }
}

pub fn with_saved_credential_secrets(
    mut details: CredentialDetails,
    secrets: &CredentialSecrets,
    saved: impl Fn(&SecretRef) -> bool,
) -> CredentialDetails {
    details.password_saved = secrets.password.as_ref().map(&saved);
    details.passphrase_saved = secrets.passphrase.as_ref().map(&saved);
    details
}

/// The credential search: its name, user, kind or ID.
pub fn credential_matches(details: &CredentialDetails, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    let kind = match details.kind {
        CredentialKindChoice::Password => "password 密码",
        CredentialKindChoice::Key => "key 密钥",
        CredentialKindChoice::Agent => "agent ssh agent",
    };
    needle.is_empty()
        || details.name.to_lowercase().contains(&needle)
        || details.user.to_lowercase().contains(&needle)
        || kind.to_lowercase().contains(&needle)
        || details.id.to_lowercase() == needle
}

fn bad(message: impl Into<String>) -> CliError {
    CliError::new(ErrorCode::BadRequest, message)
}

fn host_not_found(id: &str) -> CliError {
    CliError::new(
        ErrorCode::HostNotFound,
        format!("没有 ID 为 {id} 的主机：请用 shellrs hosts list 查看"),
    )
}

/// The host, saved or temporary, the CLI calls `id`.
pub fn find_host<'a>(store: &'a HostStore, id: &str) -> Result<&'a Host, CliError> {
    store
        .hosts()
        .iter()
        .chain(store.temporary_hosts())
        .find(|host| host.public_id.as_str() == id.trim())
        .ok_or_else(|| host_not_found(id))
}

/// The saved host the CLI calls `id`: what can be changed or deleted.
pub fn find_saved_host(store: &HostStore, id: &str) -> Result<HostId, CliError> {
    let host = find_host(store, id)?;
    if store.is_temporary(host.id) {
        return Err(bad(format!(
            "「{}」是没有保存的连接（临时连接或外部连接），不能修改或删除：关闭它的标签即可",
            host.name
        )));
    }
    Ok(host.id)
}

pub fn find_credential<'a>(store: &'a HostStore, id: &str) -> Result<&'a Credential, CliError> {
    store
        .credentials()
        .iter()
        .find(|credential| credential.keychain_id.as_str() == id.trim())
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::CredentialNotFound,
                format!("没有 ID 为 {id} 的凭据：请用 shellrs credentials list 查看"),
            )
        })
}

/// Where a host goes in the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupPlan {
    Existing(Option<GroupId>),
    /// Into groups made for it: `names` one inside the other, the first
    /// under `parent`.
    Create {
        parent: Option<GroupId>,
        names: Vec<String>,
    },
}

/// What becomes of a keychain entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretChange {
    Keep,
    Set(Secret),
    Delete,
    /// Moved here from the entry the host used to log in with, which the
    /// store forgets once nothing uses it, as the host form does.
    Carry(SecretRef),
}

impl SecretChange {
    /// What to write, `Some(None)` to delete; `None` for nothing, or for
    /// a value that has to be read from where it moves from first.
    pub fn value(&self) -> Option<Option<String>> {
        match self {
            SecretChange::Keep | SecretChange::Carry(_) => None,
            SecretChange::Set(secret) => Some(Some(secret.expose().to_string())),
            SecretChange::Delete => Some(None),
        }
    }

    fn from_field(field: Option<Option<Secret>>) -> Self {
        match field {
            None => SecretChange::Keep,
            Some(None) => SecretChange::Delete,
            Some(Some(secret)) => SecretChange::Set(secret),
        }
    }

    /// What `field` means for an entry the host moves to from `before`:
    /// left out, the old one comes along.
    fn moving(field: Option<Option<Secret>>, before: Option<SecretRef>, after: &SecretRef) -> Self {
        match (field, before) {
            (None, Some(before)) if before != *after => SecretChange::Carry(before),
            (field, _) => Self::from_field(field),
        }
    }
}

/// A host to save, checked: the draft (its group still to be placed),
/// where it goes, and what to do with its passwords.
#[derive(Debug)]
pub struct HostPlan {
    pub draft: HostDraft,
    pub group: GroupPlan,
    pub password: Option<(SecretRef, SecretChange)>,
    pub proxy_password: Option<(SecretRef, SecretChange)>,
}

/// `fields` on top of the host `existing` is (a new host's defaults when
/// `None`), or the first thing wrong with them, in the host form's words
/// where it has them.
pub fn plan_host(
    fields: HostFields,
    existing: Option<HostId>,
    store: &HostStore,
) -> Result<HostPlan, CliError> {
    let current = existing.and_then(|id| store.saved_host(id));
    let mut draft = current
        .map(Host::draft)
        .unwrap_or_else(|| HostDraft::new("", "", 22, "", AuthKind::default(), None));
    if let Some(name) = fields.name {
        draft.name = name.into();
    }
    if let Some(address) = fields.host {
        draft.address = address.into();
    }
    if let Some(port) = fields.port {
        draft.port = port;
    }
    if let Some(notes) = fields.notes {
        draft.notes = notes.into();
    }
    let group = match fields.group {
        None => GroupPlan::Existing(draft.group),
        Some(None) => GroupPlan::Existing(None),
        Some(Some(path)) => resolve_group(store, &path)?,
    };

    // How it logs in: a credential named, or its own way.
    let named = match &fields.credential {
        Some(Some(id)) => Some(find_credential(store, id)?),
        _ => None,
    };
    let credential = match (fields.auth, named) {
        (Some(AuthChoice::Password | AuthChoice::NoPassword), Some(_)) => {
            return Err(bad(
                "auth 为 password 或 no_password 时不能同时指定 credential",
            ));
        }
        (Some(AuthChoice::Password | AuthChoice::NoPassword), None) => None,
        (_, Some(named)) => Some(named),
        (_, None) if fields.credential == Some(None) => None,
        (_, None) => draft.credential.and_then(|id| store.credential(id)),
    };
    match (fields.auth, credential) {
        (Some(AuthChoice::Credential), None) => return Err(bad("请选择凭据")),
        (_, Some(credential)) => {
            if let Some(user) = &fields.user
                && user.trim() != credential.user.as_ref()
            {
                return Err(bad(format!(
                    "使用凭据的主机以凭据的用户名 {} 登录：去掉 user，或改凭据的用户名",
                    credential.user
                )));
            }
            draft.credential = Some(credential.id);
            draft.user = credential.user.clone();
            draft.auth = AuthKind::default();
        }
        (auth, None) => {
            let previous = draft.credential.and_then(|id| store.credential(id));
            draft.auth = match (auth, previous) {
                (Some(AuthChoice::NoPassword), _) => AuthKind::NoPassword,
                (Some(_), _) => AuthKind::Password,
                // Off its credential without saying how: as when the
                // credential is deleted.
                (None, Some(previous)) => previous.kind.without_credential(),
                (None, None) => draft.auth,
            };
            draft.credential = None;
            if let Some(user) = fields.user {
                draft.user = user.into();
            }
        }
    }
    let own_password = draft.credential.is_none() && draft.auth == AuthKind::Password;
    if !own_password && matches!(fields.password, Some(Some(_))) {
        return Err(bad("只有 auth 为 password 的主机才保存自己的密码"));
    }

    // The proxy password field, when the route was given at all.
    let mut proxy_field = None;
    if let Some(route) = fields.route {
        draft.route = match route {
            RouteFields::Direct => Route::Direct,
            RouteFields::Jump { hosts } => Route::Jump(jump_hosts(&hosts, existing, store)?),
            RouteFields::Proxy {
                kind,
                host,
                port,
                user,
                password,
            } => {
                let user = user.unwrap_or_default();
                if user.trim().is_empty() && matches!(password, Some(Some(_))) {
                    return Err(bad("填写代理密码时请同时填写用户名"));
                }
                proxy_field = Some(password);
                let kind = match kind {
                    ProxyChoice::Http => ProxyKind::Http,
                    ProxyChoice::Socks5 => ProxyKind::Socks5,
                };
                Route::Proxy(ProxySettings::new(kind, host, port).with_user(user))
            }
        };
    }
    let draft = draft.validated().map_err(|error| bad(error.message()))?;

    let password = own_password.then(|| {
        let after = draft.password_secret();
        let before = current
            .filter(|host| host.credential.is_none() && host.auth == AuthKind::Password)
            .map(Host::password_secret);
        let change = SecretChange::moving(fields.password, before, &after);
        (after, change)
    });
    let proxy_password = match &draft.route {
        Route::Proxy(proxy) => proxy.password_secret().map(|after| {
            let before = current.and_then(|host| match &host.route {
                Route::Proxy(proxy) => proxy.password_secret(),
                _ => None,
            });
            let change = SecretChange::moving(proxy_field.flatten(), before, &after);
            (after, change)
        }),
        _ => None,
    };
    Ok(HostPlan {
        draft,
        group,
        password,
        proxy_password,
    })
}

/// The jump hosts the CLI names, as the store keeps them, or why they will
/// not do. Only saved hosts, never the host itself, each once: the host
/// form offers no other. The store would quietly turn the rest into
/// deleted jump hosts.
fn jump_hosts(
    ids: &[Option<String>],
    own: Option<HostId>,
    store: &HostStore,
) -> Result<Vec<Option<HostId>>, CliError> {
    let mut hops = Vec::new();
    for id in ids {
        let Some(id) = id else {
            return Err(bad(crate::host::HostDraftError::DeletedJumpHost.message()));
        };
        let hop = find_host(store, id)?;
        if store.is_temporary(hop.id) {
            return Err(bad(format!("跳板只能是保存的主机：{id} 是没有保存的连接")));
        }
        if Some(hop.id) == own {
            return Err(bad("跳板主机不能是这台主机自己"));
        }
        if hops.contains(&Some(hop.id)) {
            return Err(bad(format!("跳板主机 {id} 重复了")));
        }
        hops.push(Some(hop.id));
    }
    Ok(hops)
}

/// The group a path such as `生产/数据库` names, or the groups to make for
/// it. The whole path is looked for first, so a name with a `/` in it
/// still matches; then level by level, making what is missing.
fn resolve_group(store: &HostStore, path: &str) -> Result<GroupPlan, CliError> {
    let path = path.trim();
    if path.is_empty() {
        return Ok(GroupPlan::Existing(None));
    }
    let names: Vec<&str> = path.split('/').map(str::trim).collect();
    if names.iter().any(|name| name.is_empty()) {
        return Err(bad(format!("分组路径「{path}」里有空的一级")));
    }
    let wanted = names.join("/");
    let ambiguous = |count: usize, path: &str| {
        bad(format!(
            "有 {count} 个分组的路径都是「{path}」：请先在 ShellRS 里给它们改名"
        ))
    };
    let whole: Vec<GroupId> = store
        .groups()
        .iter()
        .filter(|group| store.group_names(group.id).join("/") == wanted)
        .map(|group| group.id)
        .collect();
    match whole.as_slice() {
        [id] => return Ok(GroupPlan::Existing(Some(*id))),
        [] => {}
        many => return Err(ambiguous(many.len(), &wanted)),
    }
    let mut parent = None;
    for (level, name) in names.iter().enumerate() {
        let children: Vec<GroupId> = store
            .groups()
            .iter()
            .filter(|group| group.parent == parent && group.name.as_ref() == *name)
            .map(|group| group.id)
            .collect();
        match children.as_slice() {
            [] => {
                return Ok(GroupPlan::Create {
                    parent,
                    names: names[level..].iter().map(ToString::to_string).collect(),
                });
            }
            [id] => parent = Some(*id),
            many => return Err(ambiguous(many.len(), &names[..=level].join("/"))),
        }
    }
    Ok(GroupPlan::Existing(parent))
}

/// A credential to save, checked. Its password's and passphrase's entries
/// are only known once it is saved (and its key file written), so the
/// changes come without them.
pub struct CredentialPlan {
    pub draft: CredentialDraft,
    /// A pasted key, for ShellRS to keep in a file of its own; the draft's
    /// key path is a placeholder until it is written.
    pub private_key: Option<PastedKey>,
    pub password: SecretChange,
    pub passphrase: SecretChange,
}

/// `fields` on top of the credential `existing` is (a new password
/// credential when `None`), or the first thing wrong with them.
pub fn plan_credential(
    fields: CredentialFields,
    existing: Option<CredentialId>,
    store: &HostStore,
) -> Result<CredentialPlan, CliError> {
    let current = existing.and_then(|id| store.credential(id));
    let gives_key = fields.key_path.is_some() || fields.private_key.is_some();
    let kind = match (fields.kind, current) {
        (Some(kind), _) => kind,
        (None, Some(current)) => match current.kind {
            CredentialKind::Password => CredentialKindChoice::Password,
            CredentialKind::Key => CredentialKindChoice::Key,
            CredentialKind::Agent => CredentialKindChoice::Agent,
        },
        // A key given is a key credential's.
        (None, None) if gives_key => CredentialKindChoice::Key,
        (None, None) => CredentialKindChoice::Password,
    };
    let kind = match kind {
        CredentialKindChoice::Password => CredentialKind::Password,
        CredentialKindChoice::Key => CredentialKind::Key,
        CredentialKindChoice::Agent => CredentialKind::Agent,
    };
    let mut draft = current
        .map(Credential::draft)
        .unwrap_or_else(|| CredentialDraft::new("", kind, ""));
    draft.kind = kind;
    if let Some(name) = fields.name {
        draft.name = name.into();
    }
    if let Some(user) = fields.user {
        draft.user = user.into();
    }
    if gives_key && kind != CredentialKind::Key {
        return Err(bad("只有密钥凭据（kind 为 key）才有私钥"));
    }
    if kind != CredentialKind::Password && matches!(fields.password, Some(Some(_))) {
        return Err(bad("只有密码凭据（kind 为 password）才保存密码"));
    }
    if kind != CredentialKind::Key && matches!(fields.passphrase, Some(Some(_))) {
        return Err(bad("只有密钥凭据（kind 为 key）才有口令"));
    }
    let private_key = match (fields.key_path, fields.private_key) {
        (Some(_), Some(_)) => return Err(bad("key_path 和 private_key 只能给一个")),
        (Some(path), None) => {
            if !path.is_absolute() {
                return Err(bad(format!("私钥文件必须是绝对路径：{}", path.display())));
            }
            if !Path::new(&path).is_file() {
                return Err(bad(format!("私钥文件不存在：{}", path.display())));
            }
            draft = draft.with_key_path(path.display().to_string());
            None
        }
        (None, Some(text)) => {
            let pasted = PastedKey::parse(text.expose()).map_err(|error| bad(error.to_string()))?;
            if store.key_dir().is_none() {
                return Err(CliError::new(
                    ErrorCode::SaveFailed,
                    "没有可以保存私钥的目录",
                ));
            }
            // Stands in until the key file is written, so the draft passes.
            draft = draft.with_key_path("（新私钥）");
            Some(pasted)
        }
        (None, None) => None,
    };
    let draft = draft.validated().map_err(|error| bad(error.to_string()))?;
    let passphrase = match &private_key {
        // Only an encrypted key has a passphrase to keep; any other
        // clears what a key replaced in place had, as the form does.
        Some(pasted) => match fields.passphrase {
            Some(Some(passphrase)) if pasted.is_encrypted() && !passphrase.expose().is_empty() => {
                SecretChange::Set(passphrase)
            }
            Some(_) => SecretChange::Delete,
            None if pasted.is_encrypted() => SecretChange::Keep,
            None => SecretChange::Delete,
        },
        None => SecretChange::from_field(fields.passphrase),
    };
    Ok(CredentialPlan {
        draft,
        private_key,
        password: if kind == CredentialKind::Password {
            SecretChange::from_field(fields.password)
        } else {
            SecretChange::Keep
        },
        passphrase: if kind == CredentialKind::Key {
            passphrase
        } else {
            SecretChange::Keep
        },
    })
}

/// What `hosts show` prints and nobody sets, taken out of what `hosts
/// create` / `update` read, so the one's output can be fed to the other;
/// anything else unknown is still refused.
pub fn host_fields(mut value: serde_json::Value) -> Result<HostFields, String> {
    if let Some(object) = value.as_object_mut() {
        for key in ["id", "os", "temporary", "password_saved"] {
            object.remove(key);
        }
        if let Some(route) = object
            .get_mut("route")
            .and_then(|route| route.as_object_mut())
        {
            route.remove("password_saved");
        }
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// [`host_fields`] for a credential.
pub fn credential_fields(mut value: serde_json::Value) -> Result<CredentialFields, String> {
    if let Some(object) = value.as_object_mut() {
        for key in ["id", "kept", "hosts", "password_saved", "passphrase_saved"] {
            object.remove(key);
        }
        // `credentials show` prints `null` for a key ShellRS keeps.
        if object
            .get("key_path")
            .is_some_and(serde_json::Value::is_null)
        {
            object.remove("key_path");
        }
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use crate::host::{
        AuthKind, CredentialDraft, CredentialKind, GeneratedKey, GroupDraft, HostDraft, HostId,
        HostStore, KeyAlgorithm, ProxyKind, ProxySettings, Route,
    };
    use crate::secrets::SecretRef;

    use super::super::protocol::{
        AuthChoice, CliError, CredentialFields, CredentialKindChoice, ErrorCode, HostFields,
        RouteFields, Secret,
    };
    use super::{
        GroupPlan, SecretChange, credential_fields, host_details, host_fields, plan_credential,
        plan_host,
    };

    /// 生产 › 数据库, with web in 生产 and a password credential `deploy`.
    fn store() -> (HostStore, HostId, String) {
        let mut store = HostStore::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
        let web = store.insert_unnotified(HostDraft::new(
            "web",
            "10.0.0.1",
            22,
            "root",
            AuthKind::Password,
            Some(production),
        ));
        let credential = store.insert_credential_unnotified(CredentialDraft::new(
            "部署",
            CredentialKind::Password,
            "deploy",
        ));
        let id = store
            .credential(credential)
            .unwrap()
            .keychain_id
            .to_string();
        (store, web, id)
    }

    fn fields(json: serde_json::Value) -> HostFields {
        host_fields(json).unwrap()
    }

    fn refused(result: Result<impl std::fmt::Debug, CliError>) -> (ErrorCode, String) {
        let error = result.unwrap_err();
        (error.code, error.message)
    }

    fn bad(message: &str) -> (ErrorCode, String) {
        (ErrorCode::BadRequest, message.to_string())
    }

    #[test]
    fn a_new_host_needs_a_name_and_an_address_and_takes_the_forms_defaults() {
        let (store, _, _) = store();
        let plan = plan_host(
            fields(serde_json::json!({ "name": " db ", "host": "10.0.0.2" })),
            None,
            &store,
        )
        .unwrap();
        assert_eq!(plan.draft.name.as_ref(), "db");
        assert_eq!((plan.draft.port, plan.draft.user.as_ref()), (22, "root"));
        assert_eq!(plan.draft.auth, AuthKind::Password);
        assert_eq!(plan.group, GroupPlan::Existing(None));
        assert_eq!(
            plan.password,
            Some((
                SecretRef::password("root", "10.0.0.2", 22),
                SecretChange::Keep
            ))
        );

        for (json, message) in [
            (serde_json::json!({ "host": "10.0.0.2" }), "请输入名称"),
            (serde_json::json!({ "name": "db" }), "请输入地址"),
            (
                serde_json::json!({ "name": "db", "host": "h", "port": 0 }),
                "端口必须是 1 到 65535 之间的数字",
            ),
            (
                serde_json::json!({ "name": "db", "host": "h", "auth": "credential" }),
                "请选择凭据",
            ),
            (
                serde_json::json!({ "name": "db", "host": "h", "route": { "type": "jump", "hosts": [] } }),
                "请添加跳板主机",
            ),
            (
                serde_json::json!({ "name": "db", "host": "h", "route": { "type": "jump", "hosts": [null] } }),
                "请移除已删除的跳板主机",
            ),
            (
                serde_json::json!({ "name": "db", "host": "h",
                    "route": { "type": "proxy", "host": "p", "port": 3128, "password": "x" } }),
                "填写代理密码时请同时填写用户名",
            ),
            (
                serde_json::json!({ "name": "db", "host": "h", "auth": "no_password", "password": "x" }),
                "只有 auth 为 password 的主机才保存自己的密码",
            ),
        ] {
            assert_eq!(refused(plan_host(fields(json), None, &store)), bad(message));
        }
    }

    #[test]
    fn a_group_path_names_a_group_or_the_groups_to_make() {
        let (mut store, web, _) = store();
        let group = |path: &str, store: &HostStore| {
            plan_host(
                fields(serde_json::json!({ "group": path })),
                Some(web),
                store,
            )
            .map(|plan| plan.group)
        };
        let production = store.host(web).unwrap().group;
        assert_eq!(
            group(" 生产 ", &store).unwrap(),
            GroupPlan::Existing(production)
        );
        assert_eq!(
            group("生产 / 数据库 / 主库", &store).unwrap(),
            GroupPlan::Create {
                parent: Some(crate::host::GroupId(2)),
                names: vec!["主库".into()],
            }
        );
        assert_eq!(
            group("测试/web", &store).unwrap(),
            GroupPlan::Create {
                parent: None,
                names: vec!["测试".into(), "web".into()],
            }
        );
        assert_eq!(group("", &store).unwrap(), GroupPlan::Existing(None));
        assert_eq!(
            refused(group("生产//web", &store)),
            bad("分组路径「生产//web」里有空的一级")
        );
        // Two groups by one name: the whole path still tells them apart,
        // a level to make under them does not.
        store.insert_group_unnotified(GroupDraft::new("生产", None));
        assert_eq!(
            group("生产/数据库", &store).unwrap(),
            GroupPlan::Existing(Some(crate::host::GroupId(2)))
        );
        assert_eq!(
            refused(group("生产/新的", &store)),
            bad("有 2 个分组的路径都是「生产」：请先在 ShellRS 里给它们改名")
        );
        assert_eq!(
            refused(group("生产", &store)),
            bad("有 2 个分组的路径都是「生产」：请先在 ShellRS 里给它们改名")
        );
        // Unchanged when left out, the top level for null.
        let plan = plan_host(HostFields::default(), Some(web), &store).unwrap();
        assert_eq!(plan.group, GroupPlan::Existing(production));
        let plan = plan_host(
            fields(serde_json::json!({ "group": null })),
            Some(web),
            &store,
        );
        assert_eq!(plan.unwrap().group, GroupPlan::Existing(None));
    }

    #[test]
    fn a_host_on_a_credential_logs_in_as_the_credentials_user() {
        let (store, web, credential) = store();
        let plan = plan_host(
            fields(serde_json::json!({ "credential": credential })),
            Some(web),
            &store,
        )
        .unwrap();
        assert_eq!(plan.draft.user.as_ref(), "deploy");
        assert!(plan.draft.credential.is_some());
        assert_eq!(plan.password, None);

        let given = |json| refused(plan_host(fields(json), Some(web), &store));
        assert_eq!(
            given(serde_json::json!({ "credential": credential, "user": "root" })),
            bad("使用凭据的主机以凭据的用户名 deploy 登录：去掉 user，或改凭据的用户名")
        );
        assert_eq!(
            given(serde_json::json!({ "credential": credential, "auth": "password" })),
            bad("auth 为 password 或 no_password 时不能同时指定 credential")
        );
        assert_eq!(
            given(serde_json::json!({ "credential": "nope" })).0,
            ErrorCode::CredentialNotFound
        );
    }

    #[test]
    fn only_saved_hosts_other_than_itself_are_jump_hosts() {
        let (mut store, web, _) = store();
        let db = store.insert_unnotified(HostDraft::new(
            "db",
            "10.0.0.2",
            22,
            "root",
            AuthKind::Password,
            None,
        ));
        let temporary = store.insert_temporary_unnotified(
            HostDraft::new("临时", "10.0.0.3", 22, "root", AuthKind::Password, None),
            None,
        );
        let id = |host| store.host(host).unwrap().public_id.to_string();
        let jump = |hosts: Vec<String>| {
            plan_host(
                fields(serde_json::json!({ "route": { "type": "jump", "hosts": hosts } })),
                Some(web),
                &store,
            )
        };
        assert_eq!(
            jump(vec![id(db)]).unwrap().draft.route,
            Route::Jump(vec![Some(db)])
        );
        assert_eq!(
            refused(jump(vec![id(web)])),
            bad("跳板主机不能是这台主机自己")
        );
        assert_eq!(
            refused(jump(vec![id(db), id(db)])),
            bad(&format!("跳板主机 {} 重复了", id(db)))
        );
        assert_eq!(
            refused(jump(vec![id(temporary)])),
            bad(&format!(
                "跳板只能是保存的主机：{} 是没有保存的连接",
                id(temporary)
            ))
        );
        assert_eq!(
            refused(jump(vec!["nope".into()])).0,
            ErrorCode::HostNotFound
        );
    }

    #[test]
    fn a_host_that_moves_takes_its_passwords_along() {
        let (mut store, web, _) = store();
        let proxy = ProxySettings::new(ProxyKind::Http, "proxy", 3128).with_user("me");
        store.update_unnotified(
            web,
            store
                .host(web)
                .unwrap()
                .draft()
                .with_route(Route::Proxy(proxy.clone())),
        );
        let plan = plan_host(
            fields(serde_json::json!({ "port": 2222 })),
            Some(web),
            &store,
        )
        .unwrap();
        let before = SecretRef::password("root", "10.0.0.1", 22);
        assert_eq!(
            plan.password,
            Some((
                SecretRef::password("root", "10.0.0.1", 2222),
                SecretChange::Carry(before)
            ))
        );
        // The route left out: the proxy and its password as they were.
        assert_eq!(
            plan.proxy_password,
            Some((proxy.password_secret().unwrap(), SecretChange::Keep))
        );

        let plan = plan_host(
            fields(serde_json::json!({
                "password": null,
                "route": { "type": "proxy", "kind": "socks5", "host": "proxy", "port": 1080, "user": "me" },
            })),
            Some(web),
            &store,
        )
        .unwrap();
        assert_eq!(plan.password.unwrap().1, SecretChange::Delete);
        let (moved, change) = plan.proxy_password.unwrap();
        assert_eq!(moved, SecretRef::proxy("me", "proxy", 1080));
        assert_eq!(
            change,
            SecretChange::Carry(proxy.password_secret().unwrap())
        );

        let plan = plan_host(
            fields(serde_json::json!({ "password": "hunter2" })),
            Some(web),
            &store,
        )
        .unwrap();
        assert_eq!(
            plan.password.unwrap().1,
            SecretChange::Set(Secret::new("hunter2"))
        );
    }

    #[test]
    fn what_show_prints_goes_back_in_and_typos_do_not() {
        let (store, web, _) = store();
        let shown = serde_json::to_value(host_details(store.host(web).unwrap(), &store)).unwrap();
        let read = host_fields(shown).unwrap();
        assert_eq!(read.name.as_deref(), Some("web"));
        assert_eq!(read.group, Some(Some("生产".into())));
        assert_eq!(read.route, Some(RouteFields::Direct));
        assert_eq!(read.auth, Some(AuthChoice::Password));
        assert_eq!(read.password, None);
        let plan = plan_host(read, Some(web), &store).unwrap();
        assert_eq!(plan.draft, store.host(web).unwrap().draft());

        let error = host_fields(serde_json::json!({ "name": "web", "adress": "h" })).unwrap_err();
        assert!(error.contains("adress"), "{error}");
        let error = host_fields(serde_json::json!({ "route": { "type": "proxy", "host": "p", "port": 1, "pasword": "x" } }))
            .unwrap_err();
        assert!(error.contains("pasword"), "{error}");
        let read = credential_fields(
            serde_json::json!({ "id": "x", "kept": true, "key_path": null, "hosts": [] }),
        )
        .unwrap();
        assert_eq!(read, CredentialFields::default());
    }

    #[test]
    fn a_credential_takes_a_key_file_or_the_keys_text() {
        let (store, _, _) = store();
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("id_ed25519");
        std::fs::write(&key_file, "key").unwrap();
        let credential = |json: serde_json::Value, store: &HostStore| {
            plan_credential(credential_fields(json).unwrap(), None, store)
        };

        let plan = credential(
            serde_json::json!({ "name": "deploy", "key_path": key_file, "passphrase": "p" }),
            &store,
        )
        .unwrap();
        assert_eq!(plan.draft.kind, CredentialKind::Key);
        assert_eq!(plan.draft.user.as_ref(), "root");
        assert_eq!(plan.passphrase, SecretChange::Set(Secret::new("p")));
        assert_eq!(plan.password, SecretChange::Keep);

        let missing = dir.path().join("missing");
        assert_eq!(
            refused(
                credential(
                    serde_json::json!({ "name": "x", "key_path": missing }),
                    &store
                )
                .map(|_| ())
            ),
            bad(&format!("私钥文件不存在：{}", missing.display()))
        );
        assert_eq!(
            refused(
                credential(
                    serde_json::json!({ "name": "x", "key_path": "id_ed25519" }),
                    &store
                )
                .map(|_| ())
            ),
            bad("私钥文件必须是绝对路径：id_ed25519")
        );
        assert_eq!(
            refused(
                credential(
                    serde_json::json!({ "name": "x", "kind": "agent", "password": "p" }),
                    &store
                )
                .map(|_| ())
            ),
            bad("只有密码凭据（kind 为 password）才保存密码")
        );
        assert_eq!(
            refused(
                credential(serde_json::json!({ "name": "x", "kind": "key" }), &store).map(|_| ())
            ),
            bad("密钥凭据需要选择私钥文件")
        );

        // A pasted key needs somewhere to be kept, and to be a private key.
        let key = GeneratedKey::generate(KeyAlgorithm::Ed25519).unwrap();
        let text = key.encode("deploy", "").unwrap().to_string();
        let pasted = serde_json::json!({ "name": "x", "private_key": text, "passphrase": "p" });
        assert_eq!(
            refused(credential(pasted.clone(), &store).map(|_| ())).0,
            ErrorCode::SaveFailed
        );
        let kept = HostStore::empty().with_key_dir(dir.path().join("keys"));
        let plan = credential(pasted, &kept).unwrap();
        assert!(plan.private_key.is_some());
        // Not encrypted, so no passphrase to keep.
        assert_eq!(plan.passphrase, SecretChange::Delete);
        let public = key.public_key_line("deploy");
        assert_eq!(
            refused(
                credential(
                    serde_json::json!({ "name": "x", "private_key": public }),
                    &kept
                )
                .map(|_| ())
            ),
            bad("这是公钥，请粘贴私钥（以 -----BEGIN 开头的那一段）")
        );

        let plan = credential(serde_json::json!({ "name": "x", "password": "p" }), &store).unwrap();
        assert_eq!(plan.draft.kind, CredentialKind::Password);
        assert_eq!(plan.password, SecretChange::Set(Secret::new("p")));
        assert_eq!(
            credential(serde_json::json!({ "name": "x", "kind": "agent" }), &store)
                .unwrap()
                .draft
                .kind,
            CredentialKind::Agent
        );
        let _ = CredentialKindChoice::Agent;
    }
}
