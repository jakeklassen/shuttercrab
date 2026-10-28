//! Live streaming's connection (issue #66): a Shuttercrab that streams and
//! one that watches meet in a room on Cloudflare (`meet/`), swap the
//! addresses they can be reached at, and connect with QUIC, end to end
//! encrypted. By default each goes through Cloudflare's TURN relay, which
//! passes the packets on without reading them: neither learns where the
//! other is, and networks that let nothing in, behind a VPN among them,
//! still connect. Directly, they open a way through both routers instead.
//!
//! The streaming app [`host`]s and shares the [`Link`]; a viewer
//! [`join`]s with it. Each side then has a [`Connection`] with blocking
//! streams, for threads that are not async.

mod link;
mod punch;
mod quic;
mod room;
mod stun;
mod turn;

pub use link::Link;

use anyhow::{Context, Result, bail};
use quinn::AsyncUdpSocket;
use std::{
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::Arc,
    time::Duration,
};
use tokio::runtime::Runtime;

/// The meeting point Shuttercrab uses unless told otherwise.
pub const MEET: &str = "https://shuttercrab.sandbox.gdn";

/// How long a viewer waits for the streaming app to be in the room.
const WAIT_FOR_HOST: Duration = Duration::from_secs(10);
/// How long the room may take to answer, a refusal or a relay login.
const ROOM_ANSWERS: Duration = Duration::from_secs(5);
/// How long a connection may take once both are in the room.
const CONNECTING: Duration = Duration::from_secs(15);

/// How the two apps reach each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// Through Cloudflare's relay: always connects, and hides each one's
    /// address from the other.
    Relay,
    /// Straight between them, where both networks allow it.
    Direct,
}

/// This app's side of the way: its own socket, or its address on the relay.
enum Way {
    Direct(UdpSocket),
    Relay(turn::Allocation),
}

impl Way {
    /// Where to reach the other app at, from the addresses it gave: when
    /// direct, the one a way through the routers opened to.
    fn reach(&self, addresses: &[SocketAddr], room: &[u8; 16]) -> Result<SocketAddr> {
        match self {
            Self::Direct(socket) => punch::punch(socket, addresses, room),
            Self::Relay(_) => addresses
                .first()
                .copied()
                .context("the other app gave no relay address"),
        }
    }

    /// The socket QUIC reaches `at` through. Within the runtime.
    fn into_socket(self, at: SocketAddr) -> Result<Arc<dyn AsyncUdpSocket>> {
        match self {
            Self::Direct(socket) => quic::plain(socket),
            Self::Relay(allocation) => Ok(allocation.connect(at)?),
        }
    }
}

/// Join room `name` at `meet`, as its host with `host_key` or a viewer,
/// and say where this app can be reached by `path`.
fn enter(meet: &str, name: &str, host_key: Option<&str>, path: Path) -> Result<(room::Room, Way)> {
    match path {
        Path::Direct => {
            let (socket, addresses) = socket()?;
            let room = room::Room::join(meet, name, host_key, &addresses)?;
            log::info!("this app is at {addresses:?}");
            Ok((room, Way::Direct(socket)))
        }
        Path::Relay => {
            // Nowhere yet: the room answers with a relay login, or a refusal.
            let room = room::Room::join(meet, name, host_key, &[])?;
            let login = loop {
                match room.next(ROOM_ANSWERS) {
                    Ok(room::Event::Relay(login)) => break login,
                    Ok(room::Event::Closed(why)) => bail!("{why}"),
                    Ok(_) => {}
                    Err(_) => bail!("the meeting point offers no relay; try a direct connection"),
                }
            };
            let allocation = turn::Allocation::new(&login)?;
            log::info!("this app is at {} on the relay", allocation.relayed());
            room.hello(&[allocation.relayed()])?;
            Ok((room, Way::Relay(allocation)))
        }
    }
}

/// A room opened to stream from, waiting for a viewer.
pub struct Hosting {
    link: Link,
    identity: quic::Identity,
    room: room::Room,
    way: Way,
}

/// Open a room at `meet` with `host_key` to stream from, reached by `path`.
pub fn host(meet: &str, host_key: &str, path: Path) -> Result<Hosting> {
    let identity = quic::Identity::new()?;
    let link = Link {
        room: rand::random(),
        fingerprint: identity.fingerprint,
        token: rand::random(),
    };
    let (room, way) = enter(meet, &link.room_name(), Some(host_key), path)?;
    // A refusal comes as soon as the room reads this app's first hello.
    if let Way::Direct(_) = way
        && let Ok(room::Event::Closed(why)) = room.next(Duration::from_secs(2))
    {
        bail!("{why}");
    }
    Ok(Hosting {
        link,
        identity,
        room,
        way,
    })
}

impl Hosting {
    /// What to share: whoever has it can watch.
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// Wait for a viewer and connect to it. One viewer for now.
    pub fn accept(self) -> Result<Connection> {
        let viewer = loop {
            match self.room.next(Duration::from_secs(3600)) {
                Ok(room::Event::Peer(peer)) if !peer.host && !peer.addresses.is_empty() => {
                    break peer;
                }
                Ok(room::Event::Closed(why)) => bail!("{why}"),
                Ok(_) => {}
                Err(e) => return Err(e),
            }
        };
        let at = self.way.reach(&viewer.addresses, &self.link.room)?;
        log::info!("the viewer is at {at}");
        let relayed = matches!(self.way, Way::Relay(_));
        let runtime = runtime()?;
        let token = self.link.token;
        let (endpoint, connection) = runtime.block_on(async {
            let endpoint = quic::server(self.way.into_socket(at)?, &self.identity)?;
            let incoming = tokio::time::timeout(CONNECTING, endpoint.accept())
                .await
                .context("the viewer did not connect")?
                .context("the connection closed")?;
            let connection = incoming.await?;
            quic::check_token(&connection, &token).await?;
            anyhow::Ok((endpoint, connection))
        })?;
        Ok(Connection {
            runtime,
            connection,
            relayed,
            _endpoint: endpoint,
            _room: self.room,
        })
    }
}

/// Join the stream `link` is for, through `meet`, reached by `path`.
pub fn join(meet: &str, link: &Link, path: Path) -> Result<Connection> {
    let (room, way) = enter(meet, &link.room_name(), None, path)?;
    let host = room.wait_for_peer(true, WAIT_FOR_HOST)?;
    let at = way.reach(&host.addresses, &link.room)?;
    log::info!("the stream is at {at}");
    let relayed = matches!(way, Way::Relay(_));
    let runtime = runtime()?;
    let (endpoint, connection) = runtime.block_on(async {
        let endpoint = quic::client(way.into_socket(at)?, link.fingerprint)?;
        let connection = tokio::time::timeout(CONNECTING, quic::connect(&endpoint, at))
            .await
            .context("the stream did not answer")??;
        quic::show_token(&connection, &link.token).await?;
        anyhow::Ok((endpoint, connection))
    })?;
    Ok(Connection {
        runtime,
        connection,
        relayed,
        _endpoint: endpoint,
        _room: room,
    })
}
/// A UDP socket for the connection, and the addresses it can be reached
/// at: on this network, and as the internet sees it.
fn socket() -> Result<(UdpSocket, Vec<SocketAddr>)> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let port = socket.local_addr()?.port();
    let mut addresses = Vec::new();
    if let Some(ip) = local_ip() {
        addresses.push(SocketAddr::new(ip, port));
    }
    match stun::public_address(&socket, stun::SERVER) {
        Ok(public) if !addresses.contains(&public) => addresses.push(public),
        Ok(_) => {}
        Err(e) => log::warn!("{e:#}; only this network's address is known"),
    }
    if addresses.is_empty() {
        bail!("this PC has no network address to be reached at");
    }
    Ok((socket, addresses))
}

/// This PC's address on its network: the one it would reach the internet
/// from. Nothing is sent.
fn local_ip() -> Option<IpAddr> {
    let probe = UdpSocket::bind("0.0.0.0:0").ok()?;
    probe.connect("1.1.1.1:80").ok()?;
    Some(probe.local_addr().ok()?.ip())
}

fn runtime() -> Result<Arc<Runtime>> {
    Ok(Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("connection")
            .enable_all()
            .build()?,
    ))
}

/// A connection to the other app.
pub struct Connection {
    runtime: Arc<Runtime>,
    connection: quinn::Connection,
    relayed: bool,
    _endpoint: quinn::Endpoint,
    /// Kept open while connected: leaving the room ends this app's relay
    /// login, and for the streaming app, the stream.
    _room: room::Room,
}

impl Connection {
    /// Whether it goes through the relay.
    pub fn relayed(&self) -> bool {
        self.relayed
    }

    /// Where the other app is.
    pub fn remote(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    /// How long a packet takes there and back.
    pub fn round_trip(&self) -> Duration {
        self.connection.rtt()
    }

    /// Packets lost so far, and sent so far.
    pub fn lost(&self) -> (u64, u64) {
        let stats = self.connection.stats();
        (stats.path.lost_packets, stats.path.sent_packets)
    }

    /// A stream to the other app. Of several, one of higher `priority` is
    /// sent first, so a large write to another cannot hold it up.
    pub fn send(&self, priority: i32) -> Result<Sending> {
        let stream = self.runtime.block_on(self.connection.open_uni())?;
        stream.set_priority(priority)?;
        Ok(Sending {
            runtime: self.runtime.clone(),
            stream,
        })
    }

    /// The stream the other app sends.
    pub fn receive(&self) -> Result<Receiving> {
        Ok(Receiving {
            runtime: self.runtime.clone(),
            stream: self.runtime.block_on(self.connection.accept_uni())?,
        })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"done");
    }
}

/// A stream to the other app, written as it comes.
pub struct Sending {
    runtime: Arc<Runtime>,
    stream: quinn::SendStream,
}

impl Write for Sending {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.runtime
            .block_on(self.stream.write(buffer))
            .map_err(io::Error::other)
    }

    fn flush(&mut self) -> io::Result<()> {
        // What is written goes out as soon as it may.
        Ok(())
    }
}

/// The other app's stream.
pub struct Receiving {
    runtime: Arc<Runtime>,
    stream: quinn::RecvStream,
}

impl Read for Receiving {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        Ok(self
            .runtime
            .block_on(self.stream.read(buffer))
            .map_err(io::Error::other)?
            .unwrap_or(0))
    }
}
