//! A live recording's media sink (issue #66, spike S0): where the sink
//! writer delivers each coded frame, instead of a muxer. Each frame goes on
//! to the live sink the moment it arrives.
//!
//! A sink for the sink writer is a media sink with a fixed stream sink per
//! track. Each stream sink asks for samples with `MEStreamSinkRequestSample`
//! events, from when the presentation clock starts, and answers a marker
//! (the end of the recording) with `MEStreamSinkMarker`. H.264 arrives in
//! Annex B; AAC arrives raw, and gets an ADTS header here.

use crate::{
    live::{LiveFrame, Track, VideoCodec, adts},
    record::LiveSink,
};
use std::sync::Mutex;
use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, S_OK},
        Media::MediaFoundation::*,
        System::Com::StructuredStorage::PROPVARIANT,
    },
    core::{GUID, HRESULT, OutRef, Ref, Result, implement},
};
use windows_core::{AsImpl, IUnknownImpl};

/// A media sink with a video stream sink (id 0) and, with `sound`, a sound
/// one (id 1), for coded frames of these types, each sent to `out`.
pub(super) fn create(
    video: &IMFMediaType,
    sound: Option<&IMFMediaType>,
    out: LiveSink,
) -> anyhow::Result<IMFMediaSink> {
    let mut streams = vec![StreamSink::create(0, Track::Video, video, out.clone())?];
    if let Some(sound) = sound {
        streams.push(StreamSink::create(1, Track::Sound, sound, out)?);
    }
    let sink: IMFMediaSink = MediaSink {
        streams: streams.clone(),
        clock: Mutex::new(None),
    }
    .into();
    for stream in &streams {
        let inner: &StreamSink = unsafe { stream.as_impl() };
        *inner.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink.clone());
    }
    Ok(sink)
}

#[implement(IMFMediaSink, IMFClockStateSink)]
struct MediaSink {
    streams: Vec<IMFStreamSink>,
    clock: Mutex<Option<IMFPresentationClock>>,
}

impl MediaSink_Impl {
    /// Every stream sink is told `event`, and then asked for a sample if
    /// it is starting.
    fn tell(&self, event: MF_EVENT_TYPE) -> Result<()> {
        for stream in &self.streams {
            unsafe {
                stream.QueueEvent(event.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null())?;
                if event == MEStreamSinkStarted {
                    stream.QueueEvent(
                        MEStreamSinkRequestSample.0 as u32,
                        &GUID::zeroed(),
                        S_OK,
                        std::ptr::null(),
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl IMFMediaSink_Impl for MediaSink_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        Ok(MEDIASINK_FIXED_STREAMS | MEDIASINK_RATELESS)
    }

    fn AddStreamSink(&self, _: u32, _: Ref<IMFMediaType>) -> Result<IMFStreamSink> {
        Err(MF_E_STREAMSINKS_FIXED.into())
    }

    fn RemoveStreamSink(&self, _: u32) -> Result<()> {
        Err(MF_E_STREAMSINKS_FIXED.into())
    }

    fn GetStreamSinkCount(&self) -> Result<u32> {
        Ok(self.streams.len() as u32)
    }

    fn GetStreamSinkByIndex(&self, index: u32) -> Result<IMFStreamSink> {
        self.streams
            .get(index as usize)
            .cloned()
            .ok_or_else(|| MF_E_INVALIDINDEX.into())
    }

    fn GetStreamSinkById(&self, id: u32) -> Result<IMFStreamSink> {
        self.GetStreamSinkByIndex(id)
            .map_err(|_| MF_E_INVALIDSTREAMNUMBER.into())
    }

    fn SetPresentationClock(&self, clock: Ref<IMFPresentationClock>) -> Result<()> {
        let mut current = self.clock.lock().unwrap_or_else(|e| e.into_inner());
        let me: IMFClockStateSink = self.to_interface();
        if let Some(old) = current.take() {
            unsafe { old.RemoveClockStateSink(&me)? };
        }
        if let Some(clock) = clock.as_ref() {
            unsafe { clock.AddClockStateSink(&me)? };
            *current = Some(clock.clone());
        }
        Ok(())
    }

    fn GetPresentationClock(&self) -> Result<IMFPresentationClock> {
        self.clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| MF_E_NO_CLOCK.into())
    }

    fn Shutdown(&self) -> Result<()> {
        if let Some(clock) = self.clock.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let me: IMFClockStateSink = self.to_interface();
            let _ = unsafe { clock.RemoveClockStateSink(&me) };
        }
        for stream in &self.streams {
            let inner: &StreamSink = unsafe { stream.as_impl() };
            inner.sink.lock().unwrap_or_else(|e| e.into_inner()).take();
            let _ = unsafe { inner.queue.Shutdown() };
        }
        Ok(())
    }
}

impl IMFClockStateSink_Impl for MediaSink_Impl {
    fn OnClockStart(&self, _: i64, _: i64) -> Result<()> {
        self.tell(MEStreamSinkStarted)
    }

    fn OnClockStop(&self, _: i64) -> Result<()> {
        self.tell(MEStreamSinkStopped)
    }

    fn OnClockPause(&self, _: i64) -> Result<()> {
        self.tell(MEStreamSinkPaused)
    }

    fn OnClockRestart(&self, _: i64) -> Result<()> {
        self.tell(MEStreamSinkStarted)
    }

    fn OnClockSetRate(&self, _: i64, _: f32) -> Result<()> {
        Ok(())
    }
}

#[implement(IMFStreamSink, IMFMediaTypeHandler)]
struct StreamSink {
    id: u32,
    track: Track,
    queue: IMFMediaEventQueue,
    media_type: Mutex<IMFMediaType>,
    /// The media sink, until it shuts down.
    sink: Mutex<Option<IMFMediaSink>>,
    out: LiveSink,
}

impl StreamSink {
    fn create(
        id: u32,
        track: Track,
        media_type: &IMFMediaType,
        out: LiveSink,
    ) -> anyhow::Result<IMFStreamSink> {
        Ok(Self {
            id,
            track,
            queue: unsafe { MFCreateEventQueue()? },
            media_type: Mutex::new(media_type.clone()),
            sink: Mutex::new(None),
            out,
        }
        .into())
    }
}

impl StreamSink_Impl {
    fn media_type(&self) -> IMFMediaType {
        self.media_type
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whether `media_type` is coded as this stream is (H.264, AAC): so the
    /// sink writer puts an encoder in front rather than hand over raw
    /// frames.
    fn takes(&self, media_type: &IMFMediaType) -> Result<bool> {
        let ours = self.media_type();
        unsafe {
            Ok(media_type.GetMajorType()? == ours.GetMajorType()?
                && media_type.GetGUID(&MF_MT_SUBTYPE)? == ours.GetGUID(&MF_MT_SUBTYPE)?)
        }
    }

    /// How the pictures are coded.
    fn codec(&self) -> VideoCodec {
        match unsafe { self.media_type().GetGUID(&MF_MT_SUBTYPE) } {
            Ok(subtype) if subtype == MFVideoFormat_HEVC => VideoCodec::Hevc,
            Ok(subtype) if subtype == MFVideoFormat_AV1 => VideoCodec::Av1,
            _ => VideoCodec::H264,
        }
    }

    /// The parameter sets as the encoder's output type has them: Annex B,
    /// or AV1's sequence header OBU, out of the configuration record that
    /// wraps it.
    fn parameter_sets(&self) -> Vec<u8> {
        let media_type = self.media_type();
        unsafe {
            let Ok(size) = media_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) else {
                return Vec::new();
            };
            let mut blob = vec![0; size as usize];
            match media_type.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None) {
                // An AV1CodecConfigurationRecord: four bytes, the first
                // with its marker bit, then the OBUs.
                Ok(()) if self.codec() == VideoCodec::Av1 && blob.first() == Some(&0x81) => {
                    blob.split_off(4.min(blob.len()))
                }
                Ok(()) => blob,
                Err(_) => Vec::new(),
            }
        }
    }

    fn frame(&self, sample: &IMFSample) -> Result<LiveFrame> {
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut length = 0;
            buffer.Lock(&mut data, None, Some(&mut length))?;
            // SAFETY: Lock gives `length` readable bytes at `data` until
            // Unlock.
            let bytes = std::slice::from_raw_parts(data, length as usize).to_vec();
            buffer.Unlock()?;
            let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let data = match self.track {
                Track::Video if key && !has_parameter_sets(&bytes, self.codec()) => {
                    log::debug!("a key frame without its parameter sets: adding them");
                    let mut with = self.parameter_sets();
                    with.extend_from_slice(&bytes);
                    with
                }
                Track::Video => bytes,
                Track::Sound => adts(&bytes),
            };
            Ok(LiveFrame {
                track: self.track,
                time: sample.GetSampleTime()?,
                duration: sample.GetSampleDuration().unwrap_or(0),
                key: key || self.track == Track::Sound,
                data,
            })
        }
    }
}

/// Whether a key frame's `bytes` carry the parameter sets: in Annex B,
/// H.264's sequence parameter set or the video parameter set that leads
/// HEVC's; for AV1, a sequence header OBU.
fn has_parameter_sets(bytes: &[u8], codec: VideoCodec) -> bool {
    let annex_b = |wanted: fn(u8) -> bool| {
        bytes
            .windows(4)
            .any(|w| w[..3] == [0, 0, 1] && wanted(w[3]))
    };
    match codec {
        VideoCodec::H264 => annex_b(|nal| nal & 0x1F == 7),
        VideoCodec::Hevc => annex_b(|nal| (nal >> 1) & 0x3F == 32),
        VideoCodec::Av1 => obus(bytes).any(|kind| kind == OBU_SEQUENCE_HEADER),
    }
}

/// AV1's sequence header OBU type.
const OBU_SEQUENCE_HEADER: u8 = 1;

/// The types of the AV1 OBUs in `bytes`, each with its size, as a live
/// stream has them (AV1 spec 5.3).
fn obus(mut bytes: &[u8]) -> impl Iterator<Item = u8> {
    std::iter::from_fn(move || {
        let header = *bytes.first()?;
        let mut at = 1 + usize::from(header & 4 != 0);
        let size = if header & 2 != 0 {
            // LEB128, at most eight bytes.
            let mut size = 0usize;
            for shift in 0..8 {
                let byte = *bytes.get(at)?;
                at += 1;
                size |= usize::from(byte & 0x7F) << (shift * 7);
                if byte & 0x80 == 0 {
                    break;
                }
            }
            size
        } else {
            bytes.len() - at.min(bytes.len())
        };
        bytes = bytes.get(at + size..).unwrap_or_default();
        Some((header >> 3) & 0xF)
    })
}

impl IMFMediaEventGenerator_Impl for StreamSink_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.queue.GetEvent(flags.0) }
    }

    fn BeginGetEvent(
        &self,
        callback: Ref<IMFAsyncCallback>,
        state: Ref<windows::core::IUnknown>,
    ) -> Result<()> {
        unsafe { self.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }

    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.queue.EndGetEvent(result.as_ref()) }
    }

    fn QueueEvent(
        &self,
        event: u32,
        extended: *const GUID,
        status: HRESULT,
        value: *const PROPVARIANT,
    ) -> Result<()> {
        unsafe {
            self.queue
                .QueueEventParamVar(event, extended, status, value)
        }
    }
}

impl IMFStreamSink_Impl for StreamSink_Impl {
    fn GetMediaSink(&self) -> Result<IMFMediaSink> {
        self.sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn GetIdentifier(&self) -> Result<u32> {
        Ok(self.id)
    }

    fn GetMediaTypeHandler(&self) -> Result<IMFMediaTypeHandler> {
        Ok(self.to_interface())
    }

    fn ProcessSample(&self, sample: Ref<IMFSample>) -> Result<()> {
        let sample = sample
            .as_ref()
            .ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?;
        let frame = self.frame(sample)?;
        (self.out)(frame);
        unsafe {
            self.queue.QueueEventParamVar(
                MEStreamSinkRequestSample.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )
        }
    }

    fn PlaceMarker(
        &self,
        _: MFSTREAMSINK_MARKER_TYPE,
        _: *const PROPVARIANT,
        context: *const PROPVARIANT,
    ) -> Result<()> {
        // Nothing is held back, so every marker is reached at once.
        unsafe {
            self.queue.QueueEventParamVar(
                MEStreamSinkMarker.0 as u32,
                &GUID::zeroed(),
                S_OK,
                context,
            )
        }
    }

    fn Flush(&self) -> Result<()> {
        Ok(())
    }
}

impl IMFMediaTypeHandler_Impl for StreamSink_Impl {
    fn IsMediaTypeSupported(
        &self,
        media_type: Ref<IMFMediaType>,
        closest: OutRef<IMFMediaType>,
    ) -> Result<()> {
        // No closest type to offer; the pointer may be null.
        let _ = closest.write(None);
        let media_type = media_type
            .as_ref()
            .ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?;
        if self.takes(media_type)? {
            Ok(())
        } else {
            Err(MF_E_INVALIDMEDIATYPE.into())
        }
    }

    fn GetMediaTypeCount(&self) -> Result<u32> {
        Ok(1)
    }

    fn GetMediaTypeByIndex(&self, index: u32) -> Result<IMFMediaType> {
        if index == 0 {
            Ok(self.media_type())
        } else {
            Err(MF_E_NO_MORE_TYPES.into())
        }
    }

    fn SetCurrentMediaType(&self, media_type: Ref<IMFMediaType>) -> Result<()> {
        let media_type = media_type
            .as_ref()
            .ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?;
        if !self.takes(media_type)? {
            return Err(MF_E_INVALIDMEDIATYPE.into());
        }
        *self.media_type.lock().unwrap_or_else(|e| e.into_inner()) = media_type.clone();
        Ok(())
    }

    fn GetCurrentMediaType(&self) -> Result<IMFMediaType> {
        Ok(self.media_type())
    }

    fn GetMajorType(&self) -> Result<GUID> {
        unsafe { self.media_type().GetMajorType() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_frame_says_whether_it_carries_its_parameter_sets() {
        // H.264: a sequence parameter set (7), then a picture (5).
        let h264 = [0, 0, 0, 1, 0x67, 9, 0, 0, 1, 0x65, 9];
        assert!(has_parameter_sets(&h264, VideoCodec::H264));
        assert!(!has_parameter_sets(&h264[5..], VideoCodec::H264));
        // HEVC: a video parameter set (32) leads.
        let hevc = [0, 0, 0, 1, 32 << 1, 1, 0, 0, 1, 19 << 1, 1];
        assert!(has_parameter_sets(&hevc, VideoCodec::Hevc));
        assert!(!has_parameter_sets(&hevc[6..], VideoCodec::Hevc));
        // AV1: a temporal delimiter (2), a sequence header (1) of three
        // bytes, a frame (6) of 200 bytes, its size in two LEB128 bytes.
        let mut av1 = vec![2 << 3 | 2, 0, 1 << 3 | 2, 3, 7, 7, 7, 6 << 3 | 2, 0xC8, 1];
        av1.extend([0; 200]);
        assert_eq!(obus(&av1).collect::<Vec<_>>(), [2, 1, 6]);
        assert!(has_parameter_sets(&av1, VideoCodec::Av1));
        let without: Vec<u8> = [&av1[..2], &av1[7..]].concat();
        assert_eq!(obus(&without).collect::<Vec<_>>(), [2, 6]);
        assert!(!has_parameter_sets(&without, VideoCodec::Av1));
    }
}
