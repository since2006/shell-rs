//! Log in once with the host form's values, for its 「测试连接」 button.
use std::{
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
    time::Duration,
};

use anyhow::Result;
use tokio::sync::watch;
use zeroize::Zeroizing;

use super::describe::{describe_login_error, describe_missing_credential};
use super::{
    ConnectionPrompt, ConnectionPromptKind, ConnectionPromptReply, MissingCredential,
    PasswordWanted, RouteFailure, SshConnectionConfig, SshConnector, SshPrompts, find_cause,
    timeout_excluding_prompts,
};
use crate::{
    connection::{ConnectionTester, LoginTest, TelnetTest, TrustCallback},
    host::{JumpLogin, LoginMethod, LoginRoute},
    i18n::t,
    secrets::{SecretRef, SecretSource, SharedSecretSource},
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
            .map_err(|error| t!("ssh.test.runtime_failed", error = error).to_string())?;
        runtime.block_on(self.run(request, trust))
    }

    fn test_telnet(&self, request: TelnetTest, trust: TrustCallback) -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| t!("ssh.test.runtime_failed", error = error).to_string())?;
        runtime.block_on(self.run_telnet(request, trust))
    }

    /// A serial line has nothing of SSH: the serial module opens it.
    fn test_serial(&self, line: &crate::host::SerialLine) -> Result<(), String> {
        crate::serial::check(line)
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
        let login = request.login();
        resolve_first_leg(&login.route, &login.host, login.port).await?;

        // No password typed: go without one, as 「无密码」 does (the server's
        // leave, the SSH agent, the default keys), and fail if that fails.
        let without_password = request.password_left_empty();
        let config = if without_password {
            let mut login = request.login().clone();
            login.method = LoginMethod::NoPassword;
            SshConnectionConfig::from(&login)
        } else {
            SshConnectionConfig::from(request.login())
        };
        let secrets: SharedSecretSource =
            Arc::new(FormSecrets::new(&request, self.connector.secrets().clone()));
        let (broker, host_trust, _shutdown) = test_broker(trust);

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
            // The server wanted the password that was left empty: that, not
            // 「无密码」, is what to tell someone who chose 「密码」.
            Err(error)
                if without_password
                    && find_cause::<RouteFailure>(&error).is_none()
                    && error.chain().any(|cause| cause.is::<PasswordWanted>()) =>
            {
                Err(describe_missing_credential(MissingCredential::Password {
                    rejected: false,
                }))
            }
            Err(error) => Err(describe_test_failure(&error, *lock(&host_trust))),
        }
    }

    /// The way to a Telnet host, and nothing more: what answers there is
    /// the device's business.
    async fn run_telnet(&self, request: TelnetTest, trust: TrustCallback) -> Result<(), String> {
        resolve_first_leg(request.route(), request.host(), request.port()).await?;
        let secrets: SharedSecretSource = Arc::new(FormSecrets::for_proxy(
            request.route(),
            request.proxy_password(),
            self.connector.secrets().clone(),
        ));
        let (broker, host_trust, _shutdown) = test_broker(trust);
        let reached = timeout_excluding_prompts(
            self.connector.open_route(
                request.route(),
                request.host(),
                request.port(),
                &broker,
                &secrets,
            ),
            broker.prompt_activity_receiver(),
            TEST_TIMEOUT,
        )
        .await
        .and_then(|result| result);
        match reached {
            Ok(stream) => {
                drop(stream);
                Ok(())
            }
            Err(error) => Err(describe_test_failure(&error, *lock(&host_trust))),
        }
    }
}

/// Resolve the first leg apart from connecting, so a mistyped name says so
/// instead of surfacing the resolver's own wording. Only the first leg: a
/// proxy or a jump host resolves what comes after.
async fn resolve_first_leg(route: &LoginRoute, host: &str, port: u16) -> Result<(), String> {
    let first = match route {
        LoginRoute::Direct => Some((host, port)),
        LoginRoute::Proxy(proxy) => Some((proxy.host.as_str(), proxy.port)),
        LoginRoute::Jump(hops) => match hops.first() {
            Some(JumpLogin::Host { login, .. }) => Some((login.host.as_str(), login.port)),
            _ => None,
        },
    };
    match first {
        Some((host, port)) if tokio::net::lookup_host((host, port)).await.is_err() => {
            Err(t!("ssh.test.unresolved", host = host).into())
        }
        _ => Ok(()),
    }
}

/// The broker a test connects with: it asks nobody for a password, and
/// answers a host key seen for the first time through `trust`. What the
/// checks said comes back in the `HostTrust`; the watch's sender keeps the
/// broker from counting as shut down while it is held.
fn test_broker(
    trust: TrustCallback,
) -> (Arc<SshPrompts>, Arc<Mutex<HostTrust>>, watch::Sender<bool>) {
    let host_trust = Arc::new(Mutex::new(HostTrust::default()));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
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
            // Declined at once, which fails the test and says why:
            // replacing the saved key is left to a real connection.
            ConnectionPromptKind::HostKeyChanged(_) => {
                lock(&host_trust).key_changed = true;
                false
            }
            // Credential questions never get here: the broker is
            // non-interactive and fails with a `MissingCredential`.
            ConnectionPromptKind::Authentication(_) => false,
        })
    };
    let broker = Arc::new(SshPrompts::new(events, shutdown_rx).non_interactive());
    let _ = broker_slot.set(Arc::downgrade(&broker));
    (broker, host_trust, shutdown_tx)
}

/// Lock a mutex that a panicking thread may have poisoned; the data it guards
/// here stays usable either way.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Why a test failed, in the words the notification shows.
fn describe_test_failure(error: &anyhow::Error, host_trust: HostTrust) -> String {
    if host_trust.key_changed {
        return t!("ssh.test.host_key_changed").into();
    }
    if host_trust.declined {
        return t!("ssh.test.host_key_declined").into();
    }
    describe_login_error(error)
}

/// The secrets a test logs in with. For a login typed into the form, the
/// form is authoritative for the login password and for the passphrase of
/// the key it names, whether or not they were saved; anything else (a
/// default key's passphrase) is read from the keychain. A login through a
/// credential reads everything from the keychain. Nothing is ever written.
struct FormSecrets {
    typed: Vec<(SecretRef, Option<Zeroizing<String>>)>,
    fallback: SharedSecretSource,
}

impl FormSecrets {
    fn new(request: &LoginTest, fallback: SharedSecretSource) -> Self {
        let owned = |value: Option<&str>| value.map(|value| Zeroizing::new(value.to_string()));
        let mut typed = Vec::new();
        let login = request.login();
        if request.is_typed() {
            typed.push((login.password.clone(), owned(request.password())));
            if let Some(path) = &login.key_path {
                typed.push((SecretRef::passphrase(path), owned(request.passphrase())));
            }
        }
        // The proxy is the form's whichever way the host logs in.
        if let Some(proxy) = login.route.proxy_password() {
            typed.push((proxy, owned(request.proxy_password())));
        }
        Self { typed, fallback }
    }

    /// Only the proxy's password from the form, for a test that logs in
    /// nowhere but the jump hosts, with what they saved.
    fn for_proxy(route: &LoginRoute, password: Option<&str>, fallback: SharedSecretSource) -> Self {
        let typed = route
            .proxy_password()
            .map(|proxy| {
                (
                    proxy,
                    password.map(|value| Zeroizing::new(value.to_string())),
                )
            })
            .into_iter()
            .collect();
        Self { typed, fallback }
    }
}

impl SecretSource for FormSecrets {
    fn secret(&self, secret: &SecretRef) -> anyhow::Result<Option<Zeroizing<String>>> {
        match self.typed.iter().find(|(key, _)| key == secret) {
            Some((_, value)) => Ok(value.clone()),
            None => self.fallback.secret(secret),
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::{HostTrust, describe_test_failure};

    #[test]
    fn host_trust_outranks_the_connect_error() {
        let error = anyhow!("无法建立 SSH 连接，请检查地址、端口和主机密钥");
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
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        assert_eq!(
            describe_test_failure(&anyhow!("x"), declined),
            "The host key wasn’t trusted"
        );
    }
}
