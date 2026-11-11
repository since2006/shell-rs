//! One-time codes against OpenSSH with pam_google_authenticator, as a
//! server that asks for them is usually set up, in a container of Debian's
//! packages. Each login runs a command the way the external CLI's `exec`
//! does: nobody can be asked anything, so a code not filled in fails it.
//!
//! Needs Docker; run with `cargo test -p shellrs-ssh totp -- --include-ignored`
//! (CI: `ssh.yml`).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use zeroize::Zeroizing;

use crate::{
    AgentLocation, HostLogin, LoginMethod, MissingCredential, SecretRef, SecretSource,
    SshConnectionConfig, SshConnector, TotpLogin, find_cause, run_command, totp,
};

/// The secret every user's codes come from.
const SECRET: &str = "JBSWY3DPEHPK3PXP";
const PASSWORD: &str = "otp-password";
/// The port sshd listens on in the container.
const PORT: u16 = 2222;

/// What the container runs: sshd on [`PORT`], with three users whose codes
/// come from [`SECRET`], each code taken once.
///
/// - `otp` types a password, then a code (`Password: `, then
///   `Verification code: `), both asked by keyboard-interactive.
/// - `keyotp` signs in with a key, then types a code.
/// - `codeonly` types a code and nothing else.
const SETUP: &str = r#"set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq openssh-server libpam-google-authenticator >/dev/null
mkdir -p /run/sshd
for user in otp keyotp codeonly; do
  useradd -m -s /bin/sh "$user"
  echo "$user:otp-password" | chpasswd
  printf '%s\n" TOTP_AUTH\n" DISALLOW_REUSE\n" WINDOW_SIZE 3\n' JBSWY3DPEHPK3PXP > "/home/$user/.google_authenticator"
  chown "$user:" "/home/$user/.google_authenticator"
  chmod 600 "/home/$user/.google_authenticator"
done
mkdir -p /home/keyotp/.ssh
ssh-keygen -q -t ed25519 -N '' -f /home/keyotp/.ssh/id_ed25519
cp /home/keyotp/.ssh/id_ed25519.pub /home/keyotp/.ssh/authorized_keys
chown -R keyotp: /home/keyotp/.ssh
chmod 700 /home/keyotp/.ssh
sed -i 's/^@include common-auth$/auth [success=1 default=ignore] pam_succeed_if.so quiet user in keyotp:codeonly\nauth requisite pam_unix.so\nauth required pam_google_authenticator.so/' /etc/pam.d/sshd
printf 'KbdInteractiveAuthentication yes\nPasswordAuthentication no\nUsePAM yes\n' > /etc/ssh/sshd_config.d/totp.conf
printf '\nMatch User otp,codeonly\n    AuthenticationMethods keyboard-interactive\nMatch User keyotp\n    AuthenticationMethods publickey,keyboard-interactive\n' >> /etc/ssh/sshd_config
/usr/sbin/sshd -t
echo READY
exec /usr/sbin/sshd -D -e -p 2222
"#;

/// A container, removed when dropped.
struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}

impl Container {
    fn exec(&self, command: &str) -> String {
        let output = Command::new("docker")
            .args(["exec", &self.0, "sh", "-c", command])
            .output()
            .expect("docker exec");
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

/// sshd in a container, on a port of this computer.
struct Server {
    container: Container,
    port: u16,
}

fn start() -> Server {
    let name = format!("shellrs-totp-{:08x}", std::process::id() ^ rand_suffix());
    let started = Command::new("docker")
        .args([
            "run",
            "-d",
            "--rm",
            "--name",
            &name,
            "-p",
            &format!("127.0.0.1::{PORT}"),
            "debian:stable-slim",
            "sh",
            "-c",
            SETUP,
        ])
        .output()
        .expect("docker run");
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let container = Container(name);
    let since = Instant::now();
    loop {
        assert!(since.elapsed() < Duration::from_secs(300), "sshd starts");
        let logs = Command::new("docker")
            .args(["logs", &container.0])
            .output()
            .expect("docker logs");
        // sshd logs to standard error.
        let (out, err) = (
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr),
        );
        if out.contains("READY") && err.contains("Server listening") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let port = Command::new("docker")
        .args(["port", &container.0, &PORT.to_string()])
        .output()
        .expect("docker port");
    let port = String::from_utf8(port.stdout)
        .unwrap()
        .lines()
        .next()
        .and_then(|line| line.rsplit(':').next()?.trim().parse().ok())
        .expect("the mapped port");
    Server { container, port }
}

/// Some bits that differ between the containers of tests run at once.
fn rand_suffix() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    nanos.wrapping_mul(2_654_435_761)
}

/// Secrets in memory, as the keychain would hold them.
#[derive(Default)]
struct Secrets(HashMap<SecretRef, String>);

impl SecretSource for Secrets {
    fn secret(&self, secret: &SecretRef) -> Result<Option<Zeroizing<String>>> {
        Ok(self.0.get(secret).cloned().map(Zeroizing::new))
    }
}

/// What a test logs in with: a connector that trusts the server's key and
/// knows `secrets`, and a folder for the files it needs.
struct Client {
    connector: SshConnector,
    folder: tempfile::TempDir,
}

fn client_of(server: &Server, secrets: Secrets) -> Client {
    let folder = tempfile::tempdir().unwrap();
    let host_key = server
        .container
        .exec("cat /etc/ssh/ssh_host_ed25519_key.pub");
    let mut key = host_key.split_whitespace();
    let known_hosts = folder.path().join("known_hosts");
    std::fs::write(
        &known_hosts,
        format!(
            "[127.0.0.1]:{} {} {}\n",
            server.port,
            key.next().unwrap(),
            key.next().unwrap()
        ),
    )
    .unwrap();
    let connector = SshConnector::new(known_hosts, Arc::new(secrets))
        .with_agent(AgentLocation::None)
        .with_default_key_dir(None);
    Client { connector, folder }
}

fn login(server: &Server, user: &str, method: LoginMethod, wording: Option<&str>) -> HostLogin {
    let mut login = HostLogin::new("127.0.0.1", server.port, user, method);
    login.totp = Some(Box::new(TotpLogin {
        prompt: wording.map(str::to_string),
        secret: SecretRef::totp(user),
    }));
    login
}

/// The secrets of `user`: the code's, and the password when it types one.
fn secrets_of(server: &Server, user: &str, password: bool) -> Secrets {
    let mut secrets = Secrets::default();
    secrets.0.insert(SecretRef::totp(user), SECRET.to_string());
    if password {
        secrets.0.insert(
            SecretRef::password(user, "127.0.0.1", server.port),
            PASSWORD.to_string(),
        );
    }
    secrets
}

/// Log in and run `echo`, as `shellrs exec` does.
fn echo(client: &Client, login: &HostLogin) -> Result<String, crate::ExecError> {
    let mut printed = Vec::new();
    run_command(
        &client.connector,
        &SshConnectionConfig::from(login),
        "echo signed-in",
        &mut |_, bytes| {
            printed.extend_from_slice(bytes);
            Ok(())
        },
    )?;
    Ok(String::from_utf8_lossy(&printed).into_owned())
}

/// Wait for a code to start, so that two logins right after share it.
fn at_a_fresh_code() {
    let next = totp::current_step() + 1;
    std::thread::sleep(totp::until_step(next) + Duration::from_millis(500));
}

#[test]
#[ignore = "needs Docker"]
fn openssh_with_google_authenticator_takes_the_codes() {
    let server = start();

    // A password, then the code the host's wording names; again with the
    // usual wordings, at a code of its own: the server takes each once.
    for (ix, wording) in [Some("Verification code"), None].into_iter().enumerate() {
        if ix > 0 {
            at_a_fresh_code();
        }
        let client = client_of(&server, secrets_of(&server, "otp", true));
        let printed = echo(
            &client,
            &login(&server, "otp", LoginMethod::Password, wording),
        )
        .unwrap_or_else(|error| panic!("{wording:?}: {:?}", error.error));
        assert_eq!(printed.trim(), "signed-in");
    }

    // A key, then a code.
    let key = server.container.exec("cat /home/keyotp/.ssh/id_ed25519");
    let client = client_of(&server, secrets_of(&server, "keyotp", false));
    let key_path = client.folder.path().join("id_ed25519");
    write_private_key(&key_path, &key);
    let mut with_key = login(&server, "keyotp", LoginMethod::Key, None);
    with_key.key_path = Some(key_path);
    let printed = echo(&client, &with_key).unwrap_or_else(|error| panic!("{:?}", error.error));
    assert_eq!(printed.trim(), "signed-in");

    // Nothing but a code, for a login without a password.
    let client = client_of(&server, secrets_of(&server, "codeonly", false));
    let printed = echo(
        &client,
        &login(&server, "codeonly", LoginMethod::NoPassword, None),
    )
    .unwrap_or_else(|error| panic!("{:?}", error.error));
    assert_eq!(printed.trim(), "signed-in");

    // A wording that is not the server's leaves the question unanswered,
    // and nobody is there to answer it.
    let client = client_of(&server, secrets_of(&server, "otp", true));
    let error = echo(
        &client,
        &login(
            &server,
            "otp",
            LoginMethod::Password,
            Some("Please enter the mobile app OTP verification code"),
        ),
    )
    .unwrap_err();
    assert_eq!(
        find_cause::<MissingCredential>(&error.error),
        Some(&MissingCredential::KeyboardInteractive),
        "{:?}",
        error.error
    );
}

#[test]
#[ignore = "needs Docker"]
fn a_code_taken_once_is_followed_by_the_next_one() {
    let server = start();
    let client = client_of(&server, secrets_of(&server, "otp", true));
    let host = login(&server, "otp", LoginMethod::Password, None);
    at_a_fresh_code();
    let first = Instant::now();
    assert_eq!(
        echo(&client, &host)
            .unwrap_or_else(|error| panic!("{:?}", error.error))
            .trim(),
        "signed-in"
    );
    // Within the same 30 seconds: the server refuses the code it took, and
    // the login sends the next one, with the saved password again, once it
    // is current.
    assert!(first.elapsed() < Duration::from_secs(10));
    let second = Instant::now();
    assert_eq!(
        echo(&client, &host)
            .unwrap_or_else(|error| panic!("{:?}", error.error))
            .trim(),
        "signed-in"
    );
    assert!(
        second.elapsed() > Duration::from_secs(10),
        "{:?}: the second login took the same code",
        second.elapsed()
    );
}

fn write_private_key(path: &Path, key: &str) {
    std::fs::write(path, key).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
