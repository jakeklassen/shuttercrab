//! The room on the meeting point (`meet/` in this repository): where the
//! streaming app and its viewers say which addresses they can be reached
//! at. A WebSocket of JSON messages, each tagged with its `type`.

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        Message, client::IntoClientRequest, handshake::client::Request, http::HeaderValue,
    },
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

/// What this app says to the room.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Said {
    Hello { addresses: Vec<String> },
}

/// What the room says, as it says it. The other types (`left`) are not
/// needed.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Heard {
    Peer {
        host: bool,
        addresses: Vec<String>,
    },
    Relay {
        urls: Vec<String>,
        username: String,
        credential: String,
    },
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A connection to a room, served on a thread of its own until dropped.
pub struct Room {
    pub events: Receiver<Event>,
    /// Messages for the room's thread to send. Dropping it closes the
    /// connection.
    outgoing: UnboundedSender<String>,
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let socket = runtime.block_on(async {
            let mut socket = connect(request, CONNECT_TIMEOUT)
                .await
                .with_context(|| format!("could not reach {meet}"))?;
            send(&mut socket, Message::text(hello(addresses))).await?;
            anyhow::Ok(socket)
        })?;
        let (sender, events) = channel();
        let (outgoing, to_send) = unbounded_channel();
        std::thread::Builder::new()
            .name("room".into())
            .spawn(move || runtime.block_on(serve(socket, &sender, to_send)))?;
        Ok(Self { events, outgoing })
    }

    /// Say this app is now at `addresses`.
    pub fn hello(&self, addresses: &[SocketAddr]) -> Result<()> {
        self.outgoing
            .send(hello(addresses))
            .map_err(|_| anyhow!("the room's connection has closed"))
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

fn hello(addresses: &[SocketAddr]) -> String {
    let hello = Said::Hello {
        addresses: addresses.iter().map(ToString::to_string).collect(),
    };
    serde_json::to_string(&hello).expect("a hello is JSON")
}

/// Open a WebSocket for `request`, giving up on a meeting point that does
/// not answer within `timeout` at any step: a socket that never connects,
/// or a server that takes it but never answers.
async fn connect(request: Request, timeout: Duration) -> Result<Socket> {
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
    for address in tokio::net::lookup_host((host, port)).await? {
        match tokio::time::timeout(timeout, TcpStream::connect(address)).await {
            Ok(Ok(connected)) => {
                tcp = Some(connected);
                break;
            }
            Ok(Err(e)) => failed = Some(anyhow::Error::from(e)),
            Err(_) => failed = Some(anyhow!("{address} did not answer")),
        }
    }
    let tcp = match (tcp, failed) {
        (Some(tcp), _) => tcp,
        (None, Some(e)) => return Err(e),
        (None, None) => bail!("{host} has no address"),
    };
    let (socket, _) =
        tokio::time::timeout(timeout, tokio_tungstenite::client_async_tls(request, tcp))
            .await
            .map_err(|_| anyhow!("the meeting point did not answer"))?
            .map_err(|e| anyhow!("{e}"))?;
    Ok(socket)
}

/// Send `message`, giving up after [`CONNECT_TIMEOUT`].
async fn send(socket: &mut Socket, message: Message) -> Result<()> {
    tokio::time::timeout(CONNECT_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| anyhow!("the room took too long to take a message"))??;
    Ok(())
}

/// Pass on what the room says, send what comes to send and ping it, each
/// as it happens, until the room closes or the [`Room`] is dropped.
async fn serve(mut socket: Socket, events: &Sender<Event>, mut to_send: UnboundedReceiver<String>) {
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING, PING);
    loop {
        tokio::select! {
            text = to_send.recv() => match text {
                Some(text) => {
                    if send(&mut socket, Message::text(text)).await.is_err() {
                        break;
                    }
                }
                // The Room was dropped.
                None => break,
            },
            _ = ping.tick() => {
                if send(&mut socket, Message::Ping(Default::default())).await.is_err() {
                    break;
                }
            }
            message = socket.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if let Some(event) = parse(&text) {
                        let _ = events.send(event);
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    let why = frame
                        .map_or_else(|| "The room closed".to_string(), |f| f.reason.to_string());
                    let _ = events.send(Event::Closed(why));
                    break;
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    let _ = events.send(Event::Closed(format!("The room's connection broke: {e}")));
                    break;
                }
                None => {
                    let _ = events.send(Event::Closed("The room closed".to_string()));
                    break;
                }
            },
        }
    }
    let _ = tokio::time::timeout(CONNECT_TIMEOUT, socket.close(None)).await;
}

fn parse(text: &str) -> Option<Event> {
    Some(match serde_json::from_str(text).ok()? {
        Heard::Peer { host, addresses } => Event::Peer(Peer {
            host,
            addresses: addresses.iter().filter_map(|a| a.parse().ok()).collect(),
        }),
        Heard::Relay {
            urls,
            username,
            credential,
        } => Event::Relay(crate::turn::Login {
            urls,
            username,
            credential,
        }),
    })
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(
            runtime
                .block_on(connect(request, Duration::from_millis(200)))
                .is_err()
        );
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
        let relay = parse(r#"{"type":"relay","urls":["turn:a"],"username":"u","credential":"c"}"#);
        let Some(Event::Relay(login)) = relay else {
            panic!("{relay:?}")
        };
        assert_eq!(
            (login.urls, login.username),
            (vec!["turn:a".into()], "u".into())
        );
        assert!(parse(r#"{"type":"left","id":"a"}"#).is_none());
        assert!(parse("{}").is_none());
    }

    #[test]
    fn hello_says_where_this_app_is() {
        let text = hello(&["10.0.0.2:4000".parse().unwrap()]);
        assert_eq!(text, r#"{"type":"hello","addresses":["10.0.0.2:4000"]}"#);
    }

    /// A room on this PC: answers the hello with a peer, then reports what
    /// it is sent until the connection closes.
    fn local_room() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (heard, said) = channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                listener.set_nonblocking(true).unwrap();
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let (tcp, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    match message {
                        Message::Text(text) => {
                            if text.contains("hello") && heard.send(text.to_string()).is_ok() {
                                let peer = r#"{"type":"peer","id":"b","host":true,"addresses":["10.0.0.3:5000"]}"#;
                                let _ = socket.send(Message::text(peer)).await;
                            }
                        }
                        Message::Close(_) => {
                            let _ = heard.send("closed".into());
                            break;
                        }
                        _ => {}
                    }
                }
            });
        });
        (url, said)
    }

    #[test]
    fn a_room_passes_messages_both_ways_and_closes_when_dropped() {
        let (url, said) = local_room();
        let room = Room::join(&url, "test", None, &["10.0.0.2:4000".parse().unwrap()]).unwrap();
        let wait = Duration::from_secs(5);
        assert!(said.recv_timeout(wait).unwrap().contains("10.0.0.2:4000"));
        let peer = room.wait_for_peer(true, wait).unwrap();
        assert_eq!(peer.addresses, ["10.0.0.3:5000".parse().unwrap()]);

        room.hello(&["10.0.0.2:4001".parse().unwrap()]).unwrap();
        assert!(said.recv_timeout(wait).unwrap().contains("10.0.0.2:4001"));
        // The answer to the second hello.
        room.wait_for_peer(true, wait).unwrap();

        let dropped = Instant::now();
        drop(room);
        assert_eq!(said.recv_timeout(wait).unwrap(), "closed");
        assert!(dropped.elapsed() < Duration::from_secs(1));
    }
}
