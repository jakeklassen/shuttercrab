//! Opus (RFC 6716), through the reference libopus: a live stream's sound,
//! 48 kHz stereo, 10 ms a packet, in about a twelfth of the bits PCM takes.
//! Low-delay mode: the encoder looks 2.5 ms ahead, and nothing else waits.

use anyhow::{Context, Result, bail};
use opus_head_sys as ffi;
use std::{ffi::CStr, ptr::NonNull};

const RATE: i32 = 48_000;
const CHANNELS: usize = 2;
/// Samples per channel in a packet: 10 ms.
pub(crate) const FRAME: usize = 480;
/// Bits a second: plenty for stereo music.
const BITRATE: i32 = 128_000;
/// The most a packet may take, bytes: libopus's own suggestion.
const MOST_BYTES: usize = 4000;
/// The most samples per channel a packet can decode to: 120 ms.
const MOST_SAMPLES: usize = 5760;

/// `code`, or libopus's error for it.
fn check(code: i32) -> Result<i32> {
    if code < 0 {
        // SAFETY: opus_strerror gives a static string for any code.
        let why = unsafe { CStr::from_ptr(ffi::opus_strerror(code)) };
        bail!("Opus: {}", why.to_string_lossy());
    }
    Ok(code)
}

pub(crate) struct Encoder(NonNull<ffi::OpusEncoder>);

// SAFETY: an encoder's state is its own; it is used from one thread at a
// time.
unsafe impl Send for Encoder {}

impl Encoder {
    pub(crate) fn new() -> Result<Self> {
        let mut error = 0;
        // SAFETY: valid arguments; the state is freed in Drop.
        let raw = unsafe {
            ffi::opus_encoder_create(
                RATE,
                CHANNELS as i32,
                ffi::OPUS_APPLICATION_RESTRICTED_LOWDELAY as i32,
                &mut error,
            )
        };
        check(error)?;
        let encoder = Self(NonNull::new(raw).context("no Opus encoder")?);
        // SAFETY: OPUS_SET_BITRATE takes one opus_int32.
        check(unsafe {
            ffi::opus_encoder_ctl(
                encoder.0.as_ptr(),
                ffi::OPUS_SET_BITRATE_REQUEST as i32,
                BITRATE,
            )
        })?;
        Ok(encoder)
    }

    /// How far behind its input the decoded sound comes out, in samples
    /// per channel.
    pub(crate) fn lookahead(&self) -> Result<usize> {
        let mut samples = 0i32;
        // SAFETY: OPUS_GET_LOOKAHEAD writes one opus_int32.
        check(unsafe {
            ffi::opus_encoder_ctl(
                self.0.as_ptr(),
                ffi::OPUS_GET_LOOKAHEAD_REQUEST as i32,
                &raw mut samples,
            )
        })?;
        Ok(samples as usize)
    }

    /// One packet of `samples`: [`FRAME`] of them a channel, interleaved.
    pub(crate) fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        anyhow::ensure!(samples.len() == FRAME * CHANNELS, "not 10 ms of stereo");
        let mut packet = vec![0; MOST_BYTES];
        // SAFETY: `samples` holds FRAME samples a channel; `packet` has
        // room for MOST_BYTES.
        let len = check(unsafe {
            ffi::opus_encode(
                self.0.as_ptr(),
                samples.as_ptr(),
                FRAME as i32,
                packet.as_mut_ptr(),
                MOST_BYTES as i32,
            )
        })?;
        packet.truncate(len as usize);
        Ok(packet)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: made by opus_encoder_create, freed once.
        unsafe { ffi::opus_encoder_destroy(self.0.as_ptr()) };
    }
}

pub(crate) struct Decoder(NonNull<ffi::OpusDecoder>);

// SAFETY: as the encoder.
unsafe impl Send for Decoder {}

impl Decoder {
    pub(crate) fn new() -> Result<Self> {
        let mut error = 0;
        // SAFETY: valid arguments; the state is freed in Drop.
        let raw = unsafe { ffi::opus_decoder_create(RATE, CHANNELS as i32, &mut error) };
        check(error)?;
        Ok(Self(NonNull::new(raw).context("no Opus decoder")?))
    }

    /// The 16-bit stereo samples, interleaved, in `packet`.
    pub(crate) fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>> {
        let mut samples = vec![0; MOST_SAMPLES * CHANNELS];
        // SAFETY: `samples` has room for MOST_SAMPLES a channel.
        let decoded = check(unsafe {
            ffi::opus_decode(
                self.0.as_ptr(),
                packet.as_ptr(),
                packet.len() as i32,
                samples.as_mut_ptr(),
                MOST_SAMPLES as i32,
                0,
            )
        })?;
        samples.truncate(decoded as usize * CHANNELS);
        Ok(samples)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: made by opus_decoder_create, freed once.
        unsafe { ffi::opus_decoder_destroy(self.0.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 440 Hz tone, encoded and decoded: it comes back as a tone of about
    /// its loudness, in about a twelfth of PCM's bytes.
    #[test]
    fn a_tone_comes_back_through_opus_in_far_fewer_bytes() {
        let (mut encoder, mut decoder) = (Encoder::new().unwrap(), Decoder::new().unwrap());
        let tone: Vec<i16> = (0..FRAME * 50)
            .flat_map(|i| {
                let v = (f64::from(i as u32) * 440.0 * std::f64::consts::TAU / 48_000.0).sin();
                let v = (v * 8000.0) as i16;
                [v, v]
            })
            .collect();
        let (mut bytes, mut out) = (0, Vec::new());
        for frame in tone.chunks(FRAME * CHANNELS) {
            let packet = encoder.encode(frame).unwrap();
            bytes += packet.len();
            out.extend(decoder.decode(&packet).unwrap());
        }
        assert_eq!(out.len(), tone.len());
        let pcm = tone.len() * 2;
        assert!(bytes * 8 < pcm, "{bytes} bytes of Opus for {pcm} of PCM");
        // Past the start, as loud as what went in, within a tenth.
        let rms = |s: &[i16]| {
            (s.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
        };
        let (before, after) = (rms(&tone[FRAME * 20..]), rms(&out[FRAME * 20..]));
        assert!(
            (after / before - 1.0).abs() < 0.1,
            "{before} became {after}"
        );
        assert_eq!(encoder.lookahead().unwrap(), 120, "2.5 ms");
    }
}
