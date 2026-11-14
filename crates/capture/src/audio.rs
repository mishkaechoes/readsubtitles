//! Listening to the film's own soundtrack.
//!
//! Subtitles say *what* was said. They almost never say *who* said it: speaker
//! labels live on closed-caption tracks, and a closed-caption track only ever
//! exists in a title's original language. For a Japanese film watched with
//! English subtitles - which is the case this whole project exists for - there
//! is no label to read, in either track.
//!
//! The voice itself is the remaining signal. It is in the soundtrack, it is
//! there in every language, and it does not depend on what any subtitler chose
//! to type.
//!
//! Two properties of ScreenCaptureKit make this far cheaper than it sounds:
//!
//! 1. Audio rides the **same TCC grant** as screen capture. No second
//!    permission dialog, which matters enormously when the person using this
//!    cannot see a dialog to approve it.
//! 2. It will deliver **16 kHz mono** on request, which is exactly what speaker
//!    embedding models want, so nothing has to be resampled or downmixed.
//!
//! `excludes_current_process_audio` keeps our own spoken output out of the
//! capture. Without it the app would hear itself and treat its own voice as a
//! character in the film.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use screencapturekit::prelude::*;
use screencapturekit::stream::configuration::{AudioChannelCount, AudioSampleRate};

use crate::Target;

/// What the embedding models expect, and what we ask the compositor for.
pub const SAMPLE_RATE: u32 = 16_000;

/// How much recent audio to keep.
///
/// A speaker embedding needs a second or two of voiced audio. Eight seconds
/// leaves room to look backwards from a subtitle that appeared slightly after
/// the line was spoken, without holding a pointless amount of memory: at 16 kHz
/// mono this is half a megabyte.
const RING_SECONDS: usize = 8;

#[derive(Debug, Default)]
pub struct AudioStats {
    pub buffers: AtomicU64,
    pub samples: AtomicU64,
    /// Buffers that arrived in a layout we could not read, which would show up
    /// as silence rather than as an error.
    pub unreadable: AtomicU64,
}

/// A fixed-size window onto the most recent audio.
///
/// A ring rather than a channel because the consumer does not want every
/// sample; it wants "the last second and a half, as of now", whenever a
/// subtitle happens to appear.
pub struct AudioRing {
    samples: Mutex<Vec<f32>>,
    /// Where the next sample goes. Wraps.
    head: Mutex<usize>,
    filled: Mutex<usize>,
    capacity: usize,
}

impl AudioRing {
    #[must_use]
    pub fn new() -> Self {
        let capacity = SAMPLE_RATE as usize * RING_SECONDS;
        Self {
            samples: Mutex::new(vec![0.0; capacity]),
            head: Mutex::new(0),
            filled: Mutex::new(0),
            capacity,
        }
    }

    fn push(&self, incoming: &[f32]) {
        let Ok(mut buf) = self.samples.lock() else {
            return;
        };
        let (Ok(mut head), Ok(mut filled)) = (self.head.lock(), self.filled.lock()) else {
            return;
        };
        for &s in incoming {
            buf[*head] = s;
            *head = (*head + 1) % self.capacity;
            if *filled < self.capacity {
                *filled += 1;
            }
        }
    }

    /// The most recent `seconds` of audio, oldest first.
    ///
    /// Returns less than asked for when the ring has not filled yet, which
    /// happens for the first few seconds after starting.
    #[must_use]
    pub fn recent(&self, seconds: f64) -> Vec<f32> {
        self.window(seconds, 0.0)
    }

    /// `seconds` of audio ending `ago` seconds before now, oldest first.
    ///
    /// The offset exists because a subtitle and the line it transcribes do not
    /// appear at the same instant; being able to look slightly back is what
    /// lets the caller line the two up.
    #[must_use]
    pub fn window(&self, seconds: f64, ago: f64) -> Vec<f32> {
        let (Ok(buf), Ok(head), Ok(filled)) =
            (self.samples.lock(), self.head.lock(), self.filled.lock())
        else {
            return Vec::new();
        };

        let skip = (ago.max(0.0) * f64::from(SAMPLE_RATE)) as usize;
        let want = (seconds.max(0.0) * f64::from(SAMPLE_RATE)) as usize;
        let available = (*filled).saturating_sub(skip);
        let take = want.min(available);
        if take == 0 {
            return Vec::new();
        }

        // Walk backwards from the write head, past the skipped tail.
        let end = (*head + self.capacity - skip) % self.capacity;
        let start = (end + self.capacity - take) % self.capacity;

        let mut out = Vec::with_capacity(take);
        if start < end {
            out.extend_from_slice(&buf[start..end]);
        } else {
            out.extend_from_slice(&buf[start..]);
            out.extend_from_slice(&buf[..end]);
        }
        out
    }

    /// Root mean square of the most recent `seconds`, as a rough loudness.
    #[must_use]
    pub fn rms(&self, seconds: f64) -> f32 {
        let w = self.recent(seconds);
        if w.is_empty() {
            return 0.0;
        }
        (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt()
    }
}

impl Default for AudioRing {
    fn default() -> Self {
        Self::new()
    }
}

/// A running audio capture. Dropping it stops the stream.
pub struct AudioCapture {
    stream: SCStream,
    pub ring: Arc<AudioRing>,
    pub stats: Arc<AudioStats>,
}

impl AudioCapture {
    /// Start capturing system audio, excluding our own.
    ///
    /// `target` only decides which display's audio to take; the capture is not
    /// cropped to a region the way the video one is.
    pub fn start(target: &Target) -> Result<Self, Box<dyn std::error::Error>> {
        let content = SCShareableContent::get()?;
        let displays = content.displays();
        let display = match target {
            Target::Display(i) => displays.get(*i).cloned(),
            Target::WindowTitle(_) => displays.first().cloned(),
        }
        .ok_or("no displays found")?;

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build()?;

        // A video size is still required even when only audio is wanted, so ask
        // for the smallest one that is accepted and ignore the frames.
        let config = SCStreamConfiguration::new()
            .with_width(2)
            .with_height(2)
            .with_fps(1)
            .with_captures_audio(true)
            .with_sample_rate(AudioSampleRate::Rate16000)
            .with_channel_count(AudioChannelCount::Mono)
            // Without this the app hears its own spoken output and treats it as
            // another character.
            .with_excludes_current_process_audio(true);

        let ring = Arc::new(AudioRing::new());
        let stats = Arc::new(AudioStats::default());

        let (r, s) = (ring.clone(), stats.clone());
        let mut stream = SCStream::new(&filter, &config)?;
        stream.add_output_handler(
            move |sample: CMSampleBuffer, kind: SCStreamOutputType| {
                if kind != SCStreamOutputType::Audio {
                    return;
                }
                s.buffers.fetch_add(1, Ordering::Relaxed);

                let Ok(list) = sample.audio_buffer_list() else {
                    s.unreadable.fetch_add(1, Ordering::Relaxed);
                    return;
                };
                let Some(buffer) = list.buffer(0) else {
                    s.unreadable.fetch_add(1, Ordering::Relaxed);
                    return;
                };

                // The compositor hands back raw bytes; at the format we asked
                // for they are native-endian f32.
                let bytes = buffer.data();
                if bytes.len() < 4 {
                    return;
                }
                let mono: Vec<f32> = bytes
                    .chunks_exact(4)
                    .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();

                s.samples.fetch_add(mono.len() as u64, Ordering::Relaxed);
                r.push(&mono);
            },
            SCStreamOutputType::Audio,
        )?;
        stream.start_capture()?;

        Ok(Self {
            stream,
            ring,
            stats,
        })
    }

    pub fn stop(&mut self) {
        let _ = self.stream.stop_capture();
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_ring_yields_nothing_rather_than_silence_shaped_data() {
        // Returning zeros would look like captured silence to the caller and
        // get embedded as if it were a voice.
        let ring = AudioRing::new();
        assert!(ring.recent(1.0).is_empty());
        assert_eq!(ring.rms(1.0), 0.0);
    }

    #[test]
    fn recent_audio_comes_back_oldest_first() {
        let ring = AudioRing::new();
        ring.push(&[1.0, 2.0, 3.0, 4.0]);
        let got = ring.recent(4.0 / f64::from(SAMPLE_RATE));
        assert_eq!(got, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn asking_for_more_than_exists_returns_what_there_is() {
        let ring = AudioRing::new();
        ring.push(&[1.0, 2.0]);
        assert_eq!(ring.recent(5.0), [1.0, 2.0]);
    }

    #[test]
    fn the_window_offset_looks_backwards_in_time() {
        // A subtitle can appear after the line was spoken, so the caller needs
        // to reach back past the most recent audio.
        let ring = AudioRing::new();
        ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let one = 1.0 / f64::from(SAMPLE_RATE);
        assert_eq!(ring.window(2.0 * one, 0.0), [4.0, 5.0]);
        assert_eq!(ring.window(2.0 * one, 2.0 * one), [2.0, 3.0]);
    }

    #[test]
    fn writing_past_the_end_keeps_the_newest_audio() {
        let ring = AudioRing::new();
        let flood: Vec<f32> = (0..ring.capacity + 100).map(|i| i as f32).collect();
        ring.push(&flood);
        let last = ring.recent(1.0 / f64::from(SAMPLE_RATE));
        assert_eq!(last, [(ring.capacity + 99) as f32]);
    }

    #[test]
    fn a_full_ring_reads_back_in_order_across_the_wrap() {
        let ring = AudioRing::new();
        let flood: Vec<f32> = (0..ring.capacity + 5).map(|i| i as f32).collect();
        ring.push(&flood);
        let got = ring.recent(3.0 / f64::from(SAMPLE_RATE));
        // The flood ends at capacity + 4, so these are the final three values.
        assert_eq!(
            got,
            [
                (ring.capacity + 2) as f32,
                (ring.capacity + 3) as f32,
                (ring.capacity + 4) as f32,
            ]
        );
    }

    #[test]
    fn loudness_rises_with_signal() {
        let quiet = AudioRing::new();
        quiet.push(&vec![0.01; 1000]);
        let loud = AudioRing::new();
        loud.push(&vec![0.5; 1000]);
        assert!(loud.rms(1.0) > quiet.rms(1.0) * 10.0);
    }
}
