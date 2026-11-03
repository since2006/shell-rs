//! IPv4 addresses on an IPv6-only network, as Apple's systems reach them.
//!
//! An IPv6-only network with NAT64 reaches the IPv4 internet through IPv6
//! addresses that embed the IPv4 ones. Apple's `getaddrinfo` makes them up
//! for an IPv4 address it is handed as a name, and App Review tests apps on
//! such a network; std takes an address as it is written and never asks.

use std::ffi::CString;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

/// The addresses that reach `ip`: on a network with NAT64 the synthesized
/// IPv6 one, elsewhere `ip` itself. The address as written when the
/// resolver gives nothing.
pub(crate) async fn addresses(ip: Ipv4Addr, port: u16) -> Vec<SocketAddr> {
    let resolved = tokio::task::spawn_blocking(move || resolve(ip, port))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    if resolved.is_empty() {
        vec![SocketAddr::V4(SocketAddrV4::new(ip, port))]
    } else {
        resolved
    }
}

/// `getaddrinfo` with the flags Apple says to use for this, which let it
/// synthesize. Blocking.
fn resolve(ip: Ipv4Addr, port: u16) -> io::Result<Vec<SocketAddr>> {
    let node = CString::new(ip.to_string()).expect("an address has no NUL");
    let service = CString::new(port.to_string()).expect("a number has no NUL");
    // SAFETY: an all-zero `addrinfo` is the empty hints C code starts from.
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;
    hints.ai_flags = libc::AI_DEFAULT;
    let mut list: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: the strings and hints outlive the call, which fills `list`.
    let status = unsafe { libc::getaddrinfo(node.as_ptr(), service.as_ptr(), &hints, &mut list) };
    if status != 0 {
        return Err(io::Error::other(format!("getaddrinfo failed ({status})")));
    }
    let mut addresses = Vec::new();
    let mut entry = list;
    while !entry.is_null() {
        // SAFETY: `entry` is a node of the list `getaddrinfo` returned,
        // freed only below, and `ai_addr` holds the address `ai_family`
        // says.
        let info = unsafe { &*entry };
        match info.ai_family {
            libc::AF_INET => {
                let address = unsafe { &*(info.ai_addr as *const libc::sockaddr_in) };
                let ip = Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr));
                addresses.push(SocketAddr::V4(SocketAddrV4::new(ip, port)));
            }
            libc::AF_INET6 => {
                let address = unsafe { &*(info.ai_addr as *const libc::sockaddr_in6) };
                let ip = Ipv6Addr::from(address.sin6_addr.s6_addr);
                addresses.push(SocketAddr::V6(SocketAddrV6::new(
                    ip,
                    port,
                    address.sin6_flowinfo,
                    address.sin6_scope_id,
                )));
            }
            _ => {}
        }
        entry = info.ai_next;
    }
    // SAFETY: the list `getaddrinfo` returned, freed once.
    unsafe { libc::freeaddrinfo(list) };
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    /// No NAT64 here: an address comes back as it is.
    #[tokio::test]
    async fn an_address_without_nat64_is_reached_as_it_is() {
        let addresses = super::addresses(Ipv4Addr::LOCALHOST, 2222).await;
        assert!(
            addresses.contains(&SocketAddr::from((Ipv4Addr::LOCALHOST, 2222))),
            "{addresses:?}"
        );
    }
}
