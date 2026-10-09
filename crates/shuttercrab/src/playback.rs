//! Playing a recording in the main window. A thread of its own owns the
//! Media Engine ([`shuttercrab_capture::play`]): it takes [`Command`]s and
//! sends back [`Update`]s, among them each new picture at the size the
//! window shows it. While paused it sleeps until told something; while
//! playing or seeking it asks for a picture at each refresh of the display.
//! If its graphics device is lost (a driver update), it opens again.

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use shuttercrab_capture::{
    play::{Player, PlayerEvent},
    service::is_device_lost,
};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel},
    time::{Duration, Instant},
};

/// What the window asks of the player.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Play,
    Pause,
    Seek(Duration),
    /// 0 to 1.
    Volume(f64),
    Mute(bool),
    /// The size to send pictures at, physical pixels.
    Size(u32, u32),
    /// Something the Media Engine said.
    Engine(PlayerEvent),
    Close,
}

/// What the player tells the window.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// The file is open: its pictures' size and its length.
    Loaded {
        width: u32,
        height: u32,
        duration: Duration,
    },
    /// A picture to show, BGRA8.
    Frame {
        bgra: Vec<u8>,
        width: u32,
        height: u32,
    },
    /// Where playing is.
    Position(Duration),
    Playing(bool),
    /// Playing reached the end.
    Ended,
    Failed(String),
}

/// The window's end of a player: commands go one way, updates the other.
/// Dropping it closes the player.
pub struct PlayerLink {
    commands: Sender<Command>,
    pub updates: Option<UnboundedReceiver<Update>>,
}

impl PlayerLink {
    /// A link to nothing yet: the other ends, for a player or a test to
    /// drive.
    pub fn new() -> (
        Self,
        Receiver<Command>,
        Sender<Command>,
        UnboundedSender<Update>,
    ) {
        let (commands, inbox) = channel();
        let (updates, outbox) = unbounded();
        let link = Self {
            commands: commands.clone(),
            updates: Some(outbox),
        };
        (link, inbox, commands, updates)
    }

    pub fn send(&self, command: Command) {
        // A player that has stopped has said why in an update.
        let _ = self.commands.send(command);
    }
}

impl Drop for PlayerLink {
    fn drop(&mut self) {
        self.send(Command::Close);
    }
}

/// Open `path` on a thread of its own, paused at the start.
pub fn open(path: &Path) -> PlayerLink {
    let (link, inbox, engine, updates) = PlayerLink::new();
    let path = path.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("player".into())
        .spawn(move || run(path, inbox, engine, updates));
    if let Err(e) = spawned {
        log::error!("could not start the player: {e}");
    }
    link
}

fn run(
    path: PathBuf,
    inbox: Receiver<Command>,
    engine: Sender<Command>,
    updates: UnboundedSender<Update>,
) {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    log::info!("playing {name}");
    let mut size = None;
    let mut retries = 0;
    loop {
        match play(&path, &inbox, engine.clone(), &updates, &mut size) {
            Ok(()) => break,
            // A driver update takes the device away for a few seconds.
            Err(e) if is_device_lost(&e) && retries < DEVICE_RETRIES => {
                retries += 1;
                log::warn!("the graphics device was lost playing {name}; trying again: {e:#}");
                if !wait_for_retry(&inbox, &mut size) {
                    break;
                }
            }
            Err(e) => {
                log::error!("could not play {name}: {e:#}");
                let _ = updates.unbounded_send(Update::Failed(format!("{e:#}")));
                break;
            }
        }
    }
    log::info!("player closed");
}

/// How many times a player whose graphics device was lost opens again,
/// [`RETRY_WAIT`] apart: a driver update had it back after about 6 s.
const DEVICE_RETRIES: u32 = 15;

const RETRY_WAIT: Duration = Duration::from_secs(1);

/// Wait [`RETRY_WAIT`] before opening the player again, keeping the size the
/// window asks for and dropping the lost player's last words. False if the
/// window closed meanwhile.
fn wait_for_retry(inbox: &Receiver<Command>, size: &mut Option<(u32, u32)>) -> bool {
    let until = Instant::now() + RETRY_WAIT;
    loop {
        match inbox.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(Command::Close) | Err(RecvTimeoutError::Disconnected) => return false,
            Ok(Command::Size(width, height)) => *size = Some((width, height)),
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => return true,
        }
    }
}

/// Open `path` and play it until the window closes. `size` is the size the
/// window asked for, kept across a player opened again.
fn play(
    path: &Path,
    inbox: &Receiver<Command>,
    engine: Sender<Command>,
    updates: &UnboundedSender<Update>,
    size: &mut Option<(u32, u32)>,
) -> anyhow::Result<()> {
    let player = Player::open(path, move |event| {
        let _ = engine.send(Command::Engine(event));
    })?;
    let mut looper = Looper::new(player, updates.clone(), *size);
    let result = looper.run(inbox);
    *size = looper.size;
    result
}

/// The player thread's state.
struct Looper {
    player: Player,
    updates: UnboundedSender<Update>,
    /// The size the window wants pictures at, once it has said.
    size: Option<(u32, u32)>,
    loaded: bool,
    /// The first picture is ready: the engine can draw one.
    first_ready: bool,
    playing: bool,
    /// A seek is under way; `next_seek`, if any, follows it.
    seeking: bool,
    next_seek: Option<Duration>,
    /// The picture shown is out of date (a new size, a seek), even if the
    /// engine has no new one.
    redraw: bool,
    /// The last position sent.
    sent_position: Option<Duration>,
}

/// How far playing moves before the window hears where it is.
const POSITION_STEP: Duration = Duration::from_millis(100);

/// How near the end Play counts as at the end, and starts again.
const END_SLACK: Duration = Duration::from_millis(50);

impl Looper {
    fn new(player: Player, updates: UnboundedSender<Update>, size: Option<(u32, u32)>) -> Self {
        Self {
            player,
            updates,
            size,
            loaded: false,
            first_ready: false,
            playing: false,
            seeking: false,
            next_seek: None,
            redraw: false,
            sent_position: None,
        }
    }

    /// Take commands until the window closes.
    fn run(&mut self, inbox: &Receiver<Command>) -> anyhow::Result<()> {
        loop {
            // Asleep until told something, unless pictures are due.
            let first = if self.busy() {
                match inbox.try_recv() {
                    Ok(command) => Some(command),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            } else {
                match inbox.recv() {
                    Ok(command) => Some(command),
                    Err(_) => return Ok(()),
                }
            };
            for command in first
                .into_iter()
                .chain(std::iter::from_fn(|| inbox.try_recv().ok()))
            {
                if !self.handle(command)? {
                    return Ok(());
                }
            }
            if self.busy() {
                self.player.wait_for_refresh()?;
                self.show()?;
            }
        }
    }

    /// Whether pictures are due at each refresh.
    fn busy(&self) -> bool {
        self.loaded && self.size.is_some() && (self.playing || self.seeking || self.redraw)
    }

    fn send(&self, update: Update) {
        let _ = self.updates.unbounded_send(update);
    }

    /// Returns false once the player should close.
    fn handle(&mut self, command: Command) -> anyhow::Result<bool> {
        match command {
            Command::Play => {
                // At the end, whether played there or moved there: from the
                // start again.
                let duration = self.player.duration();
                let at_end = self.player.position() + END_SLACK >= duration;
                if self.player.is_ended() || at_end {
                    self.seek(Duration::ZERO)?;
                }
                self.player.play()?;
            }
            Command::Pause => self.player.pause()?,
            Command::Seek(at) => self.seek(at.min(self.player.duration()))?,
            Command::Volume(volume) => self.player.set_volume(volume)?,
            Command::Mute(muted) => self.player.set_muted(muted)?,
            Command::Size(width, height) => {
                if self.size != Some((width, height)) {
                    self.size = Some((width, height));
                    self.redraw = self.first_ready;
                }
            }
            Command::Engine(event) => self.on_engine(event)?,
            Command::Close => return Ok(false),
        }
        Ok(true)
    }

    fn on_engine(&mut self, event: PlayerEvent) -> anyhow::Result<()> {
        match event {
            PlayerEvent::Loaded => {
                self.loaded = true;
                let (width, height) = self
                    .player
                    .size()
                    .ok_or_else(|| anyhow::anyhow!("the recording has no pictures"))?;
                self.send(Update::Loaded {
                    width,
                    height,
                    duration: self.player.duration(),
                });
            }
            PlayerEvent::FirstFrame => {
                self.first_ready = true;
                self.redraw = true;
            }
            PlayerEvent::Playing => {
                self.playing = true;
                self.send(Update::Playing(true));
            }
            PlayerEvent::Paused => {
                self.playing = false;
                self.send(Update::Playing(false));
            }
            PlayerEvent::Seeked => {
                self.seeking = false;
                self.redraw = true;
                if let Some(at) = self.next_seek.take() {
                    self.seek(at)?;
                }
            }
            PlayerEvent::Ended => {
                self.playing = false;
                self.send(Update::Playing(false));
                self.send(Update::Ended);
                self.send_position(true);
            }
            PlayerEvent::Failed(why) => anyhow::bail!("{why}"),
        }
        Ok(())
    }

    /// Seek to `at`, or after the seek under way: a dragged slider asks
    /// faster than the engine seeks.
    fn seek(&mut self, at: Duration) -> anyhow::Result<()> {
        if self.seeking {
            self.next_seek = Some(at);
        } else {
            self.player.seek(at)?;
            self.seeking = true;
        }
        self.sent_position = Some(at);
        self.send(Update::Position(at));
        Ok(())
    }

    /// Send the new picture, if there is one, and where playing is.
    fn show(&mut self) -> anyhow::Result<()> {
        let Some((width, height)) = self.size else {
            return Ok(());
        };
        let picture = match self.player.next_frame(width, height)? {
            Some(bgra) => Some(bgra),
            // The engine has nothing new, but the picture must be redrawn:
            // at a new size, or after a seek that landed where it was.
            None if self.redraw && self.first_ready && !self.seeking => {
                Some(self.player.frame(width, height)?)
            }
            None => None,
        };
        if let Some(bgra) = picture {
            self.redraw = false;
            self.send(Update::Frame {
                bgra,
                width,
                height,
            });
        }
        if !self.seeking {
            self.send_position(false);
        }
        Ok(())
    }

    /// Send where playing is if it has moved far enough, or at all when
    /// `always`.
    fn send_position(&mut self, always: bool) {
        // The engine counts on a little past the end.
        let at = self.player.position().min(self.player.duration());
        let moved = self
            .sent_position
            .is_none_or(|sent| at.abs_diff(sent) >= POSITION_STEP);
        if moved || (always && self.sent_position != Some(at)) {
            self.sent_position = Some(at);
            self.send(Update::Position(at));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_to_retry_keeps_the_size_and_stops_on_close() {
        let (commands, inbox) = channel();
        let mut size = None;
        commands.send(Command::Size(640, 360)).unwrap();
        // The lost player's last words are dropped.
        commands.send(Command::Engine(PlayerEvent::Loaded)).unwrap();
        assert!(wait_for_retry(&inbox, &mut size));
        assert_eq!(size, Some((640, 360)));
        assert!(inbox.try_recv().is_err());

        commands.send(Command::Close).unwrap();
        assert!(!wait_for_retry(&inbox, &mut size));
        drop(commands);
        assert!(!wait_for_retry(&inbox, &mut size));
    }
}
