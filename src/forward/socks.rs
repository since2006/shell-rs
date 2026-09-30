//! The SOCKS proxy a dynamic forward speaks to its clients: SOCKS5 without
//! authentication and SOCKS4 / 4a, `CONNECT` only, which is what `ssh -D`
//! offers. The parsers are pure functions over the bytes read so far; the
//! small driver at the end feeds them from a stream.

use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// Where a client asked the proxy to connect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SocksTarget {
    pub host: String,
    pub port: u16,
}

/// Which protocol a client spoke, so the answer goes back in the same one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SocksVersion {
    V4,
    V5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SocksError {
    /// Neither SOCKS4 nor SOCKS5.
    Version,
    /// A SOCKS5 client that insists on authenticating.
    NoAcceptableMethod,
    /// `BIND` or `UDP ASSOCIATE`; only `CONNECT` is carried.
    Command,
    AddressType,
    Malformed,
    /// The client went away before finishing its request.
    Closed,
}

/// The result of looking at the bytes received so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Parsed<T> {
    /// More bytes are needed.
    Incomplete,
    /// `consumed` bytes made up the message.
    Done {
        value: T,
        consumed: usize,
    },
    Invalid(SocksError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Greeting {
    /// SOCKS5 opens with the authentication methods the client can do.
    V5 { offers_no_auth: bool },
    /// SOCKS4 sends its whole request at once.
    V4(SocksTarget),
}

/// The SOCKS5 reply codes this proxy uses (RFC 1928, section 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SocksReply {
    Succeeded = 0x00,
    GeneralFailure = 0x01,
    NotAllowed = 0x02,
    ConnectionRefused = 0x05,
    CommandNotSupported = 0x07,
    AddressTypeNotSupported = 0x08,
}

const CONNECT: u8 = 0x01;
/// The longest name either protocol can carry, and a sane bound for a
/// SOCKS4 user id, which has none of its own.
const MAX_FIELD: usize = 255;

/// The first message of either protocol.
pub(crate) fn parse_greeting(buffer: &[u8]) -> Parsed<Greeting> {
    match buffer.first() {
        None => Parsed::Incomplete,
        Some(0x05) => {
            let Some(count) = buffer.get(1).map(|count| *count as usize) else {
                return Parsed::Incomplete;
            };
            match buffer.get(2..2 + count) {
                Some(methods) => Parsed::Done {
                    value: Greeting::V5 {
                        offers_no_auth: methods.contains(&0x00),
                    },
                    consumed: 2 + count,
                },
                None => Parsed::Incomplete,
            }
        }
        Some(0x04) => parse_v4_request(buffer),
        Some(_) => Parsed::Invalid(SocksError::Version),
    }
}

/// `VER CMD PORT(2) IP(4) USERID NUL`, and for SOCKS4a `HOST NUL` after it
/// when the address is `0.0.0.x` with a non-zero `x`.
fn parse_v4_request(buffer: &[u8]) -> Parsed<Greeting> {
    if let Some(command) = buffer.get(1)
        && *command != CONNECT
    {
        return Parsed::Invalid(SocksError::Command);
    }
    let Some(fixed) = buffer.get(..8) else {
        return Parsed::Incomplete;
    };
    let port = u16::from_be_bytes([fixed[2], fixed[3]]);
    let address = [fixed[4], fixed[5], fixed[6], fixed[7]];
    let user_id = match terminated(&buffer[8..]) {
        Ok(Some(user_id)) => user_id,
        Ok(None) => return Parsed::Incomplete,
        Err(error) => return Parsed::Invalid(error),
    };
    let after_user_id = 8 + user_id.len() + 1;
    let named = address[..3] == [0, 0, 0] && address[3] != 0;
    if !named {
        return Parsed::Done {
            value: Greeting::V4(SocksTarget {
                host: Ipv4Addr::from(address).to_string(),
                port,
            }),
            consumed: after_user_id,
        };
    }
    let name = match terminated(&buffer[after_user_id..]) {
        Ok(Some(name)) => name,
        Ok(None) => return Parsed::Incomplete,
        Err(error) => return Parsed::Invalid(error),
    };
    match host_name(name) {
        Some(host) => Parsed::Done {
            value: Greeting::V4(SocksTarget { host, port }),
            consumed: after_user_id + name.len() + 1,
        },
        None => Parsed::Invalid(SocksError::Malformed),
    }
}

/// The bytes before the first NUL, once it has arrived. A field that grows
/// past `MAX_FIELD` without one is not going to be a valid request.
fn terminated(buffer: &[u8]) -> Result<Option<&[u8]>, SocksError> {
    match buffer.iter().position(|byte| *byte == 0) {
        Some(end) if end <= MAX_FIELD => Ok(Some(&buffer[..end])),
        Some(_) => Err(SocksError::Malformed),
        None if buffer.len() > MAX_FIELD => Err(SocksError::Malformed),
        None => Ok(None),
    }
}

fn host_name(bytes: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(bytes).ok()?;
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_string())
}

/// The SOCKS5 request that follows the method exchange:
/// `VER CMD RSV ATYP ADDRESS PORT(2)`.
pub(crate) fn parse_v5_request(buffer: &[u8]) -> Parsed<SocksTarget> {
    if let Some(version) = buffer.first()
        && *version != 0x05
    {
        return Parsed::Invalid(SocksError::Version);
    }
    if let Some(command) = buffer.get(1)
        && *command != CONNECT
    {
        return Parsed::Invalid(SocksError::Command);
    }
    let Some(address_type) = buffer.get(3) else {
        return Parsed::Incomplete;
    };
    let (host, address_end) = match address_type {
        0x01 => match buffer.get(4..8) {
            Some(bytes) => {
                let address: [u8; 4] = bytes.try_into().expect("four bytes");
                (Ipv4Addr::from(address).to_string(), 8)
            }
            None => return Parsed::Incomplete,
        },
        0x03 => {
            let Some(length) = buffer.get(4).map(|length| *length as usize) else {
                return Parsed::Incomplete;
            };
            match buffer.get(5..5 + length) {
                Some(name) => match host_name(name) {
                    Some(host) => (host, 5 + length),
                    None => return Parsed::Invalid(SocksError::Malformed),
                },
                None => return Parsed::Incomplete,
            }
        }
        0x04 => match buffer.get(4..20) {
            Some(bytes) => {
                let address: [u8; 16] = bytes.try_into().expect("sixteen bytes");
                (Ipv6Addr::from(address).to_string(), 20)
            }
            None => return Parsed::Incomplete,
        },
        _ => return Parsed::Invalid(SocksError::AddressType),
    };
    match buffer.get(address_end..address_end + 2) {
        Some(port) => Parsed::Done {
            value: SocksTarget {
                host,
                port: u16::from_be_bytes([port[0], port[1]]),
            },
            consumed: address_end + 2,
        },
        None => Parsed::Incomplete,
    }
}

/// The answer to a SOCKS5 greeting: no authentication, or no method at all.
pub(crate) fn v5_method_reply(accepted: bool) -> [u8; 2] {
    [0x05, if accepted { 0x00 } else { 0xFF }]
}

/// The answer to a SOCKS5 request. The bound address is reported as
/// `0.0.0.0:0`, as OpenSSH does: the client has no use for it on `CONNECT`.
pub(crate) fn v5_reply(code: SocksReply) -> [u8; 10] {
    [0x05, code as u8, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
}

/// The answer to a SOCKS4 request: granted (`0x5A`) or rejected (`0x5B`).
pub(crate) fn v4_reply(granted: bool) -> [u8; 8] {
    [0x00, if granted { 0x5A } else { 0x5B }, 0, 0, 0, 0, 0, 0]
}

/// A request the proxy understood, waiting for the connection to be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Negotiated {
    pub target: SocksTarget,
    pub version: SocksVersion,
    /// Bytes the client sent after its request without waiting for the
    /// answer. They belong to the connection and go to the target first.
    pub leftover: Vec<u8>,
}

/// More than any well-formed request needs: a SOCKS5 request tops out at
/// 262 bytes and a SOCKS4a one at 520.
const MAX_REQUEST: usize = 1024;

/// Read a client's request up to the point where the proxy has to connect.
/// A request that is refused has already been answered when this returns.
pub(crate) async fn negotiate<S>(stream: &mut S) -> Result<Negotiated, SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buffer = Vec::new();
    let greeting = match read_message(stream, &mut buffer, parse_greeting).await {
        Ok(greeting) => greeting,
        Err(error) => {
            // Only a SOCKS4 client can be told; a SOCKS5 one has not said
            // enough yet, and anything else is not SOCKS.
            if buffer.first() == Some(&0x04) && error != SocksError::Closed {
                let _ = stream.write_all(&v4_reply(false)).await;
            }
            return Err(error);
        }
    };
    let offers_no_auth = match greeting {
        Greeting::V4(target) => {
            return Ok(Negotiated {
                target,
                version: SocksVersion::V4,
                leftover: buffer,
            });
        }
        Greeting::V5 { offers_no_auth } => offers_no_auth,
    };
    write(stream, &v5_method_reply(offers_no_auth)).await?;
    if !offers_no_auth {
        return Err(SocksError::NoAcceptableMethod);
    }
    match read_message(stream, &mut buffer, parse_v5_request).await {
        Ok(target) => Ok(Negotiated {
            target,
            version: SocksVersion::V5,
            leftover: buffer,
        }),
        Err(error) => {
            let code = match error {
                SocksError::Command => Some(SocksReply::CommandNotSupported),
                SocksError::AddressType => Some(SocksReply::AddressTypeNotSupported),
                SocksError::Closed => None,
                _ => Some(SocksReply::GeneralFailure),
            };
            if let Some(code) = code {
                let _ = stream.write_all(&v5_reply(code)).await;
            }
            Err(error)
        }
    }
}

/// Tell the client how connecting went, in the protocol it spoke.
pub(crate) async fn reply<S>(
    stream: &mut S,
    version: SocksVersion,
    code: SocksReply,
) -> Result<(), SocksError>
where
    S: AsyncWrite + Unpin,
{
    match version {
        SocksVersion::V5 => write(stream, &v5_reply(code)).await,
        SocksVersion::V4 => write(stream, &v4_reply(code == SocksReply::Succeeded)).await,
    }
}

async fn write<S: AsyncWrite + Unpin>(stream: &mut S, bytes: &[u8]) -> Result<(), SocksError> {
    stream
        .write_all(bytes)
        .await
        .map_err(|_| SocksError::Closed)
}

/// Read until `parse` has a whole message, leaving what follows it in
/// `buffer`.
async fn read_message<S, T>(
    stream: &mut S,
    buffer: &mut Vec<u8>,
    parse: impl Fn(&[u8]) -> Parsed<T>,
) -> Result<T, SocksError>
where
    S: AsyncRead + Unpin,
{
    loop {
        match parse(buffer) {
            Parsed::Done { value, consumed } => {
                buffer.drain(..consumed);
                return Ok(value);
            }
            Parsed::Invalid(error) => return Err(error),
            Parsed::Incomplete if buffer.len() >= MAX_REQUEST => {
                return Err(SocksError::Malformed);
            }
            Parsed::Incomplete => {}
        }
        let mut chunk = [0_u8; 512];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return Err(SocksError::Closed),
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(host: &str, port: u16) -> SocksTarget {
        SocksTarget {
            host: host.into(),
            port,
        }
    }

    /// Every strict prefix of a message must ask for more, never misread.
    fn assert_prefixes_incomplete<T: std::fmt::Debug + PartialEq>(
        message: &[u8],
        parse: impl Fn(&[u8]) -> Parsed<T>,
    ) {
        for end in 0..message.len() {
            assert_eq!(parse(&message[..end]), Parsed::Incomplete, "prefix {end}");
        }
    }

    #[test]
    fn a_socks5_greeting_lists_its_methods() {
        let greeting = [0x05, 0x02, 0x00, 0x02];
        assert_prefixes_incomplete(&greeting, parse_greeting);
        assert_eq!(
            parse_greeting(&greeting),
            Parsed::Done {
                value: Greeting::V5 {
                    offers_no_auth: true
                },
                consumed: 4
            }
        );
        // Username/password only: there is nothing this proxy can accept.
        assert_eq!(
            parse_greeting(&[0x05, 0x01, 0x02]),
            Parsed::Done {
                value: Greeting::V5 {
                    offers_no_auth: false
                },
                consumed: 3
            }
        );
        assert_eq!(v5_method_reply(true), [0x05, 0x00]);
        assert_eq!(v5_method_reply(false), [0x05, 0xFF]);
    }

    #[test]
    fn anything_but_socks_is_refused_at_the_first_byte() {
        assert_eq!(
            parse_greeting(b"GET / HTTP/1.1\r\n"),
            Parsed::Invalid(SocksError::Version)
        );
    }

    #[test]
    fn a_socks5_request_names_an_address_of_each_kind() {
        let ipv4 = [0x05, 0x01, 0x00, 0x01, 10, 0, 0, 7, 0x1F, 0x90];
        assert_prefixes_incomplete(&ipv4, parse_v5_request);
        assert_eq!(
            parse_v5_request(&ipv4),
            Parsed::Done {
                value: target("10.0.0.7", 8080),
                consumed: 10
            }
        );

        let mut domain = vec![0x05, 0x01, 0x00, 0x03, 11];
        domain.extend_from_slice(b"example.com");
        domain.extend_from_slice(&443_u16.to_be_bytes());
        assert_prefixes_incomplete(&domain, parse_v5_request);
        assert_eq!(
            parse_v5_request(&domain),
            Parsed::Done {
                value: target("example.com", 443),
                consumed: domain.len()
            }
        );

        let mut ipv6 = vec![0x05, 0x01, 0x00, 0x04];
        ipv6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        ipv6.extend_from_slice(&22_u16.to_be_bytes());
        assert_prefixes_incomplete(&ipv6, parse_v5_request);
        assert_eq!(
            parse_v5_request(&ipv6),
            Parsed::Done {
                value: target("::1", 22),
                consumed: 22
            }
        );

        // What follows the request is left for the connection.
        let mut pipelined = ipv4.to_vec();
        pipelined.extend_from_slice(b"hello");
        assert_eq!(
            parse_v5_request(&pipelined),
            Parsed::Done {
                value: target("10.0.0.7", 8080),
                consumed: 10
            }
        );
    }

    #[test]
    fn a_socks5_request_it_cannot_carry_is_named() {
        // BIND and UDP ASSOCIATE.
        for command in [0x02, 0x03] {
            assert_eq!(
                parse_v5_request(&[0x05, command, 0x00, 0x01, 1, 2, 3, 4, 0, 80]),
                Parsed::Invalid(SocksError::Command)
            );
        }
        assert_eq!(
            parse_v5_request(&[0x05, 0x01, 0x00, 0x09, 1, 2, 3, 4, 0, 80]),
            Parsed::Invalid(SocksError::AddressType)
        );
        assert_eq!(
            parse_v5_request(&[0x04, 0x01, 0x00, 0x01]),
            Parsed::Invalid(SocksError::Version)
        );
        // A name of no characters.
        assert_eq!(
            parse_v5_request(&[0x05, 0x01, 0x00, 0x03, 0, 0, 80]),
            Parsed::Invalid(SocksError::Malformed)
        );
        assert_eq!(
            parse_v5_request(&[0x05, 0x01, 0x00, 0x03, 2, 0xFF, 0xFE, 0, 80]),
            Parsed::Invalid(SocksError::Malformed)
        );
    }

    #[test]
    fn a_socks4_request_carries_an_address_and_a_user_id() {
        let mut request = vec![0x04, 0x01, 0x00, 0x50, 192, 168, 1, 9];
        request.extend_from_slice(b"xuz\0");
        assert_prefixes_incomplete(&request, parse_greeting);
        assert_eq!(
            parse_greeting(&request),
            Parsed::Done {
                value: Greeting::V4(target("192.168.1.9", 80)),
                consumed: request.len()
            }
        );
        // No user id at all is the common case.
        assert_eq!(
            parse_greeting(&[0x04, 0x01, 0x00, 0x50, 192, 168, 1, 9, 0]),
            Parsed::Done {
                value: Greeting::V4(target("192.168.1.9", 80)),
                consumed: 9
            }
        );
    }

    #[test]
    fn a_socks4a_request_carries_a_name_after_the_user_id() {
        let mut request = vec![0x04, 0x01, 0x01, 0xBB, 0, 0, 0, 1];
        request.extend_from_slice(b"me\0");
        request.extend_from_slice(b"example.com\0");
        assert_prefixes_incomplete(&request, parse_greeting);
        assert_eq!(
            parse_greeting(&request),
            Parsed::Done {
                value: Greeting::V4(target("example.com", 443)),
                consumed: request.len()
            }
        );
        // `0.0.0.0` is a plain address, not the 4a marker.
        assert_eq!(
            parse_greeting(&[0x04, 0x01, 0x00, 0x50, 0, 0, 0, 0, 0]),
            Parsed::Done {
                value: Greeting::V4(target("0.0.0.0", 80)),
                consumed: 9
            }
        );
    }

    #[test]
    fn a_socks4_request_it_cannot_carry_is_refused() {
        // BIND.
        assert_eq!(
            parse_greeting(&[0x04, 0x02, 0x00, 0x50, 1, 2, 3, 4, 0]),
            Parsed::Invalid(SocksError::Command)
        );
        // A user id that never ends.
        let mut endless = vec![0x04, 0x01, 0x00, 0x50, 1, 2, 3, 4];
        endless.extend(std::iter::repeat_n(b'a', MAX_FIELD + 1));
        assert_eq!(
            parse_greeting(&endless),
            Parsed::Invalid(SocksError::Malformed)
        );
        // A 4a request with an empty name.
        assert_eq!(
            parse_greeting(&[0x04, 0x01, 0x00, 0x50, 0, 0, 0, 1, 0, 0]),
            Parsed::Invalid(SocksError::Malformed)
        );
    }

    #[test]
    fn replies_are_spelled_as_the_protocols_say() {
        assert_eq!(
            v5_reply(SocksReply::Succeeded),
            [0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(v5_reply(SocksReply::ConnectionRefused)[1], 0x05);
        assert_eq!(v5_reply(SocksReply::NotAllowed)[1], 0x02);
        assert_eq!(v5_reply(SocksReply::CommandNotSupported)[1], 0x07);
        assert_eq!(v5_reply(SocksReply::AddressTypeNotSupported)[1], 0x08);
        assert_eq!(v4_reply(true)[..2], [0x00, 0x5A]);
        assert_eq!(v4_reply(false)[..2], [0x00, 0x5B]);
    }

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn negotiating_socks5_answers_the_greeting_and_keeps_pipelined_bytes() {
        block_on(async {
            let (mut client, mut proxy) = tokio::io::duplex(1024);
            let mut request = vec![0x05, 0x01, 0x00];
            request.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, 2]);
            request.extend_from_slice(b"db");
            request.extend_from_slice(&5432_u16.to_be_bytes());
            request.extend_from_slice(b"early");
            client.write_all(&request).await.unwrap();

            let negotiated = negotiate(&mut proxy).await.unwrap();
            assert_eq!(negotiated.target, target("db", 5432));
            assert_eq!(negotiated.version, SocksVersion::V5);
            assert_eq!(negotiated.leftover, b"early");

            let mut method = [0_u8; 2];
            client.read_exact(&mut method).await.unwrap();
            assert_eq!(method, [0x05, 0x00]);

            reply(&mut proxy, SocksVersion::V5, SocksReply::Succeeded)
                .await
                .unwrap();
            let mut answer = [0_u8; 10];
            client.read_exact(&mut answer).await.unwrap();
            assert_eq!(answer, v5_reply(SocksReply::Succeeded));
        });
    }

    #[test]
    fn negotiating_tells_a_client_why_it_was_refused() {
        block_on(async {
            // Authentication required by the client.
            let (mut client, mut proxy) = tokio::io::duplex(1024);
            client.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
            assert_eq!(
                negotiate(&mut proxy).await,
                Err(SocksError::NoAcceptableMethod)
            );
            let mut method = [0_u8; 2];
            client.read_exact(&mut method).await.unwrap();
            assert_eq!(method, [0x05, 0xFF]);

            // UDP ASSOCIATE.
            let (mut client, mut proxy) = tokio::io::duplex(1024);
            client
                .write_all(&[0x05, 0x01, 0x00, 0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            assert_eq!(negotiate(&mut proxy).await, Err(SocksError::Command));
            let mut answer = [0_u8; 12];
            client.read_exact(&mut answer).await.unwrap();
            assert_eq!(answer[..2], [0x05, 0x00]);
            assert_eq!(answer[2..], v5_reply(SocksReply::CommandNotSupported));

            // SOCKS4 BIND.
            let (mut client, mut proxy) = tokio::io::duplex(1024);
            client
                .write_all(&[0x04, 0x02, 0x00, 0x50, 1, 2, 3, 4, 0])
                .await
                .unwrap();
            assert_eq!(negotiate(&mut proxy).await, Err(SocksError::Command));
            let mut answer = [0_u8; 8];
            client.read_exact(&mut answer).await.unwrap();
            assert_eq!(answer, v4_reply(false));

            // A client that hangs up mid-request is not answered at all.
            let (mut client, mut proxy) = tokio::io::duplex(1024);
            client.write_all(&[0x05]).await.unwrap();
            drop(client);
            assert_eq!(negotiate(&mut proxy).await, Err(SocksError::Closed));
        });
    }

    #[test]
    fn negotiating_socks4a_reads_the_name() {
        block_on(async {
            let (mut client, mut proxy) = tokio::io::duplex(1024);
            let mut request = vec![0x04, 0x01, 0x00, 0x50, 0, 0, 0, 1, 0];
            request.extend_from_slice(b"intranet\0");
            client.write_all(&request).await.unwrap();
            let negotiated = negotiate(&mut proxy).await.unwrap();
            assert_eq!(negotiated.target, target("intranet", 80));
            assert_eq!(negotiated.version, SocksVersion::V4);
            assert!(negotiated.leftover.is_empty());

            reply(&mut proxy, SocksVersion::V4, SocksReply::ConnectionRefused)
                .await
                .unwrap();
            let mut answer = [0_u8; 8];
            client.read_exact(&mut answer).await.unwrap();
            assert_eq!(answer, v4_reply(false));
        });
    }
}
