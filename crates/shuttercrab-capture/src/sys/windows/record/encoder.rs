//! From converted frames to an MP4: RGBA to NV12 in the Direct3D video
//! processor, then H.264 through Media Foundation's sink writer, beside an
//! AAC sound track if the recording has sound.
//!
//! A live recording (issue #66) goes to a media sink of ours instead of a
//! file: the sink writer encodes just the same, and the sink hands each
//! coded frame on as it comes.

use super::{live_sink, sound};
use crate::{
    live::{LiveFrame, SoundCodec, Track},
    record::{Live, LiveSink},
    sys::imp::gpu::Gpu,
};
use anyhow::{Context, Result, ensure};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::E_NOTIMPL,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
        Media::MediaFoundation::*,
    },
    core::{HSTRING, Interface, Ref, implement},
};

/// RGBA → NV12 (BT.709, studio range) with the Direct3D video processor,
/// into a small pool of textures the encoder reads.
pub(super) struct Nv12 {
    video: ID3D11VideoContext1,
    processor: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    slots: Vec<Slot>,
}

struct Slot {
    texture: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
    /// Samples of it the encoder still holds: a repeated frame (a still
    /// screen, the stop) can be with it more than once. Counted off as Media
    /// Foundation releases them.
    busy: Arc<AtomicUsize>,
    release: IMFAsyncCallback,
}

/// How many frames can be with the encoder at once.
const SLOTS: usize = 4;

impl Nv12 {
    /// From `rgba`, `(w, h)`, to NV12 of `(out_w, out_h)`: the video
    /// processor scales it on the way, for a live recording sent smaller.
    pub(super) fn new(
        gpu: &Gpu,
        rgba: &ID3D11Texture2D,
        (w, h): (u32, u32),
        (out_w, out_h): (u32, u32),
        fps: u32,
    ) -> Result<Self> {
        let device: ID3D11VideoDevice = gpu.device.cast().context("no video device")?;
        let video: ID3D11VideoContext1 = gpu.context.cast().context("no video context")?;
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: fps,
                Denominator: 1,
            },
            InputWidth: w,
            InputHeight: h,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: fps,
                Denominator: 1,
            },
            OutputWidth: out_w,
            OutputHeight: out_h,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        unsafe {
            let enumerator = device.CreateVideoProcessorEnumerator(&content)?;
            let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_R8G8B8A8_UNORM)?;
            ensure!(
                support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0,
                "the video processor cannot read RGBA"
            );
            let processor = device.CreateVideoProcessor(&enumerator, 0)?;
            let mut input = None;
            device.CreateVideoProcessorInputView(
                rgba,
                &enumerator,
                &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV {
                            MipSlice: 0,
                            ArraySlice: 0,
                        },
                    },
                },
                Some(&mut input),
            )?;
            let mut slots = Vec::with_capacity(SLOTS);
            for _ in 0..SLOTS {
                let texture = gpu.texture(
                    out_w,
                    out_h,
                    DXGI_FORMAT_NV12,
                    D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
                    None,
                )?;
                let mut view = None;
                device.CreateVideoProcessorOutputView(
                    &texture,
                    &enumerator,
                    &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                        },
                    },
                    Some(&mut view),
                )?;
                let busy = Arc::new(AtomicUsize::new(0));
                slots.push(Slot {
                    texture,
                    view: view.context("no output view")?,
                    release: Released(busy.clone()).into(),
                    busy,
                });
            }
            video.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            video.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            video.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            Ok(Self {
                video,
                processor,
                input: input.context("no input view")?,
                slots,
            })
        }
    }

    pub(super) fn free_slot(&self) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.busy.load(Ordering::Acquire) == 0)
    }

    pub(super) fn convert(&self, _gpu: &Gpu, slot: usize) -> Result<()> {
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: std::mem::ManuallyDrop::new(Some(self.input.clone())),
            ..Default::default()
        };
        let streams = [stream];
        let result = unsafe {
            self.video
                .VideoProcessorBlt(&self.processor, &self.slots[slot].view, 0, &streams)
        };
        // Release the reference the stream took.
        let [mut stream] = streams;
        unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.context("VideoProcessorBlt failed")
    }
}

/// Counts a use of a slot off when Media Foundation releases its sample.
#[implement(IMFAsyncCallback)]
struct Released(Arc<AtomicUsize>);

impl IMFAsyncCallback_Impl for Released_Impl {
    fn GetParameters(&self, _: *mut u32, _: *mut u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, _: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        self.0.fetch_sub(1, Ordering::AcqRel);
        Ok(())
    }
}

/// An H.264 MP4 file written by Media Foundation's sink writer, with an AAC
/// sound track if asked.
pub(super) struct Mp4Writer {
    writer: IMFSinkWriter,
    stream: u32,
    /// Where the sound track goes, if there is one.
    sound: Option<SoundTrack>,
    _manager: IMFDXGIDeviceManager,
    finished: bool,
}

enum SoundTrack {
    /// The sink writer's stream, encoded to AAC.
    Aac(u32),
    /// Straight to a live recording's sink, as the samples are.
    Pcm(LiveSink),
}

/// The sound track's bit rate, bytes a second: 192 kbit/s, plenty for stereo
/// AAC.
const SOUND_BYTES_PER_SECOND: u32 = 24_000;

impl Mp4Writer {
    /// With `hardware`, Media Foundation may choose a hardware encoder; with
    /// `sound`, the file gets a sound track (48 kHz stereo AAC). With
    /// `live`, nothing is written to `path`: the frames go to `live.sink`
    /// as they are encoded, the sound as `live.sound` says.
    pub(super) fn new(
        gpu: &Gpu,
        path: &Path,
        (w, h, fps): (u32, u32, u32),
        hardware: bool,
        sound: bool,
        live: Option<Live>,
    ) -> Result<Self> {
        let pcm = match &live {
            Some(live) if sound && live.sound == SoundCodec::Pcm => Some(live.sink.clone()),
            _ => None,
        };
        let aac = sound && pcm.is_none();
        unsafe {
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&gpu.device, token)?;

            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)?;
            let attributes = attributes.context("no attributes")?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, hardware as u32)?;
            attributes.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
            let streaming = live.is_some();

            // PRD §13.1: H.264, Rec.709. About 0.1 bit per pixel per frame,
            // within 2–80 Mbit/s, unless a live recording asks for its own.
            let bitrate = match live.as_ref().and_then(|l| l.bitrate) {
                Some(bitrate) => u64::from(bitrate),
                None => ((w as u64 * h as u64 * fps as u64) / 10).clamp(2_000_000, 80_000_000),
            };
            let video = video_type(&MFVideoFormat_H264, w, h, fps)?;
            video.SetUINT32(&MF_MT_AVG_BITRATE, bitrate as u32)?;
            video.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            if streaming {
                // A key frame a second: where someone who starts watching
                // late can start.
                video.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, fps)?;
            }
            let aac = if aac {
                let aac = sound_type(&MFAudioFormat_AAC)?;
                aac.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SOUND_BYTES_PER_SECOND)?;
                Some(aac)
            } else {
                None
            };

            let writer = match live {
                None => {
                    // MP4 whatever the file is called: the app writes to a
                    // `.partial` name and renames the file once it is
                    // finished.
                    attributes
                        .SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
                    let _ = std::fs::remove_file(path);
                    MFCreateSinkWriterFromURL(&HSTRING::from(path), None, &attributes)
                        .with_context(|| format!("cannot write {}", path.display()))?
                }
                Some(live) => {
                    // No frames held back to look ahead (no B-frames).
                    attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;
                    let media_sink = live_sink::create(&video, aac.as_ref(), live.sink)?;
                    MFCreateSinkWriterFromMediaSink(&media_sink, &attributes)
                        .context("cannot stream the recording")?
                }
            };
            // A live sink has its streams already, in this order.
            let stream = if streaming {
                0
            } else {
                writer.AddStream(&video)?
            };
            let input = video_type(&MFVideoFormat_NV12, w, h, fps)?;
            writer
                .SetInputMediaType(stream, &input, None)
                .context("the encoder does not take NV12 at this size")?;
            let sound = if let Some(aac) = aac {
                let stream = if streaming {
                    1
                } else {
                    writer.AddStream(&aac).context("no AAC encoder")?
                };
                let input = sound_type(&MFAudioFormat_PCM)?;
                input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, u32::from(sound::CHANNELS) * 2)?;
                input.SetUINT32(
                    &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                    sound::RATE * u32::from(sound::CHANNELS) * 2,
                )?;
                writer
                    .SetInputMediaType(stream, &input, None)
                    .context("the AAC encoder does not take 16-bit stereo")?;
                Some(SoundTrack::Aac(stream))
            } else {
                pcm.map(SoundTrack::Pcm)
            };
            writer.BeginWriting().context("BeginWriting failed")?;
            Ok(Self {
                writer,
                stream,
                sound,
                _manager: manager,
                finished: false,
            })
        }
    }

    /// Whether the file has a sound track.
    pub(super) fn has_sound(&self) -> bool {
        self.sound.is_some()
    }

    /// Hand `samples` (16-bit, the channels interleaved) to the sound track
    /// at `time`. Nothing if the file has none.
    pub(super) fn write_sound(&self, samples: &[i16], time: i64) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let frames = samples.len() as u64 / u64::from(sound::CHANNELS);
        let stream = match &self.sound {
            None => return Ok(()),
            Some(SoundTrack::Aac(stream)) => *stream,
            Some(SoundTrack::Pcm(sink)) => {
                sink(LiveFrame {
                    track: Track::Sound,
                    time,
                    duration: sound::frames_to_ticks(frames),
                    key: true,
                    data: samples.iter().flat_map(|s| s.to_le_bytes()).collect(),
                });
                return Ok(());
            }
        };
        let bytes = std::mem::size_of_val(samples);
        unsafe {
            let buffer = MFCreateMemoryBuffer(bytes as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(samples.as_ptr().cast::<u8>(), data, bytes);
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(sound::frames_to_ticks(frames))?;
            self.writer
                .WriteSample(stream, &sample)
                .context("WriteSample (sound) failed")?;
        }
        Ok(())
    }

    /// Hand the slot's texture to the encoder at `time`.
    pub(super) fn write(&self, nv12: &Nv12, slot: usize, time: i64, duration: i64) -> Result<()> {
        let slot = &nv12.slots[slot];
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &slot.texture, 0, false)?;
            let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buffer.SetCurrentLength(length)?;
            let sample = MFCreateTrackedSample()?;
            sample.SetAllocator(&slot.release, None)?;
            let sample: IMFSample = sample.cast()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            slot.busy.fetch_add(1, Ordering::AcqRel);
            if let Err(e) = self.writer.WriteSample(self.stream, &sample) {
                slot.busy.fetch_sub(1, Ordering::AcqRel);
                return Err(e).context("WriteSample failed");
            }
        }
        Ok(())
    }

    /// Finish the file. Returns whether a hardware encoder was used.
    pub(super) fn finish(&mut self) -> Result<bool> {
        self.finished = true;
        let hardware = self.hardware_encoder();
        unsafe { self.writer.Finalize() }.context("could not finish the video file")?;
        Ok(hardware)
    }

    /// A hardware encoder carries `MFT_ENUM_HARDWARE_URL_Attribute`.
    fn hardware_encoder(&self) -> bool {
        unsafe {
            let mut raw = std::ptr::null_mut();
            if self
                .writer
                .GetServiceForStream(
                    self.stream,
                    &windows::core::GUID::zeroed(),
                    &IMFTransform::IID,
                    &mut raw,
                )
                .is_err()
                || raw.is_null()
            {
                return false;
            }
            let transform = IMFTransform::from_raw(raw);
            transform
                .GetAttributes()
                .and_then(|attributes| attributes.GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute))
                .is_ok()
        }
    }
}

/// The sound track's format, 48 kHz 16-bit stereo, in `subtype` (PCM in,
/// AAC out).
fn sound_type(subtype: &windows::core::GUID) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sound::RATE)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(sound::CHANNELS))?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        Ok(t)
    }
}

fn video_type(subtype: &windows::core::GUID, w: u32, h: u32, fps: u32) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        // Rec.709, studio range: what the video processor writes and what
        // players expect of SDR H.264.
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(t)
    }
}

impl Drop for Mp4Writer {
    fn drop(&mut self) {
        if !self.finished {
            let _ = unsafe { self.writer.Finalize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        live::{LiveFormat, LivePlayer},
        record::TICKS_PER_SECOND,
    };
    use std::{
        sync::{Mutex, atomic::AtomicBool},
        time::{Duration, Instant},
    };

    /// What pictures go through: an RGBA texture the test paints, and the
    /// NV12 textures the encoder reads.
    struct Pictures {
        gpu: Gpu,
        rgba: ID3D11Texture2D,
        nv12: Nv12,
        size: (u32, u32, u32),
    }

    impl Pictures {
        fn new(w: u32, h: u32, fps: u32) -> Self {
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.unwrap();
            let gpu = Gpu::video(None).unwrap();
            let rgba = gpu
                .texture(
                    w,
                    h,
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                    D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
                    None,
                )
                .unwrap();
            let nv12 = Nv12::new(&gpu, &rgba, (w, h), (w, h), fps).unwrap();
            Self {
                gpu,
                rgba,
                nv12,
                size: (w, h, fps),
            }
        }

        /// A live writer, with a sound track sent as `sound` says (`feed`
        /// fills it with silence).
        fn writer(&self, sink: LiveSink, sound: SoundCodec) -> Mp4Writer {
            Mp4Writer::new(
                &self.gpu,
                Path::new("unused"),
                self.size,
                true,
                true,
                Some(Live {
                    sink,
                    sound,
                    size: None,
                    bitrate: None,
                }),
            )
            .unwrap()
        }

        /// `count` pictures, each painted by `paint` from its number and
        /// a pixel's x, into `writer` in real time, noting in `written` when
        /// each went in.
        fn feed(
            &self,
            writer: &mut Mp4Writer,
            count: u32,
            written: &Mutex<Vec<Instant>>,
            paint: impl Fn(u32, u32) -> [u8; 4],
        ) {
            let (w, h, fps) = self.size;
            let frame = TICKS_PER_SECOND / i64::from(fps);
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            let silence = vec![0i16; (sound::RATE / fps * 2) as usize];
            let start = Instant::now();
            for n in 0..count {
                let due = Duration::from_nanos(n as u64 * frame as u64 * 100);
                while start.elapsed() < due {
                    std::thread::sleep(Duration::from_micros(500));
                }
                for (i, px) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    *px = paint(n, i as u32 % w);
                }
                unsafe {
                    self.gpu.context.UpdateSubresource(
                        &self.rgba,
                        0,
                        None,
                        pixels.as_ptr().cast(),
                        w * 4,
                        0,
                    );
                }
                let slot = loop {
                    if let Some(slot) = self.nv12.free_slot() {
                        break slot;
                    }
                    std::thread::sleep(Duration::from_micros(200));
                };
                self.nv12.convert(&self.gpu, slot).unwrap();
                written.lock().unwrap().push(Instant::now());
                writer
                    .write(&self.nv12, slot, n as i64 * frame, frame)
                    .unwrap();
                writer.write_sound(&silence, n as i64 * frame).unwrap();
            }
        }
    }

    impl Drop for Pictures {
        fn drop(&mut self) {
            let _ = unsafe { MFShutdown() };
        }
    }

    fn percentiles(mut delays: Vec<Duration>) -> String {
        delays.sort();
        let n = delays.len();
        let ms = |i: usize| delays[i.min(n - 1)].as_millis();
        format!(
            "min {} ms, median {} ms, 90% {} ms, max {} ms",
            ms(0),
            ms(n / 2),
            ms(n * 9 / 10),
            ms(n - 1)
        )
    }

    /// A moving picture at 60 fps, with silence, through the video
    /// processor and the encoders, live, in real time: how long after each
    /// picture goes in its frame comes out of the live stream, and how long
    /// after the last of each sound frame goes in.
    #[test]
    #[ignore = "measures the live path in real time: run by hand"]
    fn live_frames_come_out_soon_after_they_go_in() {
        let (w, fps) = (1280u32, 60u32);
        let pictures = Pictures::new(w, 720, fps);
        let out = Arc::new(Mutex::new(Vec::<(Instant, LiveFrame)>::new()));
        let sink = out.clone();
        let mut writer = pictures.writer(
            Arc::new(move |frame| {
                sink.lock().unwrap().push((Instant::now(), frame));
            }),
            SoundCodec::Aac,
        );
        let frame = TICKS_PER_SECOND / i64::from(fps);
        let count = fps * 4;
        let written = Mutex::new(Vec::new());
        let start = Instant::now();
        pictures.feed(&mut writer, count, &written, |n, x| {
            let bar = (n * 12) % (w - 80);
            let v = if (bar..bar + 80).contains(&x) {
                255
            } else {
                30
            };
            [v, v / 2, 255 - v, 255]
        });
        let written = written.into_inner().unwrap();
        let hardware = writer.finish().unwrap();
        let out = out.lock().unwrap();
        let delays: Vec<Duration> = out
            .iter()
            .filter(|(_, f)| f.track == Track::Video)
            .map(|(at, f)| at.saturating_duration_since(written[(f.time / frame) as usize]))
            .collect();
        let frames = delays.len();
        let keys = out
            .iter()
            .filter(|(_, f)| f.track == Track::Video && f.key)
            .count();
        let bytes: usize = out.iter().map(|(_, f)| f.data.len()).sum();
        println!(
            "hardware encoder {hardware}; {frames} pictures of {count}, {keys} key, {:.1} Mbit/s; fed in {:?}",
            bytes as f64 * 8.0 / 4.0 / 1e6,
            written.last().unwrap().duration_since(start)
        );
        println!("picture in to frame out: {}", percentiles(delays));
        let sound: Vec<Duration> = out
            .iter()
            .filter(|(_, f)| f.track == Track::Sound)
            .filter_map(|(at, f)| {
                let last = ((f.time + f.duration) / frame) as usize;
                Some(at.saturating_duration_since(*written.get(last)?))
            })
            .collect();
        println!(
            "last of a sound frame in to frame out: {}",
            percentiles(sound)
        );
        assert!(frames as u32 >= count - 2, "{frames} of {count}");
        assert!(keys >= 3, "a key frame a second");
    }

    /// Bits of a picture's number, as black and white columns.
    const NUMBER_BITS: u32 = 8;

    /// Picture `n`'s colour at `x` of a `w` wide picture: column `b` is
    /// white where bit `b` of `n` is set.
    fn numbered(n: u32, x: u32, w: u32) -> [u8; 4] {
        let bit = x * NUMBER_BITS / w;
        let v = if n >> bit & 1 == 1 { 235 } else { 20 };
        [v, v, v, 255]
    }

    /// The number a numbered picture carries, from one of its rows (BGRA).
    fn number(row: &[u8], w: u32) -> u32 {
        (0..NUMBER_BITS)
            .filter(|bit| {
                let x = (bit * 2 + 1) * w / (NUMBER_BITS * 2);
                row[x as usize * 4 + 1] > 128
            })
            .fold(0, |n, bit| n | 1 << bit)
    }

    /// What [`watch_numbers`] saw: for each picture drawn, which of the
    /// pictures written it was, how long after it went in it was drawn,
    /// and when; and how far the sound was behind the newest picture.
    #[derive(Default)]
    struct Seen {
        pictures: Vec<(usize, Duration, Instant)>,
        sound_behind: Vec<Duration>,
    }

    /// Each picture `player` draws, read back and its number read, until
    /// `done`. A test polls; a viewer draws once a refresh.
    fn watch_numbers(
        player: &LivePlayer,
        written: &Mutex<Vec<Instant>>,
        done: &AtomicBool,
        (w, h): (u32, u32),
    ) -> Seen {
        let device = player.device();
        let context = unsafe { device.GetImmediateContext() }.unwrap();
        let gpu = Gpu::from_device(device, context).unwrap();
        let texture = gpu
            .texture(
                w,
                h,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
                None,
            )
            .unwrap();
        let mut seen = Seen::default();
        while !done.load(Ordering::Relaxed) {
            if !player.draw(&texture).unwrap() {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            let at = Instant::now();
            if let Some(behind) = player.sound_behind() {
                seen.sound_behind.push(behind);
            }
            let pixels = gpu.read_back(&texture).unwrap();
            let row = (h / 2 * w * 4) as usize;
            let code = number(&pixels[row..row + w as usize * 4], w);
            // The newest picture in with that number.
            let written = written.lock().unwrap();
            if let Some(n) = (0..written.len())
                .rev()
                .find(|&n| n as u32 % (1 << NUMBER_BITS) == code)
            {
                seen.pictures
                    .push((n, at.saturating_duration_since(written[n]), at));
            }
        }
        seen
    }

    /// Numbered pictures at 60 fps, with silence sent as AAC, through the
    /// encoder into the live player, in real time: how long after each
    /// goes in the player shows it. The whole delay of watching but for
    /// capture, the sound's lag and the network.
    #[test]
    #[ignore = "measures the live path in real time: run by hand"]
    fn the_live_player_shows_pictures_soon_after_they_go_in_with_aac() {
        show_numbered(SoundCodec::Aac);
    }

    /// The same with the sound sent as PCM.
    #[test]
    #[ignore = "measures the live path in real time: run by hand"]
    fn the_live_player_shows_pictures_soon_after_they_go_in_with_pcm() {
        show_numbered(SoundCodec::Pcm);
    }

    fn show_numbered(sound: SoundCodec) {
        let (w, h, fps) = (1280u32, 720u32, 60u32);
        let pictures = Pictures::new(w, h, fps);
        let player = Arc::new(
            LivePlayer::new(
                LiveFormat {
                    width: w,
                    height: h,
                    fps,
                    sound: Some(sound),
                },
                |why| eprintln!("the sound failed: {why}"),
            )
            .unwrap_or_else(|e| panic!("{e:#}")),
        );
        player.set_muted(true);
        let fed = player.clone();
        let mut writer = pictures.writer(Arc::new(move |frame| fed.push(frame).unwrap()), sound);
        let written = Arc::new(Mutex::new(Vec::new()));

        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (player, written, done) = (player.clone(), written.clone(), done.clone());
            std::thread::spawn(move || watch_numbers(&player, &written, &done, (w, h)))
        };

        let count = fps * 5;
        let start = Instant::now();
        pictures.feed(&mut writer, count, &written, |n, x| numbered(n, x, w));
        std::thread::sleep(Duration::from_millis(500));
        done.store(true, Ordering::Relaxed);
        let seen = watcher.join().unwrap();
        writer.finish().unwrap();

        let (n, _, at) = seen.pictures.first().expect("the player showed nothing");
        println!(
            "first picture shown {} ms after the first went in (picture {n}); lead {} ms",
            at.duration_since(start).as_millis(),
            player.lead().as_millis()
        );
        if !seen.sound_behind.is_empty() {
            println!(
                "sound behind the newest picture: {}",
                percentiles(seen.sound_behind)
            );
        }
        // Half a second at a time, to see whether the delay settles.
        for half in 0..count / (fps / 2) {
            let range = half * fps / 2..(half + 1) * fps / 2;
            let delays: Vec<Duration> = seen
                .pictures
                .iter()
                .filter(|(n, _, _)| range.contains(&(*n as u32)))
                .map(|(_, d, _)| *d)
                .collect();
            if !delays.is_empty() {
                println!(
                    "{:>4.1} s: {} shown, {}",
                    f64::from(half) / 2.0,
                    delays.len(),
                    percentiles(delays)
                );
            }
        }
        let settled: Vec<Duration> = seen
            .pictures
            .iter()
            .filter(|(n, _, _)| *n as u32 >= fps * 2)
            .map(|(_, d, _)| *d)
            .collect();
        let shown = settled.len() as u32;
        // A picture skipped shows as a stutter.
        let skipped: Vec<usize> = seen
            .pictures
            .windows(2)
            .filter(|pair| pair[1].0 > pair[0].0 + 1)
            .map(|pair| pair[1].0)
            .collect();
        println!("pictures after a skipped one: {skipped:?}");
        println!(
            "picture in to picture shown, from 2 s on: {}",
            percentiles(settled)
        );
        // Three seconds of pictures from 2 s on: nearly all of them shown.
        assert!(
            shown >= fps * 5 / 2,
            "the player showed {shown} of {}",
            fps * 3
        );
    }
}
