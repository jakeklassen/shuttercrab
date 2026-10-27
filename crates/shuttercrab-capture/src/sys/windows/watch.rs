//! Watching a live recording (issue #66, spike S0): coded frames, as they
//! arrive, into a `MediaStreamSource` that a WinRT `MediaPlayer` plays with
//! no buffering beyond what decoding needs. The player decodes on the
//! graphics card and plays the sound itself; the app copies each picture
//! out of it.
//!
//! `MediaStreamSource` asks for each track's next frame when it is ready
//! for one. A frame that has not arrived yet is promised (a deferral) and
//! handed over the moment it does.

use crate::live::{LiveFormat, LiveFrame, Track};
use anyhow::{Context, Result};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use windows::{
    Foundation::{TimeSpan, TypedEventHandler},
    Graphics::DirectX::Direct3D11::IDirect3DSurface,
    Media::{
        Core::{
            AudioStreamDescriptor, MediaSource, MediaStreamSample, MediaStreamSource,
            MediaStreamSourceSampleRequest, MediaStreamSourceSampleRequestDeferral,
            MediaStreamSourceSampleRequestedEventArgs, MediaStreamSourceStartingEventArgs,
            VideoStreamDescriptor,
        },
        MediaProperties::{AudioEncodingProperties, VideoEncodingProperties},
        Playback::{MediaPlayer, MediaPlayerFailedEventArgs},
    },
    Storage::Streams::DataWriter,
    core::{Interface, Ref},
};

/// The sound track's bit rate, as the recorder encodes it.
const SOUND_BITRATE: u32 = 192_000;

/// One track's frames waiting for the player, or the player waiting for one.
#[derive(Default)]
struct Feed {
    queue: VecDeque<MediaStreamSample>,
    waiting: Option<(
        MediaStreamSourceSampleRequest,
        MediaStreamSourceSampleRequestDeferral,
    )>,
}

impl Feed {
    fn give(&mut self, sample: MediaStreamSample) -> Result<()> {
        match self.waiting.take() {
            Some((request, deferral)) => {
                request.SetSample(&sample)?;
                deferral.Complete()?;
            }
            None => self.queue.push_back(sample),
        }
        Ok(())
    }
}

#[derive(Default)]
struct Feeds {
    video: Feed,
    sound: Feed,
    /// The first key frame's time: the player's zero.
    origin: Option<i64>,
    /// The newest picture's time, on the player's clock.
    newest: i64,
}

/// A live recording playing as its frames arrive.
pub struct LivePlayer {
    player: MediaPlayer,
    feeds: Arc<Mutex<Feeds>>,
    _source: MediaStreamSource,
}

impl LivePlayer {
    /// Play a recording of `format`. `failed` hears why the player stopped,
    /// if it does; `frame` is called, on one of the player's threads, when
    /// a new picture is ready to copy with [`LivePlayer::copy_frame`].
    pub fn new(
        format: LiveFormat,
        failed: impl Fn(String) + Send + 'static,
        frame: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let video = VideoEncodingProperties::CreateH264()?;
        video.SetWidth(format.width)?;
        video.SetHeight(format.height)?;
        let rate = video.FrameRate()?;
        rate.SetNumerator(format.fps)?;
        rate.SetDenominator(1)?;
        let video = VideoStreamDescriptor::Create(&video)?;
        let source =
            if format.sound {
                let sound = AudioStreamDescriptor::Create(
                    &AudioEncodingProperties::CreateAacAdts(48_000, 2, SOUND_BITRATE)?,
                )?;
                MediaStreamSource::CreateFromDescriptors(&video, &sound)?
            } else {
                MediaStreamSource::CreateFromDescriptor(&video)?
            };
        // Live: play each frame as soon as it is decoded.
        source.SetBufferTime(TimeSpan { Duration: 0 })?;
        source.SetIsLive(true)?;
        source.SetCanSeek(false)?;
        source.Starting(&TypedEventHandler::new(
            |_, args: Ref<MediaStreamSourceStartingEventArgs>| {
                if let Some(args) = args.as_ref() {
                    args.Request()?
                        .SetActualStartPosition(TimeSpan { Duration: 0 })?;
                }
                Ok(())
            },
        ))?;
        let feeds = Arc::new(Mutex::new(Feeds::default()));
        let requests = feeds.clone();
        source.SampleRequested(&TypedEventHandler::new(
            move |_, args: Ref<MediaStreamSourceSampleRequestedEventArgs>| {
                let Some(args) = args.as_ref() else {
                    return Ok(());
                };
                let request = args.Request()?;
                let is_video = request
                    .StreamDescriptor()?
                    .cast::<VideoStreamDescriptor>()
                    .is_ok();
                let mut feeds = requests.lock().unwrap_or_else(|e| e.into_inner());
                let feed = if is_video {
                    &mut feeds.video
                } else {
                    &mut feeds.sound
                };
                match feed.queue.pop_front() {
                    Some(sample) => request.SetSample(&sample)?,
                    None => {
                        let deferral = request.GetDeferral()?;
                        feed.waiting = Some((request, deferral));
                    }
                }
                Ok(())
            },
        ))?;

        let player = MediaPlayer::new()?;
        player.SetRealTimePlayback(true)?;
        player.SetIsVideoFrameServerEnabled(true)?;
        player.MediaFailed(&TypedEventHandler::new(
            move |_, args: Ref<MediaPlayerFailedEventArgs>| {
                if let Some(args) = args.as_ref() {
                    failed(format!(
                        "{} ({:?}, {})",
                        args.ErrorMessage()?,
                        args.Error()?,
                        args.ExtendedErrorCode()?
                    ));
                }
                Ok(())
            },
        ))?;
        player.VideoFrameAvailable(&TypedEventHandler::new(move |_, _| {
            frame();
            Ok(())
        }))?;
        player.SetSource(&MediaSource::CreateFromMediaStreamSource(&source)?)?;
        player.Play()?;
        Ok(Self {
            player,
            feeds,
            _source: source,
        })
    }

    /// Hand the player the next frame. Pictures before the first key frame
    /// cannot be decoded, and are dropped, as is sound before it.
    pub fn push(&self, frame: LiveFrame) -> Result<()> {
        let mut feeds = self.feeds.lock().unwrap_or_else(|e| e.into_inner());
        let origin = match feeds.origin {
            Some(origin) => origin,
            None if frame.track == Track::Video && frame.key => *feeds.origin.insert(frame.time),
            None => return Ok(()),
        };
        let time = frame.time - origin;
        if time < 0 {
            return Ok(());
        }
        let writer = DataWriter::new()?;
        writer.WriteBytes(&frame.data)?;
        let sample = MediaStreamSample::CreateFromBuffer(
            &writer.DetachBuffer()?,
            TimeSpan { Duration: time },
        )?;
        sample.SetDuration(TimeSpan {
            Duration: frame.duration,
        })?;
        sample.SetKeyFrame(frame.key)?;
        match frame.track {
            Track::Video => {
                feeds.newest = feeds.newest.max(time);
                feeds.video.give(sample)
            }
            Track::Sound => feeds.sound.give(sample),
        }
        .context("could not hand the player a frame")
    }

    /// How far the picture shown is behind the newest one received.
    pub fn behind(&self) -> Duration {
        let newest = self.feeds.lock().unwrap_or_else(|e| e.into_inner()).newest;
        let position = self
            .player
            .PlaybackSession()
            .and_then(|s| s.Position())
            .map_or(0, |p| p.Duration);
        Duration::from_nanos((newest - position).max(0) as u64 * 100)
    }

    /// Copy the current picture into `surface`, scaled to fit it.
    pub fn copy_frame(&self, surface: &IDirect3DSurface) -> Result<()> {
        self.player
            .CopyFrameToVideoSurface(surface)
            .context("could not copy the picture")
    }

    pub fn set_muted(&self, muted: bool) -> Result<()> {
        self.player.SetIsMuted(muted).context("could not mute")
    }
}

impl Drop for LivePlayer {
    fn drop(&mut self) {
        let _ = self.player.Close();
    }
}
