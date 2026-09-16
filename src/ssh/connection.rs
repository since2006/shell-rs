//! SSH connection and authentication shared by terminals and SFTP.
use crate::{
    connection::{
        ConnectionPrompt, ConnectionPromptField, ConnectionPromptKind, ConnectionPromptReply,
    },
    secrets::{SecretRef, SharedSecretStore},
    session::{AuthKind, Session},
};
use anyhow::{Context as _, Result, anyhow, bail};
use russh::keys::{
    HashAlg, PrivateKeyWithHashAlg, PublicKey,
    agent::client::AgentClient,
    known_hosts::{known_host_keys_path, learn_known_hosts_path},
    load_secret_key, parse_public_key_base64,
};
use russh::{
    MethodKind, MethodSet,
    client::{self, AuthResult, KeyboardInteractiveAuthResponse},
};
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{oneshot, watch};
use zeroize::Zeroizing;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const AUTH_RETRIES: usize = 3;
static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct SshConnectionConfig {
    host: String,
    port: u16,
    user: String,
    auth: AuthKind,
    key_path: Option<PathBuf>,
}

impl From<&Session> for SshConnectionConfig {
    fn from(session: &Session) -> Self {
        Self {
            host: session.host.to_string(),
            port: session.port,
            user: session.user.to_string(),
            auth: session.auth,
            key_path: session
                .key_path
                .as_ref()
                .map(|path| PathBuf::from(path.as_ref())),
        }
    }
}

/// Cloneable trust and credential service. Every clone shares the trust-file lock.
#[derive(Clone)]
pub struct SshConnector {
    known_hosts_path: PathBuf,
    known_hosts_lock: Arc<Mutex<()>>,
    secrets: SharedSecretStore,
}
impl SshConnector {
    pub fn new(path: impl Into<PathBuf>, secrets: SharedSecretStore) -> Self {
        Self {
            known_hosts_path: path.into(),
            known_hosts_lock: Arc::new(Mutex::new(())),
            secrets,
        }
    }
    /// Must run on a worker: keychain and private-key reads are blocking.
    pub async fn connect(
        &self,
        config: &SshConnectionConfig,
        broker: Arc<SshPrompts>,
    ) -> Result<(client::Handle<SshClientHandler>, String)> {
        let fingerprint = Arc::new(Mutex::new(String::new()));
        let handler = SshClientHandler {
            host: config.host.clone(),
            port: config.port,
            known_hosts_path: self.known_hosts_path.clone(),
            known_hosts_lock: self.known_hosts_lock.clone(),
            broker: broker.clone(),
            fingerprint: fingerprint.clone(),
        };
        let connect = client::connect(
            Arc::new(client::Config {
                keepalive_interval: Some(KEEPALIVE_INTERVAL),
                keepalive_max: 3,
                nodelay: true,
                ..Default::default()
            }),
            (config.host.as_str(), config.port),
            handler,
        );
        let mut shutdown = broker.shutdown_receiver();
        let mut handle = tokio::select! {
            result = timeout_excluding_prompts(connect, broker.prompt_activity_receiver(), CONNECT_TIMEOUT) => result?.map_err(|e| e.context(safe_connect_error()))?,
            _ = shutdown.changed() => bail!("连接已取消"),
        };
        tokio::select! {
            result = authenticate(&mut handle, config, &self.secrets, &broker) => result?,
            _ = shutdown.changed() => bail!("连接已取消"),
        }
        let fingerprint = fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Ok((handle, fingerprint))
    }
}
impl SshConnectionConfig {
    pub fn endpoint(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

pub struct SshPrompts {
    pending: Mutex<HashMap<u64, oneshot::Sender<ConnectionPromptReply>>>,
    events: Arc<dyn Fn(ConnectionPrompt) -> bool + Send + Sync>,
    shutdown: watch::Receiver<bool>,
    prompt_activity: watch::Sender<bool>,
}

impl SshPrompts {
    pub fn new(
        events: Arc<dyn Fn(ConnectionPrompt) -> bool + Send + Sync>,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        let (prompt_activity, _) = watch::channel(false);
        Self {
            pending: Mutex::new(HashMap::new()),
            events,
            shutdown,
            prompt_activity,
        }
    }

    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown.clone()
    }

    fn prompt_activity_receiver(&self) -> watch::Receiver<bool> {
        self.prompt_activity.subscribe()
    }

    async fn ask(&self, kind: ConnectionPromptKind) -> Result<ConnectionPromptReply> {
        let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(request_id, sender);
        let _ = self.prompt_activity.send(true);
        let result = async {
            if !(self.events)(ConnectionPrompt::new(request_id, kind)) {
                bail!("连接视图已关闭");
            }
            let mut shutdown = self.shutdown.clone();
            tokio::select! {
                reply = receiver => reply.map_err(|_| anyhow!("认证请求已取消")),
                _ = shutdown.changed() => bail!("连接已取消"),
            }
        }
        .await;
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&request_id);
        let _ = self.prompt_activity.send(false);
        result
    }

    async fn emit(&self, kind: ConnectionPromptKind) {
        let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        (self.events)(ConnectionPrompt::new(request_id, kind));
    }

    pub fn respond(&self, request_id: u64, reply: ConnectionPromptReply) {
        if let Some(sender) = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&request_id)
        {
            let _ = sender.send(reply);
        }
    }

    pub fn cancel_all(&self) {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
}

/// Apply the network handshake timeout without counting time spent waiting
/// for an explicit answer from the user to a host-trust prompt.
async fn timeout_excluding_prompts<F>(
    future: F,
    mut prompt_activity: watch::Receiver<bool>,
    timeout: Duration,
) -> Result<F::Output>
where
    F: Future,
{
    tokio::pin!(future);
    let mut remaining = timeout;
    loop {
        if *prompt_activity.borrow() {
            tokio::select! {
                result = &mut future => return Ok(result),
                changed = prompt_activity.changed() => {
                    if changed.is_err() {
                        bail!("连接已取消");
                    }
                }
            }
            continue;
        }

        let started = tokio::time::Instant::now();
        tokio::select! {
            result = &mut future => return Ok(result),
            _ = tokio::time::sleep(remaining) => return Err(russh::Error::ConnectionTimeout.into()),
            changed = prompt_activity.changed() => {
                if changed.is_err() {
                    bail!("连接已取消");
                }
                remaining = remaining.saturating_sub(started.elapsed());
                if remaining.is_zero() && !*prompt_activity.borrow() {
                    return Err(russh::Error::ConnectionTimeout.into());
                }
            }
        }
    }
}

pub type SshHandle = client::Handle<SshClientHandler>;

pub struct SshClientHandler {
    host: String,
    port: u16,
    known_hosts_path: PathBuf,
    known_hosts_lock: Arc<Mutex<()>>,
    broker: Arc<SshPrompts>,
    fingerprint: Arc<Mutex<String>>,
}

impl client::Handler for SshClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_public_key.public_key();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        *self.fingerprint.lock().unwrap_or_else(|e| e.into_inner()) = fingerprint.clone();
        let algorithm = key.algorithm().to_string();
        let known = {
            let _guard = self
                .known_hosts_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            read_known_keys(&self.host, self.port, &self.known_hosts_path)?
        };
        if known.iter().any(|(_, saved)| saved == &key) {
            return Ok(true);
        }
        if !known.is_empty() {
            let old = known
                .iter()
                .map(|(_, saved)| saved.fingerprint(HashAlg::Sha256).to_string())
                .collect();
            self.broker
                .emit(ConnectionPromptKind::host_key_changed(
                    &self.host,
                    self.port,
                    algorithm,
                    old,
                    fingerprint,
                    &self.known_hosts_path,
                ))
                .await;
            return Ok(false);
        }

        let reply = self
            .broker
            .ask(ConnectionPromptKind::unknown_host(
                &self.host,
                self.port,
                algorithm,
                fingerprint,
            ))
            .await?;
        if !matches!(reply, ConnectionPromptReply::TrustAndSave) {
            return Ok(false);
        }
        let _guard = self
            .known_hosts_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let known = read_known_keys(&self.host, self.port, &self.known_hosts_path)?;
        if known.iter().any(|(_, saved)| saved == &key) {
            return Ok(true);
        }
        if !known.is_empty() {
            bail!("保存主机密钥时发现信任文件已发生变化");
        }
        learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts_path)
            .map_err(|_| anyhow!("无法写入主机信任文件：{}", self.known_hosts_path.display()))?;
        Ok(true)
    }
}

fn read_known_keys(host: &str, port: u16, path: &Path) -> Result<Vec<(usize, PublicKey)>> {
    if path.exists() {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取主机信任文件：{}", path.display()))?;
        for line in contents.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let valid = fields.next().is_some()
                && fields.next().is_some()
                && fields
                    .next()
                    .is_some_and(|encoded| parse_public_key_base64(encoded).is_ok());
            if !valid {
                bail!("主机信任文件已损坏：{}", path.display());
            }
        }
    }
    known_host_keys_path(host, port, path)
        .map_err(|_| anyhow!("主机信任文件已损坏或无法读取：{}", path.display()))
}

async fn authenticate<H: client::Handler>(
    handle: &mut client::Handle<H>,
    config: &SshConnectionConfig,
    secrets: &SharedSecretStore,
    broker: &SshPrompts,
) -> Result<()>
where
    H::Error: From<russh::Error>,
{
    let first = handle
        .authenticate_none(&config.user)
        .await
        .map_err(|_| anyhow!("无法查询服务器支持的认证方式"))?;
    if first.success() {
        return Ok(());
    }
    let mut methods = remaining_methods(first);
    let mut partial = false;

    if config.auth == AuthKind::Auto
        && methods.contains(&MethodKind::PublicKey)
        && let Some(result) = try_agent(handle, &config.user).await?
    {
        if result.success() {
            return Ok(());
        }
        partial = is_partial(&result);
        methods = remaining_methods(result);
    }

    if matches!(config.auth, AuthKind::Auto | AuthKind::Key)
        && methods.contains(&MethodKind::PublicKey)
    {
        let paths = key_paths(config)?;
        for path in paths {
            let Some(key) = load_private_key(&path, secrets, broker).await? else {
                continue;
            };
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(|_| anyhow!("无法协商 RSA 签名算法"))?
                .flatten();
            let result = handle
                .authenticate_publickey(
                    &config.user,
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(|_| anyhow!("私钥认证失败"))?;
            if result.success() {
                return Ok(());
            }
            partial = is_partial(&result);
            methods = remaining_methods(result);
            if partial {
                break;
            }
        }
        if config.auth == AuthKind::Key && !partial {
            bail!("服务器未接受指定的私钥");
        }
    }

    if matches!(config.auth, AuthKind::Auto | AuthKind::Password) || partial {
        if methods.contains(&MethodKind::Password) {
            // Try what the session has saved before bothering anyone.
            let mut saved_rejected = false;
            if let Some(saved) = saved_secret(secrets, &password_secret(config)) {
                let result = handle
                    .authenticate_password(&config.user, saved.to_string())
                    .await
                    .map_err(|_| anyhow!("密码认证失败"))?;
                if result.success() {
                    return Ok(());
                }
                // The entry stays: the user typed it into the session dialog,
                // and deleting it behind their back would be baffling. This
                // connection just falls back to asking, and says why.
                saved_rejected = true;
                partial = is_partial(&result);
                methods = remaining_methods(result);
            }
            let instructions = if saved_rejected {
                "已保存的密码被服务器拒绝，请重新输入"
            } else {
                "请输入登录密码"
            };
            if !partial && methods.contains(&MethodKind::Password) {
                for _ in 0..AUTH_RETRIES {
                    let answer = ask_one_secret(broker, "SSH 登录", instructions, "密码").await?;
                    let result = handle
                        .authenticate_password(&config.user, answer.into_inner())
                        .await
                        .map_err(|_| anyhow!("密码认证失败"))?;
                    if result.success() {
                        return Ok(());
                    }
                    partial = is_partial(&result);
                    methods = remaining_methods(result);
                    if partial || !methods.contains(&MethodKind::Password) {
                        break;
                    }
                }
            }
        }
        if methods.contains(&MethodKind::KeyboardInteractive) {
            for _ in 0..AUTH_RETRIES {
                if keyboard_interactive(handle, &config.user, broker).await? {
                    return Ok(());
                }
            }
        }
    }
    bail!("认证失败：服务器未接受可用的认证方式")
}

async fn try_agent<H: client::Handler>(
    handle: &mut client::Handle<H>,
    user: &str,
) -> Result<Option<AuthResult>>
where
    H::Error: From<russh::Error>,
{
    #[cfg(unix)]
    {
        let Ok(mut agent) = AgentClient::connect_env().await else {
            return Ok(None);
        };
        let Ok(identities) = agent.request_identities().await else {
            return Ok(None);
        };
        let hash = handle
            .best_supported_rsa_hash()
            .await
            .map_err(|_| anyhow!("无法协商 SSH Agent 签名算法"))?
            .flatten();
        let mut last = None;
        for identity in identities {
            let key = identity.public_key().into_owned();
            match handle
                .authenticate_publickey_with(user, key, hash, &mut agent)
                .await
            {
                Ok(result) if result.success() => return Ok(Some(result)),
                Ok(result) => last = Some(result),
                Err(_) => continue,
            }
        }
        Ok(last)
    }
    #[cfg(not(unix))]
    {
        let _ = (handle, user);
        Ok(None)
    }
}

fn key_paths(config: &SshConnectionConfig) -> Result<Vec<PathBuf>> {
    if config.auth == AuthKind::Key {
        return config
            .key_path
            .clone()
            .map(|path| vec![path])
            .ok_or_else(|| anyhow!("私钥认证需要选择私钥文件"));
    }
    let Some(home) = dirs::home_dir() else {
        return Ok(Vec::new());
    };
    Ok(["id_ed25519", "id_ecdsa", "id_rsa"]
        .into_iter()
        .map(|name| home.join(".ssh").join(name))
        .collect())
}

async fn load_private_key(
    path: &Path,
    secrets: &SharedSecretStore,
    broker: &SshPrompts,
) -> Result<Option<russh::keys::PrivateKey>> {
    if !path.exists() {
        return Ok(None);
    }
    match load_secret_key(path, None) {
        Ok(key) => return Ok(Some(key)),
        Err(russh::keys::Error::KeyIsEncrypted) => {}
        Err(_) => bail!("无法读取私钥文件：{}", path.display()),
    }
    // Passphrases are saved per key file, so one saved answer unlocks the same
    // key for every session that uses it.
    let mut saved_rejected = false;
    if let Some(saved) = saved_secret(secrets, &SecretRef::passphrase(path)) {
        if let Ok(key) = load_secret_key(path, Some(saved.as_str())) {
            return Ok(Some(key));
        }
        saved_rejected = true;
    }
    let instructions = if saved_rejected {
        format!("已保存的口令无法解开 {}，请重新输入", path.display())
    } else {
        format!("请输入 {} 的口令", path.display())
    };
    for _ in 0..AUTH_RETRIES {
        let answer = ask_one_secret(broker, "私钥口令", &instructions, "口令").await?;
        if let Ok(key) = load_secret_key(path, Some(answer.expose())) {
            return Ok(Some(key));
        }
    }
    bail!("私钥口令错误次数过多")
}

/// Where this connection's password lives in the system keychain.
fn password_secret(config: &SshConnectionConfig) -> SecretRef {
    SecretRef::password(&config.user, &config.host, config.port)
}

/// Read a saved secret. A keychain that errors, is locked, or holds an empty
/// value counts as nothing saved: the connection then falls back to asking,
/// which is always better than refusing to connect.
///
/// This blocks. It only ever runs on the SSH worker thread, whose runtime
/// serves this one connection, and only while authentication is already
/// waiting on a person.
fn saved_secret(secrets: &SharedSecretStore, secret: &SecretRef) -> Option<Zeroizing<String>> {
    secrets
        .get(secret)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

async fn ask_one_secret(
    broker: &SshPrompts,
    title: &str,
    instructions: &str,
    label: &str,
) -> Result<crate::connection::ConnectionSecret> {
    let reply = broker
        .ask(ConnectionPromptKind::authentication(
            title,
            instructions,
            vec![ConnectionPromptField::new(label, false)],
        ))
        .await?;
    match reply {
        ConnectionPromptReply::Answers(mut answers) if answers.len() == 1 => Ok(answers.remove(0)),
        ConnectionPromptReply::Cancel => bail!("认证已取消"),
        _ => bail!("认证回复无效"),
    }
}

async fn keyboard_interactive<H: client::Handler>(
    handle: &mut client::Handle<H>,
    user: &str,
    broker: &SshPrompts,
) -> Result<bool>
where
    H::Error: From<russh::Error>,
{
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(|_| anyhow!("无法开始交互式认证"))?;
    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let fields = prompts
                    .into_iter()
                    .map(|prompt| ConnectionPromptField::new(prompt.prompt, prompt.echo))
                    .collect();
                let reply = broker
                    .ask(ConnectionPromptKind::authentication(
                        name,
                        instructions,
                        fields,
                    ))
                    .await?;
                let ConnectionPromptReply::Answers(answers) = reply else {
                    bail!("交互式认证已取消")
                };
                response = handle
                    .authenticate_keyboard_interactive_respond(
                        answers
                            .into_iter()
                            .map(|answer| answer.into_inner())
                            .collect(),
                    )
                    .await
                    .map_err(|_| anyhow!("交互式认证失败"))?;
            }
        }
    }
}

fn remaining_methods(result: AuthResult) -> MethodSet {
    match result {
        AuthResult::Success => MethodSet::empty(),
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
    }
}

fn is_partial(result: &AuthResult) -> bool {
    matches!(
        result,
        AuthResult::Failure {
            partial_success: true,
            ..
        }
    )
}

fn safe_connect_error() -> &'static str {
    "无法建立 SSH 连接，请检查主机、端口和主机密钥"
}

#[cfg(test)]
mod tests {
    use super::read_known_keys;
    #[test]
    fn malformed_known_hosts_is_blocked() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        std::fs::write(&path, "this is not a key\n").unwrap();
        let error = read_known_keys("example.test", 22, &path).unwrap_err();
        assert!(error.to_string().contains("已损坏"));
    }
}
