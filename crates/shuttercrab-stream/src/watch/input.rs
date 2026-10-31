//! Where a stream comes from, and joining it: the viewer says which codecs
//! it plays, then waits for the stream's format.

use crate::{PICTURES, Path, SOUND, describe};
use anyhow::{Context, Result, ensure};
use shuttercrab_capture::live::{LiveFormat, LivePlayer, SoundCodec, VideoCodec};
use shuttercrab_connect::{Connection, Link};
use std::{
    io::{BufReader, Read, Write},
    net::TcpStream,
};

/// Where frames come from.
type In = Box<dyn Read + Send>;

/// A stream to watch.
pub struct Input {
    /// The format and pictures.
    pictures: In,
    /// The sound, if it comes apart from the pictures.
    sound: Option<In>,
    /// The connection, if the stream is online.
    connection: Option<Connection>,
    /// The socket, if it is on this PC: shut down to stop reading.
    socket: Option<TcpStream>,
}

/// What the viewer says first: the codecs it can play, a bit each.
fn plays() -> u8 {
    VideoCodec::BEST_FIRST
        .into_iter()
        .filter(|&codec| LivePlayer::decodes(codec))
        .fold(0, |set, codec| set | codec.bit())
}

impl Input {
    /// The stream on this PC at `from` (`127.0.0.1:port`).
    pub fn local(from: &str) -> Result<Self> {
        let socket = TcpStream::connect(from).with_context(|| format!("no stream at {from}"))?;
        socket.set_nodelay(true)?;
        (&socket).write_all(&[plays()])?;
        Ok(Self {
            pictures: Box::new(socket.try_clone()?),
            sound: None,
            connection: None,
            socket: Some(socket),
        })
    }

    /// The stream `link` is for, through `meet`, reached by `path`.
    pub fn online(link: &Link, meet: &str, path: Path) -> Result<Self> {
        let connection = shuttercrab_connect::join(meet, link, path)?;
        log::info!(
            "connected {} to {}, {} ms there and back",
            if connection.relayed() {
                "through the relay"
            } else {
                "directly"
            },
            connection.remote(),
            connection.round_trip().as_millis()
        );
        connection.send(0)?.write_all(&[plays()])?;
        // Two streams, each tagged; they may arrive in either order.
        let (mut pictures, mut sound): (In, In) = (
            Box::new(connection.receive()?),
            Box::new(connection.receive()?),
        );
        let mut tag = [0];
        pictures.read_exact(&mut tag)?;
        if tag[0] == SOUND {
            std::mem::swap(&mut pictures, &mut sound);
            pictures.read_exact(&mut tag)?;
            ensure!(tag[0] == PICTURES, "the stream's pictures are missing");
        } else {
            ensure!(tag[0] == PICTURES, "not a Shuttercrab stream");
            sound.read_exact(&mut tag)?;
            ensure!(tag[0] == SOUND, "the stream's sound is missing");
        }
        Ok(Self {
            pictures,
            sound: Some(sound),
            connection: Some(connection),
            socket: None,
        })
    }
}

/// A stream joined and its format read, ready to play.
pub(super) struct Joined {
    pub(super) format: LiveFormat,
    pub(super) pictures: BufReader<In>,
    pub(super) sound: Option<In>,
    pub(super) connection: Option<Connection>,
    pub(super) socket: Option<TcpStream>,
}

impl Joined {
    /// Wait for `input`'s format, which comes once the sender has started.
    pub(super) fn read(input: Input) -> Result<Self> {
        let Input {
            pictures,
            sound,
            connection,
            socket,
        } = input;
        let mut pictures = BufReader::new(pictures);
        let format = LiveFormat::read_from(&mut pictures)?;
        log::info!(
            "{}x{} at {} fps, {}, {}",
            format.width,
            format.height,
            format.fps,
            describe(format.video),
            match format.sound {
                Some(SoundCodec::Aac) => "AAC sound",
                Some(SoundCodec::Pcm) => "PCM sound",
                Some(SoundCodec::Opus) => "Opus sound",
                None => "no sound",
            }
        );
        Ok(Self {
            format,
            pictures,
            sound,
            connection,
            socket,
        })
    }
}
