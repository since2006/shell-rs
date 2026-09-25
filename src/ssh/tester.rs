//! Log in once with the session form's values, for its 「测试连接」 button.
use std::{
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use anyhow::Result;
use tokio::sync::watch;
use zeroize::Zeroizing;

use super::connection::{
    MissingCredential, SshConnectionConfig, SshConnector, SshPrompts, timeout_excluding_prompts,
};
use crate::{
    connection::{
        ConnectionPrompt, ConnectionPromptKind, ConnectionPromptReply, ConnectionTester, LoginTest,
        TrustCallback,
    },
    secrets::{SecretRef, SecretStore, SharedSecretStore},
};

/// How long a test may take, not counting the time a person spends deciding
/// whether to trust the host key.
const TEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Tests a login through the same connector as terminals and SFTP, so host
/// trust goes through the same file and lock.
pub struct SshConnectionTester {
    connector: SshConnector,
}

impl SshConnectionTester {
    pub fn new(connector: SshConnector) -> Self {
        Self { connector }
    }
}

impl ConnectionTester for SshConnectionTester {
    fn test(&self, request: LoginTest, trust: TrustCallback) -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("无法启动连接测试：{error}"))?;
        runtime.block_on(self.run(request, trust))
    }
}

/// What the host-key checks said along the way, which the connect error alone
/// does not tell apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HostTrust {
    declined: bool,
    key_changed: bool,
}

impl SshConnectionTester {
    async fn run(&self, request: LoginTest, trust: TrustCallback) -> Result<(), String> {
        // Resolved apart from connecting, so a mistyped name says so instead
        // of surfacing the resolver's own wording.
        if tokio::net::lookup_host((request.host(), request.port()))
            .await
            .is_err()
        {
            return Err(format!("无法解析主机 {}", request.host()));
        }

        let config = SshConnectionConfig::new(
            request.host(),
            request.port(),
            request.user(),
            request.auth(),
            request.key_path().map(Into::into),
        );
        let secrets: SharedSecretStore =
            Arc::new(FormSecrets::new(&request, self.connector.secrets().clone()));
        let host_trust = Arc::new(Mutex::new(HostTrust::default()));
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        // The events callback answers the trust question through the broker,
        // which does not exist yet when the callback is made.
        let broker_slot: Arc<OnceLock<Weak<SshPrompts>>> = Arc::new(OnceLock::new());
        let events = {
            let trust = Arc::new(trust);
            let host_trust = host_trust.clone();
            let broker_slot = broker_slot.clone();
            Arc::new(move |prompt: ConnectionPrompt| match prompt.kind() {
                ConnectionPromptKind::UnknownHost(question) => {
                    let (trust, host_trust, broker_slot) =
                        (trust.clone(), host_trust.clone(), broker_slot.clone());
                    let (request_id, question) = (prompt.request_id(), question.clone());
                    // The answer takes a person; keep the connection's runtime
                    // free while they decide.
                    tokio::task::spawn_blocking(move || {
                        let trusted = trust(question);
                        if !trusted {
                            lock(&host_trust).declined = true;
                        }
                        if let Some(broker) = broker_slot.get().and_then(Weak::upgrade) {
                            broker.respond(
                                request_id,
                                if trusted {
                                    ConnectionPromptReply::TrustAndSave
                                } else {
                                    ConnectionPromptReply::Cancel
                                },
                            );
                        }
                    });
                    true
                }
                ConnectionPromptKind::HostKeyChanged(_) => {
                    lock(&host_trust).key_changed = true;
                    true
                }
                // Credential questions never get here: the broker is
                // non-interactive and fails with a `MissingCredential`.
                ConnectionPromptKind::Authentication(_) => false,
            })
        };
        let broker = Arc::new(SshPrompts::new(events, shutdown_rx).non_interactive());
        let _ = broker_slot.set(Arc::downgrade(&broker));

        let connected = timeout_excluding_prompts(
            self.connector
                .connect_with(&config, broker.clone(), &secrets),
            broker.prompt_activity_receiver(),
            TEST_TIMEOUT,
        )
        .await
        .and_then(|result| result);
        match connected {
            Ok((handle, _)) => {
                let _ = handle
                    .disconnect(russh::Disconnect::ByApplication, "connection test", "zh-CN")
                    .await;
                Ok(())
            }
            Err(error) => Err(describe_test_failure(&error, *lock(&host_trust))),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Why a test failed, in the words the notification shows.
fn describe_test_failure(error: &anyhow::Error, host_trust: HostTrust) -> String {
    if host_trust.key_changed {
        return "主机密钥与已保存的不一致，已阻止连接。请先核实服务器身份".into();
    }
    if host_trust.declined {
        return "未信任该主机的密钥".into();
    }
    describe_login_error(error)
}

/// Why a login failed, from the error alone: a credential that was
/// missing, the network's own answer, or whatever the error says.
pub(super) fn describe_login_error(error: &anyhow::Error) -> String {
    for cause in error.chain() {
        if let Some(need) = cause.downcast_ref::<MissingCredential>() {
            return need.to_string();
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return describe_connect_error(io);
        }
        match cause.downcast_ref::<russh::Error>() {
            // russh wraps the socket's own error rather than chaining it.
            Some(russh::Error::IO(io)) => return describe_connect_error(io),
            Some(russh::Error::ConnectionTimeout) => return "连接超时".into(),
            _ => {}
        }
    }
    error.to_string()
}

/// A connect error in plain words. The common kinds get an explanation;
/// anything else keeps the system's own description.
fn describe_connect_error(error: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::ConnectionRefused => "连接被拒绝，该端口上没有服务在监听".into(),
        ErrorKind::TimedOut => "连接超时".into(),
        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable => "主机不可达".into(),
        _ => format!("无法连接：{error}"),
    }
}

/// The secrets a test logs in with. The form is authoritative for the login
/// password and for the passphrase of the key it names, whether or not they
/// were saved; anything else (a default key's passphrase) is read from the
/// keychain. Nothing is ever written.
struct FormSecrets {
    password: (SecretRef, Option<Zeroizing<String>>),
    passphrase: Option<(SecretRef, Option<Zeroizing<String>>)>,
    fallback: SharedSecretStore,
}

impl FormSecrets {
    fn new(request: &LoginTest, fallback: SharedSecretStore) -> Self {
        let owned = |value: Option<&str>| value.map(|value| Zeroizing::new(value.to_string()));
        Self {
            password: (
                SecretRef::password(request.user(), request.host(), request.port()),
                owned(request.password()),
            ),
            passphrase: request
                .key_path()
                .map(|path| (SecretRef::passphrase(path), owned(request.passphrase()))),
            fallback,
        }
    }
}

impl SecretStore for FormSecrets {
    fn get(&self, secret: &SecretRef) -> anyhow::Result<Option<Zeroizing<String>>> {
        if *secret == self.password.0 {
            return Ok(self.password.1.clone());
        }
        if let Some((key, value)) = &self.passphrase
            && secret == key
        {
            return Ok(value.clone());
        }
        self.fallback.get(secret)
    }

    fn set(&self, _: &SecretRef, _: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn delete(&self, _: &SecretRef) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_available(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};

    use anyhow::anyhow;

    use super::{HostTrust, MissingCredential, describe_test_failure};

    fn reason(error: anyhow::Error) -> String {
        describe_test_failure(&error, HostTrust::default())
    }

    #[test]
    fn host_trust_outranks_the_connect_error() {
        let error = anyhow!("无法建立 SSH 连接，请检查主机、端口和主机密钥");
        let changed = HostTrust {
            key_changed: true,
            ..HostTrust::default()
        };
        assert!(describe_test_failure(&error, changed).starts_with("主机密钥与已保存的不一致"));
        let declined = HostTrust {
            declined: true,
            ..HostTrust::default()
        };
        assert_eq!(
            describe_test_failure(&error, declined),
            "未信任该主机的密钥"
        );
    }

    #[test]
    fn missing_credentials_name_what_was_wrong() {
        assert_eq!(
            reason(MissingCredential::Password { rejected: true }.into()),
            "用户名或密码错误"
        );
        assert_eq!(
            reason(MissingCredential::Password { rejected: false }.into()),
            "未填写密码"
        );
        assert_eq!(
            reason(MissingCredential::Passphrase { rejected: true }.into()),
            "私钥口令错误"
        );
        assert_eq!(
            reason(MissingCredential::Passphrase { rejected: false }.into()),
            "私钥已加密，未填写口令"
        );
        assert!(reason(MissingCredential::KeyboardInteractive.into()).contains("键盘交互"));
    }

    #[test]
    fn network_errors_are_explained_under_any_context() {
        let refused = anyhow::Error::from(Error::from(ErrorKind::ConnectionRefused))
            .context("无法建立 SSH 连接");
        assert_eq!(reason(refused), "连接被拒绝，该端口上没有服务在监听");
        assert_eq!(
            reason(Error::from(ErrorKind::HostUnreachable).into()),
            "主机不可达"
        );
        assert_eq!(reason(russh::Error::ConnectionTimeout.into()), "连接超时");
        let wrapped =
            anyhow::Error::from(russh::Error::IO(Error::from(ErrorKind::ConnectionRefused)))
                .context("无法建立 SSH 连接");
        assert_eq!(reason(wrapped), "连接被拒绝，该端口上没有服务在监听");
    }

    #[test]
    fn anything_else_keeps_the_connector_wording() {
        assert_eq!(
            reason(anyhow!("服务器未接受指定的私钥")),
            "服务器未接受指定的私钥"
        );
    }
}
