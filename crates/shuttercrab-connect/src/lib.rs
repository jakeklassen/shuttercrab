//! Live streaming's connection (issue #66): a Shuttercrab that streams and
//! one that watches meet in a room on Cloudflare (`meet/`), swap their
//! addresses on Cloudflare's TURN relay, and connect with QUIC through it,
//! end to end encrypted. The relay passes the packets on without reading
//! them: neither learns where the other is, and networks that let nothing
//! in, behind a VPN among them, still connect.
//!
//! The streaming app [`host`]s and shares the [`Link`]; a viewer
//! [`join`]s with it. Each side then has a [`Connection`] with blocking
//! streams, for threads that are not async.

mod link;
mod quic;
mod room;
mod turn;

pub use link::Link;

use anyhow::{Context, Result, bail};
use std::{
    io::{self, Read, Write},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use tokio::runtime::Runtime;

/// The meeting point Shuttercrab uses unless told otherwise.
pub const MEET: &str = "https://shuttercrab.sandbox.gdn";

/// Why the meeting point turns away a host whose key it does not know, as
/// `meet/access.ts` says it: the error opening a room ends
/// with it.
pub const NOT_THE_HOST_KEY: &str = "Not the host key";

/// How long a viewer waits for the streaming app to be in the room.
const WAIT_FOR_HOST: Duration = Duration::from_secs(10);
/// How long the room may take to answer, a refusal or a relay login.
const ROOM_ANSWERS: Duration = Duration::from_secs(5);
/// How long a connection may take once both are in the room.
const CONNECTING: Duration = Duration::from_secs(15);
/// How long closing waits for the other app to be told.
const CLOSING: Duration = Duration::from_millis(250);

/// Join room `name` at `meet`, as its host with `host_key` or a viewer,
/// and say where this app is on the relay, from the login the room gives.
fn enter(meet: &str, name: &str, host_key: Option<&str>) -> Result<(room::Room, turn::Allocation)> {
    // Nowhere yet: the room answers with a relay login, or a refusal.
    let room = room::Room::join(meet, name, host_key, &[])?;
    let login = loop {
        match room.next(ROOM_ANSWERS) {
            Ok(room::Event::Relay(login)) => break login,
            Ok(room::Event::Closed(why)) => bail!("{why}"),
            Ok(_) => {}
            Err(_) => bail!("the meeting point offers no relay"),
        }
    };
    let allocation = turn::Allocation::new(&login)?;
    log::info!("this app is at {} on the relay", allocation.relayed());
    room.hello(&[allocation.relayed()])?;
    Ok((room, allocation))
}

/// Where the other app is on the relay, from the addresses it gave.
fn relay_address(addresses: &[SocketAddr]) -> Result<SocketAddr> {
    addresses
        .first()
        .copied()
        .context("the other app gave no relay address")
}

/// A room opened to stream from, waiting for a viewer.
pub struct Hosting {
    link: Link,
    identity: quic::Identity,
    room: room::Room,
    allocation: turn::Allocation,
}

/// Open a room at `meet` with `host_key` to stream from.
pub fn host(meet: &str, host_key: &str) -> Result<Hosting> {
    let identity = quic::Identity::new()?;
    let link = Link {
        room: rand::random(),
        fingerprint: identity.fingerprint,
        token: rand::random(),
    };
    let (room, allocation) = enter(meet, &link.room_name(), Some(host_key))?;
    Ok(Hosting {
        link,
        identity,
        room,
        allocation,
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
        let at = relay_address(&viewer.addresses)?;
        log::info!("the viewer is at {at}");
        let runtime = runtime()?;
        let token = self.link.token;
        let (endpoint, connection) = runtime.block_on(async {
            let endpoint = quic::server(self.allocation.connect(at)?, &self.identity)?;
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
            endpoint,
            _room: self.room,
        })
    }
}

/// Join the stream `link` is for, through `meet`.
pub fn join(meet: &str, link: &Link) -> Result<Connection> {
    let (room, allocation) = enter(meet, &link.room_name(), None)?;
    let host = room.wait_for_peer(true, WAIT_FOR_HOST)?;
    let at = relay_address(&host.addresses)?;
    log::info!("the stream is at {at}");
    let runtime = runtime()?;
    let (endpoint, connection) = runtime.block_on(async {
        let endpoint = quic::client(allocation.connect(at)?, link.fingerprint)?;
        let connection = tokio::time::timeout(CONNECTING, quic::connect(&endpoint, at))
            .await
            .context("the stream did not answer")??;
        quic::show_token(&connection, &link.token).await?;
        anyhow::Ok((endpoint, connection))
    })?;
    Ok(Connection {
        runtime,
        connection,
        endpoint,
        _room: room,
    })
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
    endpoint: quinn::Endpoint,
    /// Kept open while connected: leaving the room ends this app's relay
    /// login, and for the streaming app, the stream.
    _room: room::Room,
}

impl Connection {
    /// Where the other app is on the relay.
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
        // Give the close a moment to go out, so the other app hears of it
        // now rather than when the connection times out.
        // The timer is made inside the runtime, which it needs.
        let endpoint = &self.endpoint;
        let _ = self
            .runtime
            .block_on(async { tokio::time::timeout(CLOSING, endpoint.wait_idle()).await });
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
    /// The end of the stream, too, when either app closes the connection.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.runtime.block_on(self.stream.read(buffer)) {
            Ok(read) => Ok(read.unwrap_or(0)),
            Err(quinn::ReadError::ConnectionLost(
                quinn::ConnectionError::ApplicationClosed(_)
                | quinn::ConnectionError::LocallyClosed,
            )) => Ok(0),
            Err(e) => Err(io::Error::other(e)),
        }
    }
}
