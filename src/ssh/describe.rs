//! The connection's errors and questions in the user's words. The SSH layer
//! only says what happened; this is where it is worded, at the moment it is
//! shown, so a language switch shows at once.
//!
//! An error is worded link by link down its chain, the way `{:#}` prints
//! it, with each link the SSH layer knows put in words of its own.

use std::error::Error;

use super::{
    AgentProblem, AgentUnreachable, AuthError, AuthenticationPrompt, AuthenticationRequest,
    ChannelError, ConnectError, ConnectionPromptField, ExecError, ExecErrorKind, ExecFailure,
    ForwardProblem, KnownHostsError, Latency, MissingCredential, PasswordWanted, ProxyError,
    RouteFailure, UnknownHostPrompt, link_as,
};
use crate::i18n::t;

type Cause<'a> = &'a (dyn Error + 'static);

/// The error's own words, without what lies under it: what `to_string()`
/// gives, worded.
pub fn describe_error(error: &anyhow::Error) -> String {
    let causes: Vec<Cause<'_>> = error.chain().collect();
    link(causes[0], &causes[1..])
}

/// The error down to its cause, the way `{:#}` prints it, worded.
pub fn describe_chain(error: &anyhow::Error) -> String {
    let causes: Vec<Cause<'_>> = error.chain().collect();
    chain_text(&causes)
}

/// Why a login failed, from the error alone: where on the way to the host
/// it failed, a credential that was missing, the network's own answer, the
/// algorithms the server would not agree on, or whatever the error says,
/// down to its cause: 「无法建立 SSH 连接」 alone does not say what to fix.
pub fn describe_login_error(error: &anyhow::Error) -> String {
    let causes: Vec<Cause<'_>> = error.chain().collect();
    login_reason(&causes)
}

/// That nobody could be asked for `need`, in words.
pub fn describe_missing_credential(need: MissingCredential) -> String {
    match need {
        MissingCredential::Password { rejected: true } => t!("ssh.missing.password_rejected"),
        MissingCredential::Password { rejected: false } => t!("ssh.missing.password"),
        MissingCredential::Passphrase { rejected: true } => t!("ssh.missing.passphrase_rejected"),
        MissingCredential::Passphrase { rejected: false } => t!("ssh.missing.passphrase"),
        MissingCredential::KeyboardInteractive => t!("ssh.missing.keyboard_interactive"),
    }
    .into()
}

/// Why a command from the external CLI did not run to its end.
pub fn describe_exec_error(error: &ExecError) -> String {
    let causes: Vec<Cause<'_>> = error.error.chain().collect();
    if let Some(failure) = link_as::<ExecFailure>(causes[0]) {
        return exec_failure(failure, &causes[1..]);
    }
    if error.kind == ExecErrorKind::MissingCredential
        && let Some(need) = causes
            .iter()
            .find_map(|cause| link_as::<MissingCredential>(*cause))
    {
        return match need {
            MissingCredential::Password { rejected: false } => t!("ssh.exec.no_saved_password"),
            MissingCredential::Password { rejected: true } => {
                t!("ssh.exec.saved_password_rejected")
            }
            MissingCredential::Passphrase { rejected: false } => {
                t!("ssh.exec.no_saved_passphrase")
            }
            MissingCredential::Passphrase { rejected: true } => {
                t!("ssh.exec.saved_passphrase_rejected")
            }
            MissingCredential::KeyboardInteractive => t!("ssh.exec.keyboard_interactive"),
        }
        .into();
    }
    login_reason(&causes)
}

/// A round trip as the tabs show it.
pub fn latency_label(latency: Latency) -> String {
    match latency {
        Latency::Measured(rtt) => format!("{} ms", rtt.as_millis()),
        Latency::TimedOut => t!("connection.latency.timed_out").to_string(),
    }
}

/// What the trust dialog tells the user about a key seen for the first time.
pub fn unknown_host_description(prompt: &UnknownHostPrompt) -> String {
    let (host, port, algorithm, fingerprint) = (
        prompt.host(),
        prompt.port(),
        prompt.algorithm(),
        prompt.fingerprint(),
    );
    match prompt.jump_host() {
        // A line above the rest names the jump host.
        Some(jump_host) => t!(
            "connection.unknown_host.jump_host_description",
            jump_host = jump_host,
            host = host,
            port = port,
            algorithm = algorithm,
            fingerprint = fingerprint
        ),
        None => t!(
            "connection.unknown_host.description",
            host = host,
            port = port,
            algorithm = algorithm,
            fingerprint = fingerprint
        ),
    }
    .to_string()
}

/// The credential dialog's title: the server's own for its questions.
pub fn authentication_title(prompt: &AuthenticationPrompt) -> String {
    match prompt.request() {
        AuthenticationRequest::Password { .. } => t!("ssh.auth.prompt_title").to_string(),
        AuthenticationRequest::Passphrase { .. } => t!("ssh.key.prompt_title").to_string(),
        AuthenticationRequest::KeyboardInteractive { name, .. } => name.clone(),
    }
}

/// What the credential dialog says above its fields. A jump host's says
/// which one it is: the host the user opened is a different one.
pub fn authentication_instructions(prompt: &AuthenticationPrompt) -> String {
    let instructions = match prompt.request() {
        AuthenticationRequest::Password { saved_rejected } => if *saved_rejected {
            t!("ssh.auth.saved_password_rejected")
        } else {
            t!("ssh.auth.enter_password")
        }
        .to_string(),
        AuthenticationRequest::Passphrase {
            key_path,
            saved_rejected,
        } => if *saved_rejected {
            t!(
                "ssh.key.saved_passphrase_rejected",
                path = key_path.display()
            )
        } else {
            t!("ssh.key.enter_passphrase", path = key_path.display())
        }
        .to_string(),
        AuthenticationRequest::KeyboardInteractive {
            instructions,
            saved_password_rejected,
            ..
        } => {
            if !saved_password_rejected {
                instructions.clone()
            } else if instructions.trim().is_empty() {
                t!("ssh.auth.saved_password_rejected").to_string()
            } else {
                format!("{}\n{instructions}", t!("ssh.auth.saved_password_rejected"))
            }
        }
    };
    let Some(name) = prompt.jump_host() else {
        return instructions;
    };
    if instructions.trim().is_empty() {
        t!("connection.jump_host.instructions", name = name)
    } else {
        t!(
            "connection.jump_host.with_instructions",
            name = name,
            instructions = instructions
        )
    }
    .to_string()
}

/// The credential dialog's fields, labelled.
pub fn authentication_fields(prompt: &AuthenticationPrompt) -> Vec<ConnectionPromptField> {
    match prompt.request() {
        AuthenticationRequest::Password { .. } => {
            vec![ConnectionPromptField::new(
                t!("ssh.auth.password_field"),
                false,
            )]
        }
        AuthenticationRequest::Passphrase { .. } => {
            vec![ConnectionPromptField::new(
                t!("ssh.key.passphrase_field"),
                false,
            )]
        }
        AuthenticationRequest::KeyboardInteractive { fields, .. } => fields.clone(),
    }
}

/// Every link of `causes` in words, joined as `{:#}` joins them.
fn chain_text(causes: &[Cause<'_>]) -> String {
    (0..causes.len())
        .map(|ix| link(causes[ix], &causes[ix + 1..]))
        .collect::<Vec<_>>()
        .join(": ")
}

/// See [`describe_login_error`]; `causes` is the error's chain.
fn login_reason(causes: &[Cause<'_>]) -> String {
    if let Some(ix) = causes
        .iter()
        .position(|&cause| link_as::<RouteFailure>(cause).is_some())
    {
        return link(causes[ix], &causes[ix + 1..]);
    }
    for &cause in causes {
        if let Some(need) = link_as::<MissingCredential>(cause) {
            return describe_missing_credential(*need);
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return connect_error(io);
        }
        match cause.downcast_ref::<russh::Error>() {
            // russh wraps the socket's own error rather than chaining it.
            Some(russh::Error::IO(io)) => return connect_error(io),
            Some(russh::Error::ConnectionTimeout) => return t!("ssh.error.timeout").into(),
            Some(russh::Error::NoCommonAlgo { kind, theirs, .. }) => {
                return no_common_algorithm(kind, &theirs.join(&*t!("common.list.separator")));
            }
            _ => {}
        }
    }
    chain_text(causes)
}

/// One link of a chain in words, as [`describe_chain`] words each: for the
/// layers above this one, which word their own links and leave the rest
/// here. `rest` is what lies under it.
pub fn describe_link(cause: Cause<'_>, rest: &[Cause<'_>]) -> String {
    link(cause, rest)
}

/// One link of a chain in words. `rest` is what lies under it, which the
/// links that name where something failed go on to say.
fn link(cause: Cause<'_>, rest: &[Cause<'_>]) -> String {
    if let Some(error) = link_as::<ConnectError>(cause) {
        return connect(error);
    }
    if let Some(error) = link_as::<RouteFailure>(cause) {
        return route(error, rest);
    }
    if let Some(error) = link_as::<KnownHostsError>(cause) {
        return known_hosts(error);
    }
    if let Some(error) = link_as::<AuthError>(cause) {
        return auth(error);
    }
    if let Some(error) = link_as::<AgentProblem>(cause) {
        return agent(error);
    }
    if let Some(error) = link_as::<ProxyError>(cause) {
        return proxy(error);
    }
    if let Some(error) = link_as::<ChannelError>(cause) {
        return describe_channel_error(error);
    }
    if let Some(need) = link_as::<MissingCredential>(cause) {
        return describe_missing_credential(*need);
    }
    if link_as::<PasswordWanted>(cause).is_some() {
        return t!("ssh.password_wanted").into();
    }
    if let Some(failure) = link_as::<ExecFailure>(cause) {
        return exec_failure(failure, rest);
    }
    if let Some(io) = cause.downcast_ref::<std::io::Error>() {
        return io_text(io);
    }
    cause.to_string()
}

/// An I/O error's own words: those of what the SSH layer put in it, or the
/// system's.
fn io_text(io: &std::io::Error) -> String {
    match io.get_ref() {
        Some(inner) => link(inner, &[]),
        None => io.to_string(),
    }
}

fn connect(error: &ConnectError) -> String {
    match error {
        ConnectError::Handshake => t!("ssh.connect.failed"),
        ConnectError::Unreachable { host, port } => {
            t!("ssh.route.connect_failed", host = host, port = port)
        }
        ConnectError::Cancelled => t!("ssh.connect.cancelled"),
        ConnectError::NoJumpHosts => t!("ssh.jump.none"),
        ConnectError::ViewClosed => t!("ssh.prompt.view_closed"),
        ConnectError::PromptCancelled => t!("ssh.prompt.request_cancelled"),
    }
    .into()
}

fn route(failure: &RouteFailure, rest: &[Cause<'_>]) -> String {
    match failure {
        RouteFailure::JumpDeleted { number } => t!("ssh.jump.deleted", number = number),
        RouteFailure::Jump { name } => {
            t!("ssh.jump.failed", name = name, reason = login_reason(rest))
        }
        RouteFailure::JumpForward {
            name,
            host,
            port,
            problem,
        } => match problem {
            ForwardProblem::ConnectFailed => t!(
                "ssh.jump.forward.connect_failed",
                name = name,
                host = host,
                port = port
            ),
            ForwardProblem::Prohibited => t!(
                "ssh.jump.forward.prohibited",
                name = name,
                host = host,
                port = port
            ),
            ForwardProblem::TimedOut => t!(
                "ssh.jump.forward.timed_out",
                name = name,
                host = host,
                port = port
            ),
            ForwardProblem::ChannelFailed => t!(
                "ssh.jump.forward.channel_failed",
                name = name,
                host = host,
                port = port
            ),
        },
        RouteFailure::ProxyUnreachable { host, port } => t!(
            "ssh.proxy.connect_failed",
            host = host,
            port = port,
            reason = login_reason(rest)
        ),
        // What the proxy answered says it all.
        RouteFailure::Proxy => {
            return match rest {
                [answer, under @ ..] => link(*answer, under),
                [] => String::new(),
            };
        }
    }
    .into()
}

fn known_hosts(error: &KnownHostsError) -> String {
    match error {
        KnownHostsError::ReadFailed { path } => {
            t!("ssh.known_hosts.read_failed", path = path.display())
        }
        KnownHostsError::WriteFailed { path } => {
            t!("ssh.known_hosts.write_failed", path = path.display())
        }
        KnownHostsError::Corrupt { path } => t!("ssh.known_hosts.corrupt", path = path.display()),
        KnownHostsError::Unreadable { path } => {
            t!("ssh.known_hosts.unreadable", path = path.display())
        }
        KnownHostsError::ChangedWhileSaving => t!("ssh.known_hosts.changed_while_saving"),
    }
    .into()
}

fn auth(error: &AuthError) -> String {
    match error {
        AuthError::MethodsUnknown => t!("ssh.auth.methods_unknown"),
        AuthError::AgentRejected => t!("ssh.auth.agent_rejected"),
        AuthError::AgentNoPublicKey => t!("ssh.auth.agent_no_publickey"),
        AuthError::RsaHash => t!("ssh.auth.rsa_hash"),
        AuthError::AgentHash => t!("ssh.agent.hash"),
        AuthError::KeyFailed => t!("ssh.auth.key_failed"),
        AuthError::KeyRejected => t!("ssh.auth.key_rejected"),
        AuthError::NoKeyAccepted => t!("ssh.auth.no_key_accepted"),
        AuthError::KeyNotChosen => t!("ssh.key.not_chosen"),
        AuthError::KeyUnreadable { path } => t!("ssh.key.unreadable", path = path.display()),
        AuthError::KeyTooManyAttempts => t!("ssh.key.too_many_attempts"),
        AuthError::PasswordFailed => t!("ssh.auth.password_failed"),
        AuthError::Cancelled => t!("ssh.auth.cancelled"),
        AuthError::InvalidReply => t!("ssh.auth.invalid_reply"),
        AuthError::KeyboardStartFailed => t!("ssh.auth.keyboard_start_failed"),
        AuthError::KeyboardCancelled => t!("ssh.auth.keyboard_cancelled"),
        AuthError::KeyboardFailed => t!("ssh.auth.keyboard_failed"),
        AuthError::Failed => t!("ssh.auth.failed"),
    }
    .into()
}

fn agent(problem: &AgentProblem) -> String {
    match problem {
        AgentProblem::Unreachable(reason) => {
            let reason = match reason {
                AgentUnreachable::NoSocket => t!("ssh.agent.no_socket").to_string(),
                AgentUnreachable::MissingSocket { path } => {
                    t!("ssh.agent.missing_socket", path = path.display()).to_string()
                }
                AgentUnreachable::NoResponse => t!("ssh.agent.no_response").to_string(),
                AgentUnreachable::ServiceNotRunning => {
                    t!("ssh.agent.service_not_running").to_string()
                }
                AgentUnreachable::Unsupported => t!("ssh.agent.unsupported").to_string(),
                AgentUnreachable::Other(reason) => reason.clone(),
            };
            t!("ssh.agent.unreachable", reason = reason)
        }
        AgentProblem::Empty => t!("ssh.agent.empty"),
        AgentProblem::Unsigned => t!("ssh.agent.unsigned"),
    }
    .into()
}

fn proxy(error: &ProxyError) -> String {
    match error {
        ProxyError::BadCredentials => t!("ssh.proxy.bad_credentials"),
        ProxyError::AuthRequired => t!("ssh.proxy.auth_required"),
        ProxyError::Forbidden { target } => t!("ssh.proxy.forbidden", target = target),
        ProxyError::ReplyTooLong => t!("ssh.proxy.reply_too_long"),
        ProxyError::Closed => t!("ssh.proxy.closed"),
        ProxyError::HttpBadGateway { target } => {
            t!("ssh.proxy.http.bad_gateway", target = target)
        }
        ProxyError::HttpGatewayTimeout { target } => {
            t!("ssh.proxy.http.gateway_timeout", target = target)
        }
        ProxyError::HttpRefused { status } => t!("ssh.proxy.http.refused", status = status),
        ProxyError::HttpUnrecognized => t!("ssh.proxy.http.unrecognized"),
        ProxyError::SocksUnrecognized => t!("ssh.proxy.socks.unrecognized"),
        ProxyError::SocksCredentialsTooLong => t!("ssh.proxy.socks.credentials_too_long"),
        ProxyError::SocksNoPasswordAuth => t!("ssh.proxy.socks.no_password_auth"),
        ProxyError::SocksHostTooLong => t!("ssh.proxy.socks.host_too_long"),
        ProxyError::SocksRulesForbid { target } => {
            t!("ssh.proxy.socks.rules_forbid", target = target)
        }
        ProxyError::SocksNetworkUnreachable { target } => {
            t!("ssh.proxy.socks.network_unreachable", target = target)
        }
        ProxyError::SocksHostUnreachable { target } => {
            t!("ssh.proxy.socks.host_unreachable", target = target)
        }
        ProxyError::SocksRefused { target } => t!("ssh.proxy.socks.refused", target = target),
        ProxyError::SocksTimeout { target } => t!("ssh.proxy.socks.timeout", target = target),
        ProxyError::SocksInternal { target } => t!("ssh.proxy.socks.internal", target = target),
        ProxyError::SocksNoConnect => t!("ssh.proxy.socks.no_connect"),
        ProxyError::SocksAddressType => t!("ssh.proxy.socks.address_type"),
    }
    .into()
}

/// A channel on a live connection that failed, in words.
pub fn describe_channel_error(error: &ChannelError) -> String {
    match error {
        ChannelError::SessionFailed => t!("ssh.terminal.session_failed"),
        ChannelError::PtyRefused => t!("ssh.terminal.pty_refused"),
        ChannelError::ShellRefused => t!("ssh.terminal.shell_refused"),
        ChannelError::WriteFailed => t!("ssh.terminal.write_failed"),
        ChannelError::ResizeFailed => t!("ssh.terminal.resize_failed"),
        ChannelError::ExecChannelFailed => t!("ssh.exec.channel_failed"),
        ChannelError::ExecSendFailed => t!("ssh.exec.send_failed"),
        ChannelError::ExecRefused => t!("ssh.exec.refused"),
        ChannelError::ExecTimeout => t!("ssh.exec.timeout"),
        ChannelError::SftpRefused => t!("ssh.terminal.sftp_refused"),
        ChannelError::SftpTimeout => t!("ssh.terminal.sftp_timeout"),
        ChannelError::ShellOnly => t!("ssh.terminal.shell_only"),
        ChannelError::ShellOnlySftp => t!("ssh.terminal.shell_only_sftp"),
    }
    .into()
}

fn exec_failure(failure: &ExecFailure, rest: &[Cause<'_>]) -> String {
    let under = || chain_text(rest);
    match failure {
        ExecFailure::RuntimeFailed => t!("ssh.exec.runtime_failed", error = under()),
        ExecFailure::CommandFailed => t!("ssh.exec.command_failed", error = under()),
        ExecFailure::Aborted => t!("ssh.exec.aborted"),
        ExecFailure::Refused => t!("ssh.exec.refused"),
        ExecFailure::Disconnected => t!("ssh.exec.disconnected"),
        ExecFailure::HostKeyChanged { endpoint } => {
            t!("ssh.exec.host_key_changed", endpoint = endpoint)
        }
        ExecFailure::HostKeyUnknown { endpoint } => {
            t!("ssh.exec.host_key_unknown", endpoint = endpoint)
        }
    }
    .into()
}

/// That the server has none of the algorithms of `kind` that ShellRS has,
/// and which it has: `theirs`.
fn no_common_algorithm(kind: &russh::AlgorithmKind, theirs: &str) -> String {
    match kind {
        russh::AlgorithmKind::Kex => t!("ssh.error.no_common.kex", theirs = theirs),
        russh::AlgorithmKind::Key => t!("ssh.error.no_common.host_key", theirs = theirs),
        russh::AlgorithmKind::Cipher => t!("ssh.error.no_common.cipher", theirs = theirs),
        russh::AlgorithmKind::Mac => t!("ssh.error.no_common.mac", theirs = theirs),
        russh::AlgorithmKind::Compression => {
            t!("ssh.error.no_common.compression", theirs = theirs)
        }
    }
    .into()
}

/// A connect error in plain words. The common kinds get an explanation;
/// anything else keeps the system's own description.
fn connect_error(error: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::ConnectionRefused => t!("ssh.error.refused").into(),
        ErrorKind::TimedOut => t!("ssh.error.timeout").into(),
        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable => {
            t!("ssh.error.unreachable").into()
        }
        _ => t!("ssh.error.connect_failed", error = io_text(error)).into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};
    use std::time::Duration;

    use anyhow::anyhow;

    use super::*;
    use crate::ssh::{ConnectionPromptKind, find_cause, wrap};

    fn reason(error: anyhow::Error) -> String {
        describe_login_error(&error)
    }

    #[test]
    fn latency_labels_show_whole_milliseconds() {
        assert_eq!(
            latency_label(Latency::Measured(Duration::from_micros(32_900))),
            "32 ms"
        );
        assert_eq!(latency_label(Latency::TimedOut), "超时");
    }

    fn description(jump_host: Option<&str>) -> String {
        match ConnectionPromptKind::unknown_host("10.0.0.5", 22, "ssh-ed25519", "SHA256:abc")
            .at_jump_host(jump_host)
        {
            ConnectionPromptKind::UnknownHost(prompt) => unknown_host_description(&prompt),
            _ => unreachable!("an unknown host stays one"),
        }
    }

    #[test]
    fn a_new_host_key_is_described_line_by_line() {
        assert_eq!(
            description(Some("堡垒机")),
            "跳板主机：堡垒机\n主机：10.0.0.5:22\n算法：ssh-ed25519\n\
             SHA-256 指纹：SHA256:abc\n\n请先确认该指纹来自可信渠道。"
        );
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        assert_eq!(
            description(None),
            "Host: 10.0.0.5:22\nAlgorithm: ssh-ed25519\nSHA-256 fingerprint: SHA256:abc\n\n\
             Make sure the fingerprint matches one from a source you trust."
        );
    }

    fn prompt(request: AuthenticationRequest, jump_host: Option<&str>) -> AuthenticationPrompt {
        match ConnectionPromptKind::authentication(request).at_jump_host(jump_host) {
            ConnectionPromptKind::Authentication(prompt) => prompt,
            _ => unreachable!("a credential question stays one"),
        }
    }

    #[test]
    fn credential_questions_are_worded_for_what_they_ask() {
        let password = prompt(
            AuthenticationRequest::Password {
                saved_rejected: false,
            },
            None,
        );
        assert_eq!(authentication_title(&password), "SSH 登录");
        assert_eq!(authentication_instructions(&password), "请输入登录密码");
        assert_eq!(authentication_fields(&password)[0].label(), "密码");

        let passphrase = prompt(
            AuthenticationRequest::Passphrase {
                key_path: "/keys/id_ed25519".into(),
                saved_rejected: true,
            },
            Some("堡垒机"),
        );
        assert_eq!(authentication_title(&passphrase), "私钥口令");
        assert_eq!(
            authentication_instructions(&passphrase),
            "跳板主机「堡垒机」：已保存的口令无法解开 /keys/id_ed25519，请重新输入"
        );

        let code = prompt(
            AuthenticationRequest::KeyboardInteractive {
                name: "OTP".into(),
                instructions: "Enter the code".into(),
                fields: vec![ConnectionPromptField::new("Code: ", true)],
                saved_password_rejected: true,
            },
            None,
        );
        assert_eq!(authentication_title(&code), "OTP");
        assert_eq!(
            authentication_instructions(&code),
            "已保存的密码被服务器拒绝，请重新输入\nEnter the code"
        );
        assert_eq!(authentication_fields(&code)[0].label(), "Code: ");
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
        let refused = wrap(
            ConnectError::Handshake,
            Error::from(ErrorKind::ConnectionRefused),
        );
        assert_eq!(reason(refused), "连接被拒绝，该端口上没有服务在监听");
        assert_eq!(
            reason(Error::from(ErrorKind::HostUnreachable).into()),
            "主机不可达"
        );
        assert_eq!(reason(russh::Error::ConnectionTimeout.into()), "连接超时");
        let wrapped = wrap(
            ConnectError::Handshake,
            russh::Error::IO(Error::from(ErrorKind::ConnectionRefused)),
        );
        assert_eq!(reason(wrapped), "连接被拒绝，该端口上没有服务在监听");
    }

    #[test]
    fn a_failure_on_the_way_says_where_it_happened() {
        let missing = wrap(
            RouteFailure::Jump {
                name: "阿里云99".into(),
            },
            MissingCredential::Password { rejected: false },
        );
        assert_eq!(reason(missing), "跳板主机「阿里云99」：未填写密码");
        let deleted = anyhow::Error::from(RouteFailure::JumpDeleted { number: 2 });
        assert_eq!(
            reason(deleted),
            "第 2 台跳板主机已被删除，请编辑这台主机的连接方式"
        );
        let proxy = wrap(
            RouteFailure::ProxyUnreachable {
                host: "proxy.test".into(),
                port: 8080,
            },
            russh::Error::IO(Error::from(ErrorKind::ConnectionRefused)),
        );
        assert_eq!(
            reason(proxy),
            "无法连接代理服务器 proxy.test:8080：连接被拒绝，该端口上没有服务在监听"
        );
        // A proxy that reports the host unreachable says it in its words.
        let gateway = wrap(
            RouteFailure::Proxy,
            Error::new(
                ErrorKind::ConnectionRefused,
                ProxyError::HttpBadGateway {
                    target: "10.0.0.5:22".into(),
                },
            ),
        );
        assert_eq!(
            reason(gateway),
            "代理服务器无法连接到 10.0.0.5:22：连接失败（HTTP 502）"
        );
    }

    #[test]
    fn anything_else_keeps_the_connector_wording() {
        assert_eq!(
            reason(AuthError::KeyRejected.into()),
            "服务器未接受指定的私钥"
        );
        // With what lies under it, which says what to fix.
        let kex = wrap(ConnectError::Handshake, russh::Error::Kex);
        assert_eq!(
            reason(kex),
            "无法建立 SSH 连接，请检查地址、端口和主机密钥: Key exchange failed"
        );
        assert_eq!(reason(anyhow!("anything")), "anything");
    }

    #[test]
    fn reasons_read_as_english_sentences() {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        let refused = wrap(
            ConnectError::Handshake,
            Error::from(ErrorKind::ConnectionRefused),
        );
        assert_eq!(
            reason(refused),
            "Connection refused: nothing is listening on that port"
        );
        assert_eq!(
            reason(MissingCredential::Password { rejected: true }.into()),
            "Incorrect username or password"
        );
        let mac = anyhow::Error::from(russh::Error::NoCommonAlgo {
            kind: russh::AlgorithmKind::Mac,
            ours: vec!["hmac-sha2-256".into()],
            theirs: vec!["hmac-sha1".into(), "hmac-md5".into()],
        });
        assert_eq!(
            reason(mac),
            "Couldn’t establish an SSH connection: no MAC algorithm in common with the \
             server, which supports hmac-sha1, hmac-md5"
        );
    }

    #[test]
    fn a_server_of_other_algorithms_says_which_it_has() {
        let mac = wrap(
            ConnectError::Handshake,
            russh::Error::NoCommonAlgo {
                kind: russh::AlgorithmKind::Mac,
                ours: vec!["hmac-sha2-256".into()],
                theirs: vec!["hmac-sha1".into(), "hmac-md5".into()],
            },
        );
        assert_eq!(
            reason(mac),
            "无法建立 SSH 连接：和服务器没有共同的消息校验（MAC）算法，服务器支持的是 hmac-sha1、hmac-md5"
        );
    }

    #[test]
    fn errors_word_every_link_of_their_chain() {
        let corrupt = anyhow::Error::from(KnownHostsError::Corrupt {
            path: "/data/known_hosts".into(),
        });
        assert_eq!(
            describe_error(&corrupt),
            "主机信任文件已损坏：/data/known_hosts"
        );
        let agent = anyhow::Error::from(AgentProblem::Unreachable(AgentUnreachable::NoSocket));
        assert_eq!(
            describe_chain(&agent),
            "无法连接 SSH Agent：没有设置 SSH_AUTH_SOCK"
        );
        let channel = wrap(ConnectError::Cancelled, ChannelError::PtyRefused);
        assert_eq!(describe_chain(&channel), "连接已取消: 服务器拒绝创建终端");
        // A layer is found by its type wherever it is in the chain.
        let deep = channel.context("outer");
        assert_eq!(
            find_cause::<ConnectError>(&deep),
            Some(&ConnectError::Cancelled)
        );
        assert_eq!(
            describe_chain(&deep),
            "outer: 连接已取消: 服务器拒绝创建终端"
        );
    }
}
