//! A TURN relay (RFC 8656) under the QUIC connection: each app gets an
//! address on Cloudflare's relay and connects to the other's, so neither
//! learns where the other is, and networks that let nothing in, behind a
//! VPN among them, still connect. The relay passes on QUIC's packets,
//! encrypted end to end, and cannot read them.
//!
//! The setup (allocate, bind the other app to a channel) is blocking, on
//! a plain socket; the relay then becomes the socket QUIC sends on, and
//! keeps itself alive while it is.

use anyhow::{Context, Result, bail, ensure};
use md5::{Digest, Md5};
use quinn::{
    AsyncUdpSocket, UdpPoller,
    udp::{RecvMeta, Transmit},
};
use std::{
    fmt,
    io::{self, IoSliceMut},
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context as TaskContext, Poll, ready},
    time::Duration,
};
use tokio::io::ReadBuf;

const MAGIC: u32 = 0x2112_A442;

// Methods; a success response adds 0x0100, an error 0x0110.
const ALLOCATE: u16 = 0x0003;
const REFRESH: u16 = 0x0004;
const CHANNEL_BIND: u16 = 0x0009;
const SUCCESS: u16 = 0x0100;
const ERROR: u16 = 0x0110;

// Attributes.
const CHANNEL_NUMBER: u16 = 0x000C;
const LIFETIME: u16 = 0x000D;
const USERNAME: u16 = 0x0006;
const MESSAGE_INTEGRITY: u16 = 0x0008;
const ERROR_CODE: u16 = 0x0009;
const XOR_PEER_ADDRESS: u16 = 0x0012;
const REALM: u16 = 0x0014;
const NONCE: u16 = 0x0015;
const XOR_RELAYED_ADDRESS: u16 = 0x0016;
const REQUESTED_TRANSPORT: u16 = 0x0019;

/// The one channel: one other app for now.
const CHANNEL: u16 = 0x4000;
/// How long the relay keeps the allocation without a refresh, and how
/// often it is refreshed (a channel lasts 10 minutes, its permission 5).
const ALLOCATION_LIFETIME: u32 = 600;
const KEEP_ALIVE: Duration = Duration::from_secs(120);

/// A login to a relay, as the room gives it.
#[derive(Clone)]
pub struct Login {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Login {{ urls: {:?}, … }}", self.urls)
    }
}

impl Login {
    /// The relay's UDP address: its first `turn:` URL over UDP.
    fn server(&self) -> Result<SocketAddr> {
        let url = self
            .urls
            .iter()
            .find(|u| u.starts_with("turn:") && !u.contains("transport=tcp"))
            .context("the relay offers no UDP address")?;
        let host = url["turn:".len()..].split('?').next().unwrap_or_default();
        host.to_socket_addrs()
            .with_context(|| format!("could not look up {host}"))?
            .find(SocketAddr::is_ipv4)
            .with_context(|| format!("{host} has no IPv4 address"))
    }
}

/// What every request after the first carries: who, the relay's realm and
/// nonce, and the key its integrity is checked with.
#[derive(Clone)]
struct Auth {
    username: String,
    realm: String,
    nonce: String,
    key: [u8; 16],
}

/// An address on the relay, being set up.
pub struct Allocation {
    socket: UdpSocket,
    server: SocketAddr,
    auth: Auth,
    relayed: SocketAddr,
}

impl Allocation {
    /// Ask the relay for an address, from a new socket.
    pub fn new(login: &Login) -> Result<Self> {
        let server = login.server()?;
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        let transport = [17, 0, 0, 0];
        // The first ask is refused, with the realm and nonce to log in with.
        let first = transact(
            &socket,
            server,
            ALLOCATE,
            &[(REQUESTED_TRANSPORT, &transport)],
            None,
        )?;
        let refusal = first
            .error()
            .context("the relay allocated without a login")?;
        ensure!(refusal == 401, "the relay refused: error {refusal}");
        let realm = first.text(REALM).context("the relay gave no realm")?;
        let mut auth = Auth {
            key: Md5::digest(format!("{}:{realm}:{}", login.username, login.credential)).into(),
            username: login.username.clone(),
            realm,
            nonce: first.text(NONCE).context("the relay gave no nonce")?,
        };
        let lifetime = ALLOCATION_LIFETIME.to_be_bytes();
        let attributes = [
            (REQUESTED_TRANSPORT, &transport[..]),
            (LIFETIME, &lifetime[..]),
        ];
        let answer = transact_with(&socket, server, ALLOCATE, &attributes, &mut auth)?;
        let relayed = answer
            .xor_address(XOR_RELAYED_ADDRESS)
            .context("the relay gave no address")?;
        Ok(Self {
            socket,
            server,
            auth,
            relayed,
        })
    }

    /// Where the other app sends to reach this one.
    pub fn relayed(&self) -> SocketAddr {
        self.relayed
    }

    /// Let `peer`, the other app's relay address, through, on a channel,
    /// and become the socket QUIC uses to reach it. Within the runtime.
    pub fn connect(mut self, peer: SocketAddr) -> Result<Arc<Relay>> {
        bind(&self.socket, self.server, peer, &mut self.auth)?;
        self.socket.set_read_timeout(None)?;
        self.socket.set_nonblocking(true)?;
        let relay = Arc::new(Relay {
            io: Arc::new(tokio::net::UdpSocket::from_std(self.socket)?),
            server: self.server,
            peer,
            auth: Mutex::new(self.auth),
        });
        tokio::spawn(keep_alive(Arc::downgrade(&relay)));
        Ok(relay)
    }
}

/// The relay as QUIC's socket, to one other app.
pub struct Relay {
    io: Arc<tokio::net::UdpSocket>,
    server: SocketAddr,
    peer: SocketAddr,
    auth: Mutex<Auth>,
}

impl fmt::Debug for Relay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Relay {{ server: {}, peer: {} }}",
            self.server, self.peer
        )
    }
}

impl Relay {
    /// A request, signed with the latest nonce, sent without waiting: the
    /// answer arrives among QUIC's packets and is read there.
    fn send_request(&self, method: u16, attributes: &[(u16, &[u8])]) {
        let auth = self.auth.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let request = encode(method, &rand::random(), attributes, Some(&auth));
        let _ = self.io.try_send_to(&request, self.server);
    }

    /// An answer from the relay itself: a new nonce when the old one went
    /// stale, so the next request uses it; any other refusal logged.
    fn answer(&self, packet: &[u8]) {
        let Some(message) = Message::parse(packet) else {
            return;
        };
        match message.error() {
            Some(438) => {
                if let Some(nonce) = message.text(NONCE) {
                    self.auth.lock().unwrap_or_else(|e| e.into_inner()).nonce = nonce;
                    self.refresh();
                }
            }
            Some(code) => log::warn!("the relay refused a refresh: error {code}"),
            None => {}
        }
    }

    /// Keep the allocation, and the channel and its permission, alive.
    fn refresh(&self) {
        self.send_request(REFRESH, &[(LIFETIME, &ALLOCATION_LIFETIME.to_be_bytes())]);
        let channel = channel_attributes(self.peer);
        self.send_request(CHANNEL_BIND, &borrowed(&channel));
    }
}

impl Drop for Relay {
    /// Give the address back rather than leave it to lapse.
    fn drop(&mut self) {
        self.send_request(REFRESH, &[(LIFETIME, &0u32.to_be_bytes())]);
    }
}

async fn keep_alive(relay: std::sync::Weak<Relay>) {
    loop {
        tokio::time::sleep(KEEP_ALIVE).await;
        let Some(relay) = relay.upgrade() else {
            return;
        };
        relay.refresh();
    }
}

impl AsyncUdpSocket for Relay {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(Writable(self.io.clone()))
    }

    /// Each of QUIC's packets to the other app goes to the relay as
    /// channel data: the channel, the length, the packet.
    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        if transmit.destination != self.peer {
            return Ok(());
        }
        let length = u16::try_from(transmit.contents.len()).map_err(io::Error::other)?;
        let mut packet = Vec::with_capacity(4 + transmit.contents.len());
        packet.extend(CHANNEL.to_be_bytes());
        packet.extend(length.to_be_bytes());
        packet.extend(transmit.contents);
        self.io.try_send_to(&packet, self.server).map(drop)
    }

    /// The other app's packets, out of the relay's channel data; the
    /// relay's own answers are read and kept from QUIC.
    fn poll_recv(
        &self,
        cx: &mut TaskContext,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let Some(buffer) = bufs.first_mut() else {
            return Poll::Ready(Ok(0));
        };
        loop {
            let mut read = ReadBuf::new(buffer);
            let from = ready!(self.io.poll_recv_from(cx, &mut read))?;
            let n = read.filled().len();
            if from != self.server || n < 4 {
                continue;
            }
            let data = &mut buffer[..n];
            if data[0] & 0xC0 == 0x40 {
                let channel = u16::from_be_bytes([data[0], data[1]]);
                let length = usize::from(u16::from_be_bytes([data[2], data[3]]));
                if channel != CHANNEL || 4 + length > n {
                    continue;
                }
                data.copy_within(4..4 + length, 0);
                meta[0] = RecvMeta {
                    addr: self.peer,
                    len: length,
                    stride: length,
                    ..RecvMeta::default()
                };
                return Poll::Ready(Ok(1));
            }
            self.answer(data);
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }
}

/// When the relay socket can be written to.
struct Writable(Arc<tokio::net::UdpSocket>);

impl fmt::Debug for Writable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Writable")
    }
}

impl UdpPoller for Writable {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut TaskContext) -> Poll<io::Result<()>> {
        self.0.poll_send_ready(cx)
    }
}

/// The channel and the address it is for, as a channel bind carries them.
fn channel_attributes(peer: SocketAddr) -> [(u16, Vec<u8>); 2] {
    let mut channel = CHANNEL.to_be_bytes().to_vec();
    channel.extend([0, 0]);
    [
        (CHANNEL_NUMBER, channel),
        (XOR_PEER_ADDRESS, xor_address(peer)),
    ]
}

fn borrowed(attributes: &[(u16, Vec<u8>)]) -> Vec<(u16, &[u8])> {
    attributes.iter().map(|(k, v)| (*k, &v[..])).collect()
}

/// Bind `peer` to the channel, which also lets its packets through.
fn bind(socket: &UdpSocket, server: SocketAddr, peer: SocketAddr, auth: &mut Auth) -> Result<()> {
    let attributes = channel_attributes(peer);
    transact_with(socket, server, CHANNEL_BIND, &borrowed(&attributes), auth)?;
    Ok(())
}

/// A signed request and its success, taking a fresh nonce once if the
/// relay says the last went stale.
fn transact_with(
    socket: &UdpSocket,
    server: SocketAddr,
    method: u16,
    attributes: &[(u16, &[u8])],
    auth: &mut Auth,
) -> Result<Message> {
    for _ in 0..2 {
        let answer = transact(socket, server, method, attributes, Some(auth))?;
        match answer.error() {
            None => return Ok(answer),
            Some(438) => {
                auth.nonce = answer
                    .text(NONCE)
                    .context("a stale nonce, but no new one")?;
            }
            Some(code) => bail!("the relay refused: error {code}"),
        }
    }
    bail!("the relay kept refusing the nonce")
}

/// Send a request until its answer comes, three tries.
fn transact(
    socket: &UdpSocket,
    server: SocketAddr,
    method: u16,
    attributes: &[(u16, &[u8])],
    auth: Option<&Auth>,
) -> Result<Message> {
    let id: [u8; 12] = rand::random();
    let request = encode(method, &id, attributes, auth);
    let mut buffer = [0; 1500];
    for _ in 0..3 {
        socket.send_to(&request, server)?;
        socket.set_read_timeout(Some(Duration::from_millis(800)))?;
        while let Ok((n, from)) = socket.recv_from(&mut buffer) {
            if from != server {
                continue;
            }
            if let Some(message) = Message::parse(&buffer[..n])
                && message.id == id
            {
                return Ok(message);
            }
        }
    }
    bail!("no answer from the relay at {server}")
}

/// A STUN message: the header, the attributes, then if `auth`, the login
/// and its integrity (an HMAC-SHA1 of all before it).
fn encode(method: u16, id: &[u8; 12], attributes: &[(u16, &[u8])], auth: Option<&Auth>) -> Vec<u8> {
    let mut body = Vec::new();
    let mut add = |kind: u16, value: &[u8]| {
        body.extend(kind.to_be_bytes());
        body.extend((value.len() as u16).to_be_bytes());
        body.extend(value);
        body.resize(body.len().div_ceil(4) * 4, 0);
    };
    for (kind, value) in attributes {
        add(*kind, value);
    }
    if let Some(auth) = auth {
        add(USERNAME, auth.username.as_bytes());
        add(NONCE, auth.nonce.as_bytes());
        add(REALM, auth.realm.as_bytes());
    }
    let header = |length: usize| {
        let mut header = Vec::with_capacity(20);
        header.extend(method.to_be_bytes());
        header.extend((length as u16).to_be_bytes());
        header.extend(MAGIC.to_be_bytes());
        header.extend(id);
        header
    };
    let Some(auth) = auth else {
        return [header(body.len()), body].concat();
    };
    // The length covers the integrity attribute, as if it were there.
    let signed = [header(body.len() + 24), body].concat();
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &auth.key);
    let tag = ring::hmac::sign(&key, &signed);
    let mut message = signed;
    message.extend(MESSAGE_INTEGRITY.to_be_bytes());
    message.extend(20u16.to_be_bytes());
    message.extend(tag.as_ref());
    message
}

/// An IPv4 address as an XOR-…-ADDRESS attribute carries it.
fn xor_address(address: SocketAddr) -> Vec<u8> {
    let IpAddr::V4(ip) = address.ip() else {
        unreachable!("the relay is reached over IPv4");
    };
    let mut value = vec![0, 1];
    value.extend((address.port() ^ (MAGIC >> 16) as u16).to_be_bytes());
    value.extend((u32::from(ip) ^ MAGIC).to_be_bytes());
    value
}

/// A STUN message from the relay.
struct Message {
    kind: u16,
    id: [u8; 12],
    attributes: Vec<(u16, Vec<u8>)>,
}

impl Message {
    fn parse(packet: &[u8]) -> Option<Self> {
        if packet.len() < 20 || packet[0] & 0xC0 != 0 || packet[4..8] != MAGIC.to_be_bytes() {
            return None;
        }
        let end = (20 + usize::from(u16::from_be_bytes([packet[2], packet[3]]))).min(packet.len());
        let mut attributes = Vec::new();
        let mut at = 20;
        while at + 4 <= end {
            let kind = u16::from_be_bytes([packet[at], packet[at + 1]]);
            let len = usize::from(u16::from_be_bytes([packet[at + 2], packet[at + 3]]));
            attributes.push((kind, packet.get(at + 4..at + 4 + len)?.to_vec()));
            at += 4 + len.div_ceil(4) * 4;
        }
        Some(Self {
            kind: u16::from_be_bytes([packet[0], packet[1]]),
            id: packet[8..20].try_into().ok()?,
            attributes,
        })
    }

    fn get(&self, kind: u16) -> Option<&[u8]> {
        self.attributes
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, v)| &v[..])
    }

    fn text(&self, kind: u16) -> Option<String> {
        Some(String::from_utf8_lossy(self.get(kind)?).into_owned())
    }

    /// The error code, if this is an error response.
    fn error(&self) -> Option<u16> {
        if self.kind & ERROR != ERROR {
            debug_assert!(self.kind & SUCCESS == SUCCESS);
            return None;
        }
        let value = self.get(ERROR_CODE)?;
        Some(u16::from(value.get(2)? & 7) * 100 + u16::from(*value.get(3)?))
    }

    fn xor_address(&self, kind: u16) -> Option<SocketAddr> {
        let value = self.get(kind)?;
        if value.len() < 8 || value[1] != 1 {
            return None;
        }
        let port = u16::from_be_bytes([value[2], value[3]]) ^ (MAGIC >> 16) as u16;
        let ip = u32::from_be_bytes(value[4..8].try_into().ok()?) ^ MAGIC;
        Some(SocketAddr::new(Ipv4Addr::from(ip).into(), port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 5769's sample request with long-term authentication, 2.4: its
    /// integrity, computed here, matches the one the RFC gives.
    #[test]
    fn a_signed_request_has_the_integrity_rfc_5769_gives() {
        let auth = Auth {
            username: "\u{30DE}\u{30C8}\u{30EA}\u{30C3}\u{30AF}\u{30B9}".into(),
            realm: "example.org".into(),
            nonce: "f//499k954d6OL34oL9FSTvy64sA".into(),
            key: Md5::digest(
                "\u{30DE}\u{30C8}\u{30EA}\u{30C3}\u{30AF}\u{30B9}:example.org:TheMatrIX",
            )
            .into(),
        };
        let id = [
            0x78, 0xad, 0x34, 0x33, 0xc6, 0xad, 0x72, 0xc0, 0x29, 0xda, 0x41, 0x2e,
        ];
        // A binding request: USERNAME, NONCE, REALM, then the integrity.
        let message = encode(0x0001, &id, &[], Some(&auth));
        assert_eq!(message.len(), 20 + 96);
        let integrity = &message[message.len() - 20..];
        assert_eq!(
            integrity,
            [
                0xf6, 0x70, 0x24, 0x65, 0x6d, 0xd6, 0x4a, 0x3e, 0x02, 0xb8, 0xe0, 0x71, 0x2e, 0x85,
                0xc9, 0xa2, 0x8c, 0xa8, 0x96, 0x66
            ]
        );
    }

    #[test]
    fn an_address_reads_back_as_it_was_written() {
        let address: SocketAddr = "203.0.113.7:41000".parse().unwrap();
        let mut packet = encode(
            SUCCESS | ALLOCATE,
            &[1; 12],
            &[(XOR_RELAYED_ADDRESS, &xor_address(address))],
            None,
        );
        packet[0] = 0x01;
        assert_eq!(
            Message::parse(&packet)
                .unwrap()
                .xor_address(XOR_RELAYED_ADDRESS),
            Some(address)
        );
    }
}
