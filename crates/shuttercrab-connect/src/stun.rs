//! The address the internet sees a UDP socket at, from a STUN server
//! (RFC 8489): the router's public address and the port it gave this
//! socket. Only a binding request, and only IPv4 for now.

use anyhow::{Context, Result, bail};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket},
    time::{Duration, Instant},
};

/// Cloudflare's, as the meeting point is there too.
pub const SERVER: &str = "stun.cloudflare.com:3478";

const MAGIC: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const MAPPED_ADDRESS: u16 = 0x0001;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Ask `server` from `socket`, trying three times. Other packets that
/// arrive meanwhile are dropped.
pub fn public_address(socket: &UdpSocket, server: &str) -> Result<SocketAddr> {
    let server = server
        .to_socket_addrs()
        .with_context(|| format!("could not look up {server}"))?
        .find(SocketAddr::is_ipv4)
        .with_context(|| format!("{server} has no IPv4 address"))?;
    let id: [u8; 12] = rand::random();
    let mut request = Vec::with_capacity(20);
    request.extend(BINDING_REQUEST.to_be_bytes());
    request.extend(0u16.to_be_bytes());
    request.extend(MAGIC.to_be_bytes());
    request.extend(id);

    let previous = socket.read_timeout()?;
    let mut buffer = [0; 1500];
    let found = (|| {
        for _ in 0..3 {
            socket.send_to(&request, server)?;
            let deadline = Instant::now() + Duration::from_millis(700);
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                socket.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
                let Ok((n, from)) = socket.recv_from(&mut buffer) else {
                    break;
                };
                if from == server
                    && let Some(address) = parse_response(&buffer[..n], &id)
                {
                    return Ok(address);
                }
            }
        }
        bail!("no answer from the STUN server {server}")
    })();
    socket.set_read_timeout(previous)?;
    found
}

/// The address in a binding success response to request `id`.
fn parse_response(packet: &[u8], id: &[u8; 12]) -> Option<SocketAddr> {
    let u16_at = |i: usize| Some(u16::from_be_bytes(packet.get(i..i + 2)?.try_into().ok()?));
    if packet.len() < 20
        || u16_at(0)? != BINDING_SUCCESS
        || packet[4..8] != MAGIC.to_be_bytes()
        || &packet[8..20] != id
    {
        return None;
    }
    let end = (20 + usize::from(u16_at(2)?)).min(packet.len());
    let mut at = 20;
    let mut mapped = None;
    while at + 4 <= end {
        let (kind, len) = (u16_at(at)?, usize::from(u16_at(at + 2)?));
        let value = packet.get(at + 4..at + 4 + len)?;
        // Family 1 is IPv4: a reserved byte, the family, the port, the address.
        if value.len() >= 8 && value[1] == 1 {
            let port = u16::from_be_bytes([value[2], value[3]]);
            let ip = u32::from_be_bytes(value[4..8].try_into().ok()?);
            match kind {
                XOR_MAPPED_ADDRESS => {
                    let ip = Ipv4Addr::from(ip ^ MAGIC);
                    return Some(SocketAddr::new(IpAddr::V4(ip), port ^ (MAGIC >> 16) as u16));
                }
                MAPPED_ADDRESS => mapped = Some(SocketAddr::new(Ipv4Addr::from(ip).into(), port)),
                _ => {}
            }
        }
        // Attributes are padded to four bytes.
        at += 4 + len.div_ceil(4) * 4;
    }
    mapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_xor_mapped_address_is_read_from_a_response() {
        // 203.0.113.7:41000, as RFC 8489 encodes it.
        let id = [7; 12];
        let mut packet = Vec::new();
        packet.extend(BINDING_SUCCESS.to_be_bytes());
        packet.extend(12u16.to_be_bytes());
        packet.extend(MAGIC.to_be_bytes());
        packet.extend(id);
        packet.extend(XOR_MAPPED_ADDRESS.to_be_bytes());
        packet.extend(8u16.to_be_bytes());
        packet.extend([0, 1]);
        packet.extend((41000 ^ (MAGIC >> 16) as u16).to_be_bytes());
        packet.extend((u32::from(Ipv4Addr::new(203, 0, 113, 7)) ^ MAGIC).to_be_bytes());
        assert_eq!(
            parse_response(&packet, &id),
            Some("203.0.113.7:41000".parse().unwrap())
        );
        assert_eq!(parse_response(&packet, &[8; 12]), None);
    }
}
