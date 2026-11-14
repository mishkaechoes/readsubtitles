//! Estimating how high or low a voice is.
//!
//! Knowing that two lines come from different people is only half of what is
//! needed. The other half is picking a voice that sounds like them: a woman
//! read in a man's voice is actively misleading, and worse than one voice for
//! everyone, because the listener draws a conclusion and it is wrong.
//!
//! The speaker embedding cannot answer this. It encodes identity in a space
//! with no axis that means "higher" or "lower", so nothing in it says which of
//! the available voices would suit. Fundamental frequency does say that, it is
//! measurable from the same audio window, and it is the strongest single cue
//! separating adult voices.
//!
//! Autocorrelation is used rather than anything cleverer because the question
//! is coarse. Telling roughly 110 Hz from roughly 200 Hz survives music beds,
//! compression and a noisy estimate; what would not survive is a method that
//! needs clean speech.

/// The range of human speaking pitch worth looking in.
///
/// Adult speech sits roughly between these. Looking wider invites octave
/// errors, where half or double the true pitch scores better than the truth.
const MIN_HZ: f32 = 70.0;
const MAX_HZ: f32 = 320.0;

/// Analysis frame, long enough to hold several periods of the lowest pitch.
const FRAME: usize = 1024;
const HOP: usize = 512;

/// How peaked the autocorrelation must be to count as voiced.
///
/// Music and noise produce a flat correlation with no clear period. Demanding a
/// strong peak is what stops a soundtrack being assigned a pitch.
const VOICING_THRESHOLD: f32 = 0.35;

/// Roughly where adult voices divide.
///
/// Deliberately a band rather than a line: between these, the evidence does not
/// support a confident choice, and guessing is worse than not choosing.
const LOWER_MAX_HZ: f32 = 155.0;
const HIGHER_MIN_HZ: f32 = 175.0;

/// Which kind of voice would suit a speaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Register {
    /// A lower voice, typically an adult man.
    Lower,
    /// A higher voice, typically an adult woman or a child.
    Higher,
    /// Not confident enough to say, so the caller should not guess.
    Unclear,
}

impl Register {
    /// Classify a pitch in hertz.
    #[must_use]
    pub fn from_hz(hz: f32) -> Self {
        if hz <= 0.0 || !hz.is_finite() {
            Self::Unclear
        } else if hz <= LOWER_MAX_HZ {
            Self::Lower
        } else if hz >= HIGHER_MIN_HZ {
            Self::Higher
        } else {
            Self::Unclear
        }
    }
}

/// The median pitch of the voiced parts of `samples`, in hertz.
///
/// Median rather than mean because pitch estimates produce occasional wild
/// outliers, usually at half or double the true value, and one of those would
/// drag a mean across the boundary between registers.
///
/// Returns `None` when too little of the window is voiced, which is the honest
/// answer for music, silence or a sound effect.
#[must_use]
pub fn median_pitch(samples: &[f32], sample_rate: u32) -> Option<f32> {
    let rate = sample_rate as f32;
    let min_lag = (rate / MAX_HZ) as usize;
    let max_lag = (rate / MIN_HZ) as usize;
    if samples.len() < FRAME || max_lag >= FRAME {
        return None;
    }

    let mut voiced = Vec::new();
    let mut frames = 0usize;
    let mut start = 0;
    while start + FRAME <= samples.len() {
        frames += 1;
        if let Some(hz) = frame_pitch(&samples[start..start + FRAME], min_lag, max_lag, rate) {
            voiced.push(hz);
        }
        start += HOP;
    }

    // Require a reasonable share of the window to be voiced. A stray voiced
    // frame inside thirty seconds of music is not a speaker.
    if frames == 0 || voiced.len() * 3 < frames {
        return None;
    }

    voiced.sort_by(f32::total_cmp);
    Some(voiced[voiced.len() / 2])
}

/// Pitch of one frame, or `None` if it does not look voiced.
fn frame_pitch(frame: &[f32], min_lag: usize, max_lag: usize, rate: f32) -> Option<f32> {
    // Remove the mean: a DC offset correlates with itself at every lag and
    // flattens the peak we are looking for.
    let mean = frame.iter().sum::<f32>() / frame.len() as f32;
    let centred: Vec<f32> = frame.iter().map(|s| s - mean).collect();

    let energy: f32 = centred.iter().map(|s| s * s).sum();
    if energy <= f32::EPSILON {
        return None;
    }

    let mut best = (0usize, 0.0f32);
    for lag in min_lag..=max_lag {
        let mut correlation = 0.0;
        for i in 0..centred.len() - lag {
            correlation += centred[i] * centred[i + lag];
        }
        // Normalised by the energy of the overlapping part, so long lags are
        // not penalised simply for comparing fewer samples.
        let overlap: f32 = centred[..centred.len() - lag].iter().map(|s| s * s).sum();
        let tail: f32 = centred[lag..].iter().map(|s| s * s).sum();
        let norm = (overlap * tail).sqrt();
        if norm <= f32::EPSILON {
            continue;
        }
        let score = correlation / norm;
        if score > best.1 {
            best = (lag, score);
        }
    }

    if best.0 == 0 || best.1 < VOICING_THRESHOLD {
        return None;
    }
    Some(rate / best.0 as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const RATE: u32 = 16_000;

    /// A voice-like signal: a fundamental with harmonics, as speech has.
    fn voiced(hz: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                (2.0 * PI * hz * t).sin() * 0.5
                    + (2.0 * PI * hz * 2.0 * t).sin() * 0.25
                    + (2.0 * PI * hz * 3.0 * t).sin() * 0.12
            })
            .collect()
    }

    #[test]
    fn a_low_voice_measures_low() {
        let hz = median_pitch(&voiced(110.0, 16_000), RATE).expect("should find a pitch");
        assert!(
            (hz - 110.0).abs() < 8.0,
            "measured {hz}, expected about 110"
        );
    }

    #[test]
    fn a_high_voice_measures_high() {
        let hz = median_pitch(&voiced(210.0, 16_000), RATE).expect("should find a pitch");
        assert!(
            (hz - 210.0).abs() < 12.0,
            "measured {hz}, expected about 210"
        );
    }

    #[test]
    fn a_typical_mans_voice_reads_as_the_lower_register() {
        let hz = median_pitch(&voiced(120.0, 16_000), RATE).unwrap();
        assert_eq!(Register::from_hz(hz), Register::Lower);
    }

    #[test]
    fn a_typical_womans_voice_reads_as_the_higher_register() {
        let hz = median_pitch(&voiced(200.0, 16_000), RATE).unwrap();
        assert_eq!(Register::from_hz(hz), Register::Higher);
    }

    #[test]
    fn the_boundary_between_registers_is_a_band_not_a_line() {
        // Between the two, the evidence does not support a confident choice and
        // a wrong one is worse than none.
        assert_eq!(Register::from_hz(165.0), Register::Unclear);
    }

    #[test]
    fn silence_has_no_pitch() {
        assert_eq!(median_pitch(&vec![0.0; 16_000], RATE), None);
    }

    #[test]
    fn noise_has_no_pitch() {
        // The case that matters on a soundtrack: music and effects must not be
        // handed a register, or they get a character's voice.
        let mut state = 0x1234_5678_9abc_def0_u64;
        let noise: Vec<f32> = (0..16_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 40) as f32 / 8_388_608.0 - 0.5
            })
            .collect();
        assert_eq!(median_pitch(&noise, RATE), None, "noise was given a pitch");
    }

    #[test]
    fn too_short_a_window_yields_nothing() {
        assert_eq!(median_pitch(&voiced(150.0, 100), RATE), None);
    }

    #[test]
    fn loudness_does_not_change_the_measured_pitch() {
        // The same person speaking quietly is the same person.
        let quiet = voiced(130.0, 16_000);
        let loud: Vec<f32> = quiet.iter().map(|s| s * 6.0).collect();
        let a = median_pitch(&quiet, RATE).unwrap();
        let b = median_pitch(&loud, RATE).unwrap();
        assert!((a - b).abs() < 2.0, "{a} vs {b}");
    }

    #[test]
    fn a_nonsense_pitch_is_unclear_rather_than_classified() {
        assert_eq!(Register::from_hz(0.0), Register::Unclear);
        assert_eq!(Register::from_hz(-5.0), Register::Unclear);
        assert_eq!(Register::from_hz(f32::NAN), Register::Unclear);
    }
}
