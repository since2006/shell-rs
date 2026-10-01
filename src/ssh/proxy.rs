//! The handshake that turns a connection to an HTTP or SOCKS5 proxy into a
//! stream to the host behind it.
//!
//! Failures to reach the host come back as the I/O error the same failure
//! would be on a direct connection (refused, unreachable, timed out), with
//! words that say the proxy reported it: a retry and the explanation then
//! work as they do without a proxy.
use std::io::{self, ErrorKind};
use std::net::IpAddr;

use anyhow::{Result, anyhow, bail};
use base64::Engine as _;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::host::ProxyKind;

/// What a proxy that wants a user name and password is given.
pub(super) struct ProxyAuth<'a> {
    pub user: &'a str,
    pub password: &'a str,
}

/// The longest response head an HTTP proxy may send before the tunnel.
const MAX_HEAD: usize = 16 * 1024;

/// Ask the proxy at the other end of `stream` for a tunnel to `host:port`.
/// When this returns, what `stream` carries is the host's.
pub(super) async fn handshake<S>(
    stream: &mut S,
    kind: ProxyKind,
    host: &str,
    port: u16,
    auth: Option<ProxyAuth<'_>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match kind {
        ProxyKind::Http => http_connect(stream, host, port, auth).await,
        ProxyKind::Socks5 => socks5_connect(stream, host, port, auth).await,
    }
}

/// `host:port` as an HTTP request names it, an IPv6 address in brackets.
fn authority(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    }
}

/// An error that reads as the proxy's report on reaching `target`, of the
/// kind a direct connection would have failed with.
fn unreachable(kind: ErrorKind, target: &str, reason: &str) -> anyhow::Error {
    io::Error::new(kind, format!("代理服务器无法连接到 {target}：{reason}")).into()
}

async fn http_connect<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    auth: Option<ProxyAuth<'_>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let target = authority(host, port);
    let mut request = Zeroizing::new(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n"));
    let authenticated = auth.is_some();
    if let Some(auth) = auth {
        let pair = Zeroizing::new(format!("{}:{}", auth.user, auth.password));
        let token =
            Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(pair.as_bytes()));
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(&token);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let head = read_head(stream).await?;
    let status_line = head.lines().next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    let status = match (parts.next(), parts.next()) {
        (Some(version), Some(code)) if version.starts_with("HTTP/") => code.parse::<u16>().ok(),
        _ => None,
    };
    match status {
        Some(200..=299) => Ok(()),
        Some(407) if authenticated => bail!("代理服务器拒绝了用户名或密码"),
        Some(407) => bail!("代理服务器要求认证，请填写代理的用户名和密码"),
        Some(403) => bail!("代理服务器不允许连接到 {target}"),
        Some(502) => Err(unreachable(
            ErrorKind::ConnectionRefused,
            &target,
            "连接失败（HTTP 502）",
        )),
        Some(504) => Err(unreachable(
            ErrorKind::TimedOut,
            &target,
            "连接超时（HTTP 504）",
        )),
        Some(_) => bail!("代理服务器拒绝了连接：{status_line}"),
        None => bail!("代理服务器的回复无法识别，请确认它是 HTTP 代理"),
    }
}

/// The response head up to its blank line, read a byte at a time: what
/// follows it is the host's and has to stay in the stream.
async fn read_head<S>(stream: &mut S) -> Result<String>
where
    S: AsyncRead + Unpin,
{
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            bail!("代理服务器的回复过长");
        }
        let byte = stream
            .read_u8()
            .await
            .map_err(|_| anyhow!("代理服务器没有回复就关闭了连接"))?;
        head.push(byte);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

const SOCKS_VERSION: u8 = 5;
const NO_AUTHENTICATION: u8 = 0x00;
const USER_PASSWORD: u8 = 0x02;
const NO_ACCEPTABLE_METHOD: u8 = 0xff;

async fn socks5_connect<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    auth: Option<ProxyAuth<'_>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let target = authority(host, port);
    let greeting: &[u8] = if auth.is_some() {
        &[SOCKS_VERSION, 2, NO_AUTHENTICATION, USER_PASSWORD]
    } else {
        &[SOCKS_VERSION, 1, NO_AUTHENTICATION]
    };
    stream.write_all(greeting).await?;
    let mut chosen = [0u8; 2];
    read_reply(stream, &mut chosen).await?;
    if chosen[0] != SOCKS_VERSION {
        bail!("代理服务器的回复无法识别，请确认它是 SOCKS5 代理");
    }
    match (chosen[1], auth) {
        (NO_AUTHENTICATION, _) => {}
        (USER_PASSWORD, Some(auth)) => {
            let (user, password) = (auth.user.as_bytes(), auth.password.as_bytes());
            if user.len() > 255 || password.len() > 255 {
                bail!("代理的用户名或密码太长，SOCKS5 最多 255 个字节");
            }
            let mut request = Zeroizing::new(Vec::with_capacity(3 + user.len() + password.len()));
            request.push(0x01);
            request.push(user.len() as u8);
            request.extend_from_slice(user);
            request.push(password.len() as u8);
            request.extend_from_slice(password);
            stream.write_all(&request).await?;
            let mut status = [0u8; 2];
            read_reply(stream, &mut status).await?;
            if status[1] != 0 {
                bail!("代理服务器拒绝了用户名或密码");
            }
        }
        (NO_ACCEPTABLE_METHOD | USER_PASSWORD, None) => {
            bail!("代理服务器要求认证，请填写代理的用户名和密码")
        }
        _ => bail!("代理服务器不接受用户名和密码认证"),
    }

    let mut request = vec![SOCKS_VERSION, 0x01, 0x00];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            request.push(0x01);
            request.extend_from_slice(&address.octets());
        }
        Ok(IpAddr::V6(address)) => {
            request.push(0x04);
            request.extend_from_slice(&address.octets());
        }
        // A name goes to the proxy as it is, for the proxy to resolve: the
        // host may only be known by that name on the proxy's side.
        Err(_) => {
            if host.len() > 255 {
                bail!("主机名太长，SOCKS5 最多 255 个字节");
            }
            request.push(0x03);
            request.push(host.len() as u8);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;
    stream.flush().await?;

    let mut reply = [0u8; 4];
    read_reply(stream, &mut reply).await?;
    if reply[0] != SOCKS_VERSION {
        bail!("代理服务器的回复无法识别，请确认它是 SOCKS5 代理");
    }
    match reply[1] {
        0x00 => {}
        0x02 => bail!("代理服务器的规则不允许连接到 {target}"),
        0x03 => {
            return Err(unreachable(
                ErrorKind::NetworkUnreachable,
                &target,
                "网络不可达",
            ));
        }
        0x04 => {
            return Err(unreachable(
                ErrorKind::HostUnreachable,
                &target,
                "主机不可达",
            ));
        }
        0x05 => {
            return Err(unreachable(
                ErrorKind::ConnectionRefused,
                &target,
                "连接被拒绝",
            ));
        }
        0x06 => return Err(unreachable(ErrorKind::TimedOut, &target, "连接超时")),
        0x07 => bail!("代理服务器不支持 CONNECT 命令"),
        0x08 => bail!("代理服务器不支持这种地址类型"),
        _ => return Err(unreachable(ErrorKind::Other, &target, "代理服务器内部错误")),
    }
    // The address the proxy connected from, which nothing here needs; it
    // has to be read off the stream all the same.
    let bound = match reply[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut length = [0u8; 1];
            read_reply(stream, &mut length).await?;
            usize::from(length[0])
        }
        _ => bail!("代理服务器的回复无法识别，请确认它是 SOCKS5 代理"),
    };
    let mut rest = vec![0u8; bound + 2];
    read_reply(stream, &mut rest).await?;
    Ok(())
}

async fn read_reply<S>(stream: &mut S, buffer: &mut [u8]) -> Result<()>
where
    S: AsyncRead + Unpin,
{
    stream
        .read_exact(buffer)
        .await
        .map(|_| ())
        .map_err(|_| anyhow!("代理服务器没有回复就关闭了连接"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use tokio::io::{DuplexStream, duplex};

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    /// Run a handshake against a proxy played by `proxy`, which reads what
    /// the client sent and answers. Returns the handshake's outcome and what
    /// the client reads after it, which has to be the host's first bytes.
    fn against<P, F>(
        kind: ProxyKind,
        host: &str,
        auth: Option<(&str, &str)>,
        proxy: P,
    ) -> (Result<()>, Vec<u8>)
    where
        P: FnOnce(DuplexStream) -> F,
        F: Future<Output = ()> + Send + 'static,
    {
        block_on(async {
            let (mut client, server) = duplex(4096);
            let proxy = tokio::spawn(proxy(server));
            let auth = auth.map(|(user, password)| ProxyAuth { user, password });
            let result = handshake(&mut client, kind, host, 22, auth).await;
            proxy.await.unwrap();
            let mut after = Vec::new();
            client.read_to_end(&mut after).await.unwrap();
            (result, after)
        })
    }

    async fn read_http_request(server: &mut DuplexStream) -> String {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(server.read_u8().await.unwrap());
        }
        String::from_utf8(request).unwrap()
    }

    #[test]
    fn an_http_proxy_opens_a_tunnel_and_leaves_the_hosts_bytes_alone() {
        let (result, after) = against(ProxyKind::Http, "10.0.0.5", None, |mut server| async move {
            let request = read_http_request(&mut server).await;
            assert_eq!(
                request,
                "CONNECT 10.0.0.5:22 HTTP/1.1\r\nHost: 10.0.0.5:22\r\n\r\n"
            );
            // The banner comes in the same write as the answer.
            server
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\nSSH-2.0-test\r\n")
                .await
                .unwrap();
        });
        result.unwrap();
        assert_eq!(after, b"SSH-2.0-test\r\n");
    }

    #[test]
    fn an_http_proxy_is_given_the_user_and_password() {
        let (result, _) = against(
            ProxyKind::Http,
            "::1",
            Some(("me", "hunter2")),
            |mut server| async move {
                let request = read_http_request(&mut server).await;
                assert!(
                    request.starts_with("CONNECT [::1]:22 HTTP/1.1\r\n"),
                    "{request}"
                );
                // base64("me:hunter2")
                assert!(request.contains("Proxy-Authorization: Basic bWU6aHVudGVyMg==\r\n"));
                server.write_all(b"HTTP/1.0 200 OK\r\n\r\n").await.unwrap();
            },
        );
        result.unwrap();
    }

    fn http_answer(status: &'static str, auth: Option<(&str, &str)>) -> anyhow::Error {
        against(
            ProxyKind::Http,
            "db.internal",
            auth,
            move |mut server| async move {
                read_http_request(&mut server).await;
                server.write_all(status.as_bytes()).await.unwrap();
            },
        )
        .0
        .unwrap_err()
    }

    fn io_kind(error: &anyhow::Error) -> Option<ErrorKind> {
        error.downcast_ref::<io::Error>().map(io::Error::kind)
    }

    #[test]
    fn an_http_proxys_refusal_says_why() {
        let error = http_answer("HTTP/1.1 407 Proxy Authentication Required\r\n\r\n", None);
        assert_eq!(
            error.to_string(),
            "代理服务器要求认证，请填写代理的用户名和密码"
        );
        let error = http_answer(
            "HTTP/1.1 407 Proxy Authentication Required\r\n\r\n",
            Some(("me", "wrong")),
        );
        assert_eq!(error.to_string(), "代理服务器拒绝了用户名或密码");

        // A host the proxy cannot reach fails like an unreachable host.
        let error = http_answer("HTTP/1.1 502 Bad Gateway\r\n\r\n", None);
        assert_eq!(io_kind(&error), Some(ErrorKind::ConnectionRefused));
        assert_eq!(
            error.to_string(),
            "代理服务器无法连接到 db.internal:22：连接失败（HTTP 502）"
        );
        let error = http_answer("HTTP/1.1 504 Gateway Timeout\r\n\r\n", None);
        assert_eq!(io_kind(&error), Some(ErrorKind::TimedOut));

        let error = http_answer("SSH-2.0-OpenSSH\r\n\r\n", None);
        assert_eq!(
            error.to_string(),
            "代理服务器的回复无法识别，请确认它是 HTTP 代理"
        );
    }

    #[test]
    fn an_endless_http_answer_is_cut_off() {
        let (result, _) = against(ProxyKind::Http, "10.0.0.5", None, |mut server| async move {
            read_http_request(&mut server).await;
            let line = [b'x'; 1024];
            for _ in 0..(MAX_HEAD / line.len() + 1) {
                if server.write_all(&line).await.is_err() {
                    break;
                }
            }
        });
        assert_eq!(result.unwrap_err().to_string(), "代理服务器的回复过长");
    }

    /// A SOCKS5 proxy that wants `auth` (or nothing), checks the request for
    /// `expected` and answers `reply`.
    fn socks5(
        host: &str,
        auth: Option<(&'static str, &'static str)>,
        wanted: Option<(&'static str, &'static str)>,
        expected: Vec<u8>,
        reply: u8,
    ) -> (Result<()>, Vec<u8>) {
        against(
            ProxyKind::Socks5,
            host,
            auth,
            move |mut server| async move {
                let mut greeting = [0u8; 2];
                server.read_exact(&mut greeting).await.unwrap();
                let mut methods = vec![0u8; usize::from(greeting[1])];
                server.read_exact(&mut methods).await.unwrap();
                let Some((user, password)) = wanted else {
                    server.write_all(&[5, 0]).await.unwrap();
                    return answer(server, expected, reply).await;
                };
                if !methods.contains(&2) {
                    server.write_all(&[5, 0xff]).await.unwrap();
                    return;
                }
                server.write_all(&[5, 2]).await.unwrap();
                let mut header = [0u8; 2];
                server.read_exact(&mut header).await.unwrap();
                let mut given_user = vec![0u8; usize::from(header[1])];
                server.read_exact(&mut given_user).await.unwrap();
                let length = server.read_u8().await.unwrap();
                let mut given_password = vec![0u8; usize::from(length)];
                server.read_exact(&mut given_password).await.unwrap();
                if given_user != user.as_bytes() || given_password != password.as_bytes() {
                    server.write_all(&[1, 1]).await.unwrap();
                    return;
                }
                server.write_all(&[1, 0]).await.unwrap();
                answer(server, expected, reply).await;
            },
        )
    }

    async fn answer(mut server: DuplexStream, expected: Vec<u8>, reply: u8) {
        let mut request = vec![0u8; expected.len()];
        server.read_exact(&mut request).await.unwrap();
        assert_eq!(request, expected);
        // Bound to a name, which has to be read off before the banner.
        let mut answer = vec![5, reply, 0, 3, 5];
        answer.extend_from_slice(b"proxy");
        answer.extend_from_slice(&1080u16.to_be_bytes());
        answer.extend_from_slice(b"SSH-2.0-test\r\n");
        server.write_all(&answer).await.unwrap();
    }

    fn connect_request(address: &[u8]) -> Vec<u8> {
        let mut request = vec![5, 1, 0];
        request.extend_from_slice(address);
        request.extend_from_slice(&22u16.to_be_bytes());
        request
    }

    #[test]
    fn a_socks5_proxy_resolves_a_name_itself() {
        let mut name = vec![3, 11];
        name.extend_from_slice(b"db.internal");
        let (result, after) = socks5("db.internal", None, None, connect_request(&name), 0);
        result.unwrap();
        assert_eq!(after, b"SSH-2.0-test\r\n");
    }

    #[test]
    fn a_socks5_proxy_is_given_addresses_as_addresses() {
        let (result, _) = socks5(
            "10.0.0.5",
            None,
            None,
            connect_request(&[1, 10, 0, 0, 5]),
            0,
        );
        result.unwrap();
        let mut v6 = vec![4];
        v6.extend_from_slice(&"::1".parse::<std::net::Ipv6Addr>().unwrap().octets());
        let (result, _) = socks5("::1", None, None, connect_request(&v6), 0);
        result.unwrap();
    }

    #[test]
    fn a_socks5_proxy_checks_the_user_and_password() {
        let request = connect_request(&[1, 10, 0, 0, 5]);
        let me = Some(("me", "hunter2"));
        let (result, _) = socks5("10.0.0.5", me, me, request.clone(), 0);
        result.unwrap();

        let (result, _) = socks5("10.0.0.5", Some(("me", "wrong")), me, request.clone(), 0);
        assert_eq!(
            result.unwrap_err().to_string(),
            "代理服务器拒绝了用户名或密码"
        );

        let (result, _) = socks5("10.0.0.5", None, me, request, 0);
        assert_eq!(
            result.unwrap_err().to_string(),
            "代理服务器要求认证，请填写代理的用户名和密码"
        );
    }

    #[test]
    fn a_socks5_proxys_failure_to_reach_the_host_fails_like_a_direct_one() {
        let request = connect_request(&[1, 10, 0, 0, 5]);
        for (reply, kind) in [
            (3, ErrorKind::NetworkUnreachable),
            (4, ErrorKind::HostUnreachable),
            (5, ErrorKind::ConnectionRefused),
            (6, ErrorKind::TimedOut),
        ] {
            let (result, _) = socks5("10.0.0.5", None, None, request.clone(), reply);
            assert_eq!(io_kind(&result.unwrap_err()), Some(kind), "reply {reply}");
        }
        let (result, _) = socks5("10.0.0.5", None, None, request, 5);
        assert_eq!(
            result.unwrap_err().to_string(),
            "代理服务器无法连接到 10.0.0.5:22：连接被拒绝"
        );
    }
}
