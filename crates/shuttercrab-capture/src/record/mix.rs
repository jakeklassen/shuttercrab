//! A recording's sound track, for every backend: the sources' sound (what
//! the speakers play, a microphone) mixed into one, each packet placed
//! where its time says. A source's gaps stay silent, overlaps are trimmed,
//! and the track is written out a little behind now, so sound never drifts
//! from the picture.

use super::{Source, TICKS_PER_SECOND};

/// The track's sample rate, frames a second.
pub(crate) const RATE: u32 = 48_000;

/// The track's channels: stereo.
pub(crate) const CHANNELS: u16 = 2;

impl Source {
    fn index(self) -> usize {
        match self {
            Source::System => 0,
            Source::Microphone => 1,
        }
    }
}

/// Within this, ticks, a packet counts as on time after the one before it
/// from the same source: placed right after it, nothing added or trimmed.
const TOLERANCE: i64 = TICKS_PER_SECOND / 50;

/// The sources' sound mixed into one track, held from where the track
/// written so far ends until it is written out.
#[derive(Debug, Default)]
pub(crate) struct Mixer {
    /// Frames written out so far.
    written: u64,
    /// Mixed samples from `written` on, the channels interleaved: wide, so
    /// the sources add up before they are clipped.
    pending: Vec<i32>,
    /// Where each source's sound so far ends, frames.
    ends: [u64; 2],
    /// Each source's frames left out because they came too late.
    late: [u64; 2],
}

impl Mixer {
    /// Frames written out so far.
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// How much of `source`'s sound came too late to mix, ticks.
    pub(crate) fn late(&self, source: Source) -> i64 {
        frames_to_ticks(self.late[source.index()])
    }

    /// Mix `samples` from `source`, captured at `time` (ticks on the
    /// recording's timeline): right after that source's last packet if on
    /// time, after silence if it came late, trimmed if early. Sound for
    /// what is written out already is left out.
    pub(crate) fn add(&mut self, source: Source, time: i64, samples: &[i16]) {
        let channels = usize::from(CHANNELS);
        let frames = (samples.len() / channels) as u64;
        let end = &mut self.ends[source.index()];
        let gap = time - frames_to_ticks(*end);
        // Where what is kept begins, and how much of the start is trimmed.
        let (start, trim) = if gap > TOLERANCE {
            (ticks_to_frames(time), 0)
        } else if gap < -TOLERANCE {
            (*end, ticks_to_frames(-gap).min(frames))
        } else {
            (*end, 0)
        };
        *end = start + (frames - trim);
        // What falls on what is written out already is too late to mix.
        let late = self.written.saturating_sub(start).min(frames - trim);
        self.late[source.index()] += late;
        let from = (start + late - self.written) as usize * channels;
        let kept = &samples[(trim + late) as usize * channels..];
        if self.pending.len() < from + kept.len() {
            self.pending.resize(from + kept.len(), 0);
        }
        for (mixed, &sample) in self.pending[from..].iter_mut().zip(kept) {
            *mixed += i32::from(sample);
        }
    }

    /// The mix up to `time` (ticks), to write out from frame
    /// [`Mixer::written`]: silence where no source had sound. Empty if the
    /// track reaches `time` already.
    pub(crate) fn take_until(&mut self, time: i64) -> Vec<i16> {
        let until = ticks_to_frames(time);
        if until <= self.written {
            return Vec::new();
        }
        let count = (until - self.written) as usize * usize::from(CHANNELS);
        if self.pending.len() < count {
            self.pending.resize(count, 0);
        }
        self.written = until;
        self.pending
            .drain(..count)
            .map(|sample| sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16)
            .collect()
    }
}

/// `frames` of sound at the track's rate, as 100 ns ticks.
pub(crate) fn frames_to_ticks(frames: u64) -> i64 {
    (frames as i128 * TICKS_PER_SECOND as i128 / i128::from(RATE)) as i64
}

/// `ticks` (none if negative) as whole frames at the track's rate.
fn ticks_to_frames(ticks: i64) -> u64 {
    (ticks.max(0) as i128 * i128::from(RATE) / TICKS_PER_SECOND as i128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tenth of a second, frames and ticks.
    const TENTH: usize = RATE as usize / 10;
    const TENTH_TICKS: i64 = TICKS_PER_SECOND / 10;

    /// A tenth of a second of one sample value, both channels.
    fn tone(value: i16) -> Vec<i16> {
        vec![value; TENTH * usize::from(CHANNELS)]
    }

    #[test]
    fn packets_on_time_follow_one_another() {
        let mut mixer = Mixer::default();
        mixer.add(Source::System, 0, &tone(1));
        // A little late, or a little early: still right after.
        mixer.add(Source::System, TENTH_TICKS + 1_000, &tone(2));
        mixer.add(Source::System, 2 * TENTH_TICKS - 1_000, &tone(3));
        let out = mixer.take_until(3 * TENTH_TICKS);
        assert_eq!(out.len(), 3 * TENTH * 2);
        assert_eq!((out[0], out[TENTH * 2], out[2 * TENTH * 2]), (1, 2, 3));
        assert_eq!(mixer.written(), 3 * TENTH as u64);
    }

    #[test]
    fn a_gap_is_silent_and_an_overlap_is_trimmed() {
        let mut mixer = Mixer::default();
        mixer.add(Source::System, 0, &tone(1));
        // Nothing played for a tenth: silence, then the next.
        mixer.add(Source::System, 2 * TENTH_TICKS, &tone(2));
        // Half a tenth early: its first half is left out.
        mixer.add(Source::System, 3 * TENTH_TICKS - TENTH_TICKS / 2, &tone(3));
        let out = mixer.take_until(4 * TENTH_TICKS);
        let at = |tenths: f32| out[(tenths * TENTH as f32) as usize * 2];
        assert_eq!(
            (at(0.5), at(1.5), at(2.5), at(3.25), at(3.75)),
            (1, 0, 2, 3, 0)
        );
    }

    #[test]
    fn two_sources_add_up_and_clip() {
        let mut mixer = Mixer::default();
        mixer.add(Source::System, 0, &tone(1_000));
        mixer.add(Source::Microphone, 0, &tone(500));
        mixer.add(Source::System, TENTH_TICKS, &tone(30_000));
        mixer.add(Source::Microphone, TENTH_TICKS, &tone(30_000));
        let out = mixer.take_until(2 * TENTH_TICKS);
        assert_eq!((out[0], out[TENTH * 2]), (1_500, i16::MAX));
    }

    #[test]
    fn the_track_is_silent_where_no_source_has_sound_and_late_sound_is_left_out() {
        let mut mixer = Mixer::default();
        assert_eq!(
            mixer.take_until(TICKS_PER_SECOND),
            vec![0; RATE as usize * 2]
        );
        assert!(mixer.take_until(TICKS_PER_SECOND / 2).is_empty());
        // Sound for what is written out already: only what comes after.
        mixer.add(
            Source::Microphone,
            TICKS_PER_SECOND - TENTH_TICKS / 2,
            &tone(7),
        );
        let out = mixer.take_until(TICKS_PER_SECOND + TENTH_TICKS);
        assert_eq!((out[0], out[TENTH / 2 * 2], out[TENTH * 2 - 1]), (7, 0, 0));
    }
}
