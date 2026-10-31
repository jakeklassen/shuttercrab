//! The room on the meeting point (`meet/` in this repository): where the
//! streaming app and its viewers say which addresses they can be reached
//! at. A WebSocket of JSON events.

use anyhow::{Context, Result, anyhow, bail};
use std::{
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    },
    time::{Duration, Instant},
};
use tungstenite::{
    Message, WebSocket, client::IntoClientRequest, handshake::client::Request, http::HeaderValue,
    stream::MaybeTlsStream,
};

/// Another app in the room.
#[derive(Debug, Clone)]
pub struct Peer {
    pub host: bool,
    pub addresses: Vec<SocketAddr>,
}

/// What the room says.
#[derive(Debug)]
pub enum Event {
    Peer(Peer),
    /// A login to the relay, for this app while it stays in the room.
    Relay(crate::turn::Login),
    /// The room closed this app's connection, and why.
    Closed(String),
}

/// A connection to a room, read on a thread of its own until dropped.
pub struct Room {
    pub events: Receiver<Event>,
    /// Messages for the reading thread to send.
    outgoing: Sender<String>,
    stop: Arc<AtomicBool>,
}

/// How often the room is pinged, so nothing between closes a quiet socket.
const PING: Duration = Duration::from_secs(20);

/// How long each step of reaching the room may take: connecting, the TLS
/// handshake, the WebSocket's, and each send.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

impl Room {
    /// Join room `name` at `meet` (`https://…`), as its host with
    /// `host_key`, or as a viewer, and say this app is at `addresses`.
    pub fn join(
        meet: &str,
        name: &str,
        host_key: Option<&str>,
        addresses: &[SocketAddr],
    ) -> Result<Self> {
        let url = format!(
            "{}/rooms/{name}",
            meet.trim_end_matches('/')
                .replacen("https://", "wss://", 1)
                .replacen("http://", "ws://", 1)
        );
        let mut request = url.as_str().into_client_request()?;
        if let Some(key) = host_key {
            request.headers_mut().insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .context("the host key has characters a header cannot hold")?,
            );
        }
        let mut socket =
            connect(request, CONNECT_TIMEOUT).with_context(|| format!("could not reach {meet}"))?;
        socket.send(Message::text(hello(addresses)))?;
        // Reads wake now and then to send, to ping, and to see whether to
        // stop.
        if let Some(tcp) = tcp(&socket) {
            tcp.set_read_timeout(Some(Duration::from_millis(50)))?;
        }

        let (sender, events) = channel();
        let (outgoing, to_send) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::Builder::new()
            .name("room".into())
            .spawn(move || read(socket, &sender, &to_send, &stopped))?;
        Ok(Self {
            events,
            outgoing,
            stop,
        })
    }

    /// Say this app is now at `addresses`.
    pub fn hello(&self, addresses: &[SocketAddr]) -> Result<()> {
        self.outgoing
            .send(hello(addresses))
            .context("the room's connection has closed")
    }

    /// The next event, waiting up to `timeout`.
    pub fn next(&self, timeout: Duration) -> Result<Event> {
        self.events.recv_timeout(timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => anyhow!("the room said nothing in time"),
            RecvTimeoutError::Disconnected => anyhow!("the room's connection has closed"),
        })
    }

    /// Wait for the other side, up to `timeout`: the host for a viewer, a
    /// viewer for the host.
    pub fn wait_for_peer(&self, host: bool, timeout: Duration) -> Result<Peer> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.next(left)? {
                Event::Peer(peer) if peer.host == host && !peer.addresses.is_empty() => {
                    return Ok(peer);
                }
                Event::Closed(why) => bail!("{why}"),
                _ => {}
            }
        }
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn hello(addresses: &[SocketAddr]) -> String {
    serde_json::json!({
        "type": "hello",
        "addresses": addresses.iter().map(ToString::to_string).collect::<Vec<_>>(),
    })
    .to_string()
}

/// Open a WebSocket for `request`, giving up on a meeting point that does
/// not answer within `timeout` at any step: a socket that never connects,
/// or a server that takes it but never answers.
fn connect(request: Request, timeout: Duration) -> Result<WebSocket<MaybeTlsStream<TcpStream>>> {
    let uri = request.uri();
    let host = uri.host().context("the address has no host")?;
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("wss") {
            443
        } else {
            80
        });
    let mut failed = None;
    let mut tcp = None;
    for address in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(connected) => {
                tcp = Some(connected);
                break;
            }
            Err(e) => failed = Some(e),
        }
    }
    let tcp = match (tcp, failed) {
        (Some(tcp), _) => tcp,
        (None, Some(e)) => return Err(e.into()),
        (None, None) => bail!("{host} has no address"),
    };
    tcp.set_read_timeout(Some(timeout))?;
    tcp.set_write_timeout(Some(timeout))?;
    let (socket, _) = tungstenite::client_tls(request, tcp).map_err(|e| anyhow!("{e}"))?;
    Ok(socket)
}

fn tcp(socket: &WebSocket<MaybeTlsStream<TcpStream>>) -> Option<&TcpStream> {
    match socket.get_ref() {
        MaybeTlsStream::Plain(tcp) => Some(tcp),
        MaybeTlsStream::Rustls(tls) => Some(tls.get_ref()),
        _ => None,
    }
}

/// Pass on what the room says until it closes or `stop`, sending what
/// comes to send and pinging it.
fn read(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    events: &Sender<Event>,
    to_send: &Receiver<String>,
    stop: &AtomicBool,
) {
    let mut pinged = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        while let Ok(text) = to_send.try_recv() {
            if socket.send(Message::text(text)).is_err() {
                break;
            }
        }
        if pinged.elapsed() >= PING {
            pinged = Instant::now();
            if socket.send(Message::Ping(Default::default())).is_err() {
                break;
            }
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Some(event) = parse(&text) {
                    let _ = events.send(event);
                }
            }
            Ok(Message::Close(frame)) => {
                let why =
                    frame.map_or_else(|| "The room closed".to_string(), |f| f.reason.to_string());
                let _ = events.send(Event::Closed(why));
                break;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => {
                let _ = events.send(Event::Closed(format!("The room's connection broke: {e}")));
                break;
            }
        }
    }
    let _ = socket.close(None);
    let _ = socket.flush();
}

fn parse(text: &str) -> Option<Event> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    match value["type"].as_str()? {
        "peer" => Some(Event::Peer(Peer {
            host: value["host"].as_bool()?,
            addresses: value["addresses"]
                .as_array()?
                .iter()
                .filter_map(|a| a.as_str()?.parse().ok())
                .collect(),
        })),
        "relay" => Some(Event::Relay(crate::turn::Login {
            urls: value["urls"]
                .as_array()?
                .iter()
                .filter_map(|u| Some(u.as_str()?.to_string()))
                .collect(),
            username: value["username"].as_str()?.to_string(),
            credential: value["credential"].as_str()?.to_string(),
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meeting_point_that_never_answers_is_given_up_on() {
        // Takes the connection, then says nothing.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/rooms/test", silent.local_addr().unwrap());
        let started = Instant::now();
        let request = url.as_str().into_client_request().unwrap();
        assert!(connect(request, Duration::from_millis(200)).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_rooms_events_are_read() {
        let peer = parse(
            r#"{"type":"peer","id":"a","host":true,"addresses":["192.168.1.10:5000","bad"],"seen":"x"}"#,
        );
        let Some(Event::Peer(peer)) = peer else {
            panic!("{peer:?}")
        };
        assert!(peer.host);
        assert_eq!(peer.addresses, ["192.168.1.10:5000".parse().unwrap()]);
        assert!(parse(r#"{"type":"left","id":"a"}"#).is_none());
        assert!(parse("{}").is_none());
    }
}
