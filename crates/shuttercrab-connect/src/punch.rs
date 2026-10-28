//! Opening a way through both routers: each app sends small packets from
//! its socket to every address the other gave. A router lets in packets
//! from an address its side has just sent to, so once both have sent,
//! each one's packets get through to the other. The address the other's
//! packets come from is the one that works.

use anyhow::{Result, bail};
use std::{
    net::{SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

const MAGIC: &[u8; 8] = b"SCPUNCH1";
/// How often to send, and how long to keep trying.
const EVERY: Duration = Duration::from_millis(50);
const GIVE_UP: Duration = Duration::from_secs(10);
/// How long to go on once this side has heard the other but not been told
/// it was heard: its packets saying so may have been lost.
const GRACE: Duration = Duration::from_secs(1);

/// Send to `addresses` from `socket` until a packet of the same `room`
/// comes back and the other side says it has heard this one: the address
/// it came from. Other packets are dropped.
pub fn punch(socket: &UdpSocket, addresses: &[SocketAddr], room: &[u8; 16]) -> Result<SocketAddr> {
    if addresses.is_empty() {
        bail!("the other side gave no addresses");
    }
    let previous = socket.read_timeout()?;
    let found = exchange(socket, addresses, room);
    socket.set_read_timeout(previous)?;
    found
}

fn exchange(socket: &UdpSocket, addresses: &[SocketAddr], room: &[u8; 16]) -> Result<SocketAddr> {
    let started = Instant::now();
    let mut heard: Option<(SocketAddr, Instant)> = None;
    let mut heard_back = false;
    let mut buffer = [0; 64];
    while started.elapsed() < GIVE_UP {
        let packet = [&MAGIC[..], room, &[u8::from(heard.is_some())]].concat();
        match heard {
            // Once one address works, only it.
            Some((at, _)) => {
                socket.send_to(&packet, at)?;
            }
            None => {
                for address in addresses {
                    // An address may be unreachable from here, such as
                    // another network's private one.
                    let _ = socket.send_to(&packet, address);
                }
            }
        }
        if let Some((at, since)) = heard
            && (heard_back || since.elapsed() >= GRACE)
        {
            // A few more saying so, in case the last were lost.
            for _ in 0..3 {
                socket.send_to(&packet, at)?;
            }
            return Ok(at);
        }
        let until = Instant::now() + EVERY;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            socket.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
            let Ok((n, from)) = socket.recv_from(&mut buffer) else {
                break;
            };
            let packet = &buffer[..n];
            if n == MAGIC.len() + 17 && packet.starts_with(MAGIC) && &packet[8..24] == room {
                heard.get_or_insert((from, Instant::now()));
                heard_back |= packet[24] == 1;
            }
        }
    }
    bail!(
        "could not connect directly: no packets came back from {}",
        addresses
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_sockets_find_each_other() {
        let a = UdpSocket::bind("127.0.0.1:0").unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").unwrap();
        let (a_at, b_at) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let room = [9; 16];
        // One address that leads nowhere, as a private address of another
        // network would.
        let nowhere: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let other = std::thread::spawn(move || punch(&b, &[nowhere, a_at], &room).unwrap());
        assert_eq!(punch(&a, &[b_at], &room).unwrap(), b_at);
        assert_eq!(other.join().unwrap(), a_at);
    }
}
