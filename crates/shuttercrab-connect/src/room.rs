//! The room on the meeting point (`meet/` in this repository): where the
//! streaming app and its viewers say which addresses they can be reached
//! at. A WebSocket of JSON events.

use anyhow::{Context, Result, bail};
use std::{
    net::{SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    time::{Duration, Instant},
};
use tungstenite::{
    Message, WebSocket, client::IntoClientRequest, http::HeaderValue, stream::MaybeTlsStream,
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
    /// The room closed this app's connection, and why.
    Closed(String),
}

/// A connection to a room, read on a thread of its own until dropped.
pub struct Room {
    pub events: Receiver<Event>,
    stop: Arc<AtomicBool>,
}

/// How often the room is pinged, so nothing between closes a quiet socket.
const PING: Duration = Duration::from_secs(20);

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
        let (mut socket, _) =
            tungstenite::connect(request).with_context(|| format!("could not reach {meet}"))?;
        let hello = serde_json::json!({
            "type": "hello",
            "addresses": addresses.iter().map(ToString::to_string).collect::<Vec<_>>(),
        });
        socket.send(Message::text(hello.to_string()))?;
        // Reads wake now and then to ping, and to see whether to stop.
        if let Some(tcp) = tcp(&socket) {
            tcp.set_read_timeout(Some(Duration::from_millis(500)))?;
        }

        let (sender, events) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::Builder::new()
            .name("room".into())
            .spawn(move || read(socket, &sender, &stopped))?;
        Ok(Self { events, stop })
    }

    /// The next event, waiting up to `timeout`.
    pub fn next(&self, timeout: Duration) -> Result<Event> {
        self.events
            .recv_timeout(timeout)
            .context("the room said nothing in time")
    }

    /// Wait for the other side, up to `timeout`: the host for a viewer, a
    /// viewer for the host.
    pub fn wait_for_peer(&self, host: bool, timeout: Duration) -> Result<Peer> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.next(left)? {
                Event::Peer(peer) if peer.host == host => return Ok(peer),
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

fn tcp(socket: &WebSocket<MaybeTlsStream<TcpStream>>) -> Option<&TcpStream> {
    match socket.get_ref() {
        MaybeTlsStream::Plain(tcp) => Some(tcp),
        MaybeTlsStream::Rustls(tls) => Some(tls.get_ref()),
        _ => None,
    }
}

/// Pass on what the room says until it closes or `stop`, pinging it.
fn read(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    events: &Sender<Event>,
    stop: &AtomicBool,
) {
    let mut pinged = Instant::now();
    while !stop.load(Ordering::Relaxed) {
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
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
