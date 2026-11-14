//! Mel filterbank features, the form the speaker model was trained on.
//!
//! The model takes `feats` of shape `[batch, frames, 80]`, not a waveform, so
//! the audio has to be turned into exactly the features WeSpeaker used during
//! training. Those come from Kaldi by way of
//! `torchaudio.compliance.kaldi.fbank`, and the details are not negotiable: a
//! window function or a mel scale that is merely close produces features that
//! look entirely reasonable and embeddings that cluster badly, which is a
//! miserable thing to debug from the far end.
//!
//! So this follows Kaldi deliberately and exactly:
//!
//! - 25 ms frames every 10 ms, edges snipped rather than padded
//! - DC offset removed per frame, then pre-emphasis at 0.97
//! - a Povey window, which is a Hann window raised to 0.85
//! - padded to 512 points, power spectrum, 80 triangular mel bands from 20 Hz
//!   to Nyquist on Kaldi's own mel scale
//! - natural log, floored
//! - cepstral mean normalisation over time, which is what makes the result
//!   robust to the particular mix and playback level of a given film

use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// Mel bands. The model's input dimension, so not a free choice.
pub const NUM_BINS: usize = 80;
/// 25 ms at 16 kHz.
pub const FRAME_LENGTH: usize = 400;
/// 10 ms at 16 kHz.
pub const FRAME_SHIFT: usize = 160;
/// `FRAME_LENGTH` rounded up to a power of two, as Kaldi does.
pub const FFT_SIZE: usize = 512;

const SAMPLE_RATE: f32 = 16_000.0;
const PREEMPHASIS: f32 = 0.97;
const LOW_FREQ: f32 = 20.0;
/// Kaldi floors the log at the float epsilon rather than at zero.
const LOG_FLOOR: f32 = 1.192_092_9e-7;

/// Kaldi's mel scale. Note 1127, not the 2595/log10 form used elsewhere; they
/// are the same curve but the constants must match what the model saw.
fn mel(freq: f32) -> f32 {
    1127.0 * (1.0 + freq / 700.0).ln()
}

/// Computes filterbank features, reusing its FFT plan and filters.
pub struct Fbank {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// For each mel band, the first FFT bin it covers and its weights.
    filters: Vec<(usize, Vec<f32>)>,
}

impl Fbank {
    #[must_use]
    pub fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);

        // Povey: a Hann window raised to 0.85. Kaldi's default, and a different
        // window changes every feature slightly.
        let window: Vec<f32> = (0..FRAME_LENGTH)
            .map(|i| {
                let hann = 0.5 - 0.5 * (2.0 * PI * i as f32 / (FRAME_LENGTH as f32 - 1.0)).cos();
                hann.powf(0.85)
            })
            .collect();

        Self {
            fft,
            window,
            filters: mel_filters(),
        }
    }

    /// How many frames `samples` will produce.
    ///
    /// Kaldi's `snip_edges` behaviour: frames that would run off the end are
    /// dropped rather than zero-padded.
    #[must_use]
    pub fn frame_count(samples: usize) -> usize {
        if samples < FRAME_LENGTH {
            0
        } else {
            1 + (samples - FRAME_LENGTH) / FRAME_SHIFT
        }
    }

    /// Features for `samples`, as `frames * NUM_BINS` values in row-major order.
    ///
    /// Returns an empty vector when there is not a single whole frame, which
    /// the caller must treat as "cannot tell" rather than as silence.
    #[must_use]
    pub fn compute(&self, samples: &[f32]) -> Vec<f32> {
        let frames = Self::frame_count(samples.len());
        if frames == 0 {
            return Vec::new();
        }

        let mut out = Vec::with_capacity(frames * NUM_BINS);
        let mut buffer = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
        let mut frame = vec![0.0f32; FRAME_LENGTH];

        for f in 0..frames {
            let start = f * FRAME_SHIFT;
            frame.copy_from_slice(&samples[start..start + FRAME_LENGTH]);

            // Remove the DC offset before pre-emphasis, as Kaldi does; doing it
            // afterwards gives measurably different low bands.
            let mean = frame.iter().sum::<f32>() / FRAME_LENGTH as f32;
            for s in frame.iter_mut() {
                *s -= mean;
            }

            // Pre-emphasis, walked backwards so each sample sees its original
            // predecessor. The first sample uses itself, which is Kaldi's edge
            // convention.
            for i in (1..FRAME_LENGTH).rev() {
                frame[i] -= PREEMPHASIS * frame[i - 1];
            }
            frame[0] -= PREEMPHASIS * frame[0];

            for (i, slot) in buffer.iter_mut().enumerate() {
                *slot = if i < FRAME_LENGTH {
                    Complex32::new(frame[i] * self.window[i], 0.0)
                } else {
                    Complex32::new(0.0, 0.0)
                };
            }
            self.fft.process(&mut buffer);

            // Power spectrum over the first half; Kaldi uses FFT_SIZE/2 bins.
            let power: Vec<f32> = buffer[..FFT_SIZE / 2]
                .iter()
                .map(|c| c.re * c.re + c.im * c.im)
                .collect();

            for (offset, weights) in &self.filters {
                let energy: f32 = weights
                    .iter()
                    .enumerate()
                    .map(|(i, w)| w * power.get(offset + i).copied().unwrap_or(0.0))
                    .sum();
                out.push(energy.max(LOG_FLOOR).ln());
            }
        }

        cepstral_mean_normalise(&mut out, frames);
        out
    }
}

impl Default for Fbank {
    fn default() -> Self {
        Self::new()
    }
}

/// Subtract each band's mean across time.
///
/// This is what makes the features independent of the overall level and tone of
/// a particular film's mix, which matters here because the audio comes from
/// whatever the player happened to output rather than from a clean recording.
fn cepstral_mean_normalise(feats: &mut [f32], frames: usize) {
    if frames == 0 {
        return;
    }
    for bin in 0..NUM_BINS {
        let mut sum = 0.0;
        for f in 0..frames {
            sum += feats[f * NUM_BINS + bin];
        }
        let mean = sum / frames as f32;
        for f in 0..frames {
            feats[f * NUM_BINS + bin] -= mean;
        }
    }
}

/// Triangular mel filters, as Kaldi lays them out.
fn mel_filters() -> Vec<(usize, Vec<f32>)> {
    let num_fft_bins = FFT_SIZE / 2;
    let fft_bin_width = SAMPLE_RATE / FFT_SIZE as f32;
    let high_freq = SAMPLE_RATE / 2.0;

    let mel_low = mel(LOW_FREQ);
    let mel_high = mel(high_freq);
    let delta = (mel_high - mel_low) / (NUM_BINS + 1) as f32;

    (0..NUM_BINS)
        .map(|bin| {
            let left = mel_low + bin as f32 * delta;
            let centre = left + delta;
            let right = left + 2.0 * delta;

            let mut first = None;
            let mut weights = Vec::new();
            for i in 0..num_fft_bins {
                let m = mel(fft_bin_width * i as f32);
                if m > left && m < right {
                    let w = if m <= centre {
                        (m - left) / (centre - left)
                    } else {
                        (right - m) / (right - centre)
                    };
                    if first.is_none() {
                        first = Some(i);
                    }
                    weights.push(w);
                } else if first.is_some() {
                    break;
                }
            }
            (first.unwrap_or(0), weights)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| (2.0 * PI * freq * i as f32 / SAMPLE_RATE).sin() * 0.3)
            .collect()
    }

    #[test]
    fn frame_counts_follow_kaldis_snip_edges_rule() {
        assert_eq!(Fbank::frame_count(0), 0);
        assert_eq!(Fbank::frame_count(399), 0, "a partial frame is dropped");
        assert_eq!(Fbank::frame_count(400), 1);
        assert_eq!(Fbank::frame_count(559), 1, "not yet a second whole frame");
        assert_eq!(Fbank::frame_count(560), 2);
        // One second of audio.
        assert_eq!(Fbank::frame_count(16_000), 1 + (16_000 - 400) / 160);
    }

    #[test]
    fn too_little_audio_yields_nothing_rather_than_a_partial_frame() {
        // The caller must be able to tell "no features" from "silent features".
        assert!(Fbank::new().compute(&[0.0; 100]).is_empty());
    }

    #[test]
    fn output_is_frames_by_eighty() {
        let f = Fbank::new();
        let feats = f.compute(&tone(440.0, 16_000));
        assert_eq!(feats.len(), Fbank::frame_count(16_000) * NUM_BINS);
    }

    #[test]
    fn every_feature_is_finite_even_for_pure_silence() {
        // log(0) would be -inf and would poison the whole embedding.
        let f = Fbank::new();
        let feats = f.compute(&vec![0.0; 16_000]);
        assert!(!feats.is_empty());
        assert!(
            feats.iter().all(|x| x.is_finite()),
            "silence produced non-finite features"
        );
    }

    #[test]
    fn there_are_eighty_filters_and_none_is_empty() {
        let filters = mel_filters();
        assert_eq!(filters.len(), NUM_BINS);
        for (i, (_, weights)) in filters.iter().enumerate() {
            assert!(!weights.is_empty(), "mel band {i} covers no FFT bin");
        }
    }

    #[test]
    fn filters_climb_the_spectrum_in_order() {
        let filters = mel_filters();
        for pair in filters.windows(2) {
            assert!(
                pair[1].0 >= pair[0].0,
                "mel bands are not in increasing frequency order"
            );
        }
    }

    #[test]
    fn mel_filters_stay_inside_the_spectrum() {
        for (offset, weights) in mel_filters() {
            assert!(
                offset + weights.len() <= FFT_SIZE / 2,
                "a filter runs past the end of the power spectrum"
            );
        }
    }

    #[test]
    fn a_high_tone_excites_higher_bands_than_a_low_tone() {
        // The sanity check that the mel mapping is the right way round.
        let f = Fbank::new();
        let loudest = |freq: f32| {
            let feats = f.compute(&tone(freq, 16_000));
            // Look at one frame in the middle, away from the edges.
            let frame = feats.len() / NUM_BINS / 2;
            let row = &feats[frame * NUM_BINS..(frame + 1) * NUM_BINS];
            row.iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i)
                .unwrap()
        };
        assert!(
            loudest(3000.0) > loudest(300.0),
            "a 3 kHz tone did not land above a 300 Hz tone"
        );
    }

    #[test]
    fn mean_normalisation_centres_each_band() {
        let f = Fbank::new();
        let feats = f.compute(&tone(440.0, 16_000));
        let frames = feats.len() / NUM_BINS;
        for bin in 0..NUM_BINS {
            let mean: f32 = (0..frames)
                .map(|fr| feats[fr * NUM_BINS + bin])
                .sum::<f32>()
                / frames as f32;
            assert!(mean.abs() < 1e-3, "band {bin} has mean {mean}, not ~0");
        }
    }

    #[test]
    fn the_same_audio_gives_the_same_features_every_time() {
        // Any dithering or uninitialised state would show up here, and would
        // make a speaker's embedding wander between scenes.
        let f = Fbank::new();
        let audio = tone(440.0, 8_000);
        assert_eq!(f.compute(&audio), f.compute(&audio));
    }

    /// Deterministic broadband noise, standing in for speech.
    ///
    /// Speech occupies the whole spectrum; a pure tone does not, and the
    /// difference matters for the test below.
    fn broadband(samples: usize) -> Vec<f32> {
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        (0..samples)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 40) as f32 / 8_388_608.0 - 0.15
            })
            .collect()
    }

    #[test]
    fn mean_normalisation_makes_features_independent_of_playback_level() {
        // The whole point of CMN here: the same dialogue at a different volume
        // must embed to the same voice, so the film's mix and the viewer's
        // volume setting do not change who the model thinks is speaking.
        //
        // This holds for broadband sound such as speech. It does not hold for a
        // signal that leaves most mel bands at the log floor, because raising
        // the level lifts those bands off the floor by differing amounts; a
        // pure tone behaves that way, real dialogue does not.
        let f = Fbank::new();
        let quiet = broadband(16_000);
        let loud: Vec<f32> = quiet.iter().map(|s| s * 4.0).collect();
        let a = f.compute(&quiet);
        let b = f.compute(&loud);
        let worst = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0_f32, f32::max);
        assert!(worst < 1e-2, "level change moved features by {worst}");
    }

    #[test]
    fn kaldis_mel_scale_constants_are_what_the_model_was_trained_on() {
        assert!((mel(0.0) - 0.0).abs() < 1e-6);
        // 1127 * ln(1 + 1000/700)
        let expected = 1127.0_f32 * (1.0_f32 + 1000.0_f32 / 700.0_f32).ln();
        assert!((mel(1000.0) - expected).abs() < 1e-3);
        assert!(mel(8000.0) > mel(4000.0));
    }
}
