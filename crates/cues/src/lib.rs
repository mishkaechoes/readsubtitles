//! Turning a stream of noisy OCR reads into the sequence of lines worth
//! speaking.
//!
//! This crate is deliberately free of macOS types so it can be tested against
//! fixtures on any machine. Everything here is driven by two calls:
//!
//! - [`CueTracker::observe`] records what OCR just read off the screen.
//! - [`CueTracker::poll`] is called on a timer and returns a line once it has
//!   stopped changing.
//!
//! The split matters because ScreenCaptureKit suppresses duplicate frames: a
//! subtitle that sits still on screen produces exactly one observation, so we
//! cannot wait for a second sighting to confirm it. A timer has to do that job.

use std::time::{Duration, Instant};

pub mod regions;
pub mod speakers;
pub mod text;

pub use regions::{Candidate, FilterConfig, keep_dialogue};
pub use speakers::{ITALIC_MARK, Segment, Voice, split};
pub use text::{normalize, similarity};

/// How long a candidate line must stop changing before we accept it.
///
/// Subtitles fade in, and players sometimes reveal a line in pieces, so reading
/// the first OCR hit aloud produces stutter. Waiting for the text to settle
/// trades a little latency for reading each line once, correctly.
pub const DEFAULT_SETTLE: Duration = Duration::from_millis(180);

/// Above this similarity to the last spoken line, a new read is treated as the
/// same subtitle re-recognised rather than a new one.
///
/// OCR jitters by a character or two on a fading cue; without this the same
/// line gets spoken twice.
pub const DEFAULT_SIMILARITY: f64 = 0.90;

/// A line that has settled and is ready to speak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    /// Normalised to one line: what dedupe and similarity compare.
    pub text: String,
    /// Exactly as it appeared, line breaks intact.
    ///
    /// Kept separate because the two consumers need opposite things. Dedupe
    /// wants whitespace flattened so a re-wrapped subtitle is recognised as the
    /// same line; speaker detection needs the line breaks, since the dash
    /// convention for two speakers is expressed entirely in them.
    pub raw: String,
    /// When the text was first seen on screen, for measuring end-to-end lag.
    pub first_seen: Instant,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub settle: Duration,
    pub similarity_threshold: f64,
    /// A line shorter than this is treated as OCR noise rather than dialogue.
    pub min_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            settle: DEFAULT_SETTLE,
            similarity_threshold: DEFAULT_SIMILARITY,
            min_chars: 2,
        }
    }
}

/// Collapses a stream of OCR reads into stable, non-repeating cues.
#[derive(Debug)]
pub struct CueTracker {
    config: Config,
    /// Current candidate: the text, when it first appeared, and when it last
    /// changed. The settle timer runs from `changed_at`, not `first_seen`, so
    /// a line revealed in pieces is spoken once when it finishes.
    pending: Option<Pending>,
    last_spoken: Option<String>,
}

#[derive(Debug, Clone)]
struct Pending {
    text: String,
    raw: String,
    first_seen: Instant,
    changed_at: Instant,
}

impl CueTracker {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            pending: None,
            last_spoken: None,
        }
    }

    /// Record what OCR read. `raw` may be empty, multi-line, or noisy.
    pub fn observe(&mut self, raw: &str, now: Instant) {
        let text = normalize(raw);
        let raw = raw.trim().to_string();

        if text.chars().count() < self.config.min_chars {
            // Screen went blank between subtitles. Drop the candidate, but keep
            // `last_spoken`, so a cue that flickers off and back on is not read
            // out a second time.
            self.pending = None;
            return;
        }

        match &mut self.pending {
            // Same text again: leave the settle timer alone so it can expire.
            Some(p) if p.text == text => {}
            // Changed while settling: this is still the same subtitle being
            // revealed or re-recognised, so keep the original first_seen for
            // honest lag reporting and restart the settle timer.
            Some(p) => {
                p.text = text;
                p.raw = raw;
                p.changed_at = now;
            }
            None => {
                self.pending = Some(Pending {
                    text,
                    raw,
                    first_seen: now,
                    changed_at: now,
                });
            }
        }
    }

    /// Emit a cue if the candidate has held still long enough.
    ///
    /// Call this on a timer, not only when a frame arrives.
    pub fn poll(&mut self, now: Instant) -> Option<Cue> {
        let p = self.pending.as_ref()?;
        if now.duration_since(p.changed_at) < self.config.settle {
            return None;
        }

        let p = self.pending.take()?;

        // Suppress a near-repeat of whatever we said last.
        if let Some(last) = &self.last_spoken
            && similarity(last, &p.text) >= self.config.similarity_threshold
        {
            return None;
        }

        self.last_spoken = Some(p.text.clone());
        Some(Cue {
            text: p.text,
            raw: p.raw,
            first_seen: p.first_seen,
        })
    }

    /// Forget what was last spoken, so the next identical line is read again.
    /// Used when the user stops and restarts.
    pub fn reset(&mut self) {
        self.pending = None;
        self.last_spoken = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cue_keeps_its_line_breaks_for_speaker_detection() {
        // The dash convention for two speakers lives entirely in the line
        // breaks, so flattening them here silently disables it.
        let mut t = CueTracker::new(Config::default());
        let now = Instant::now();
        t.observe("- Did you hear?\n- No, tell me.", now);
        let cue = t
            .poll(now + DEFAULT_SETTLE + Duration::from_millis(1))
            .unwrap();
        assert_eq!(cue.raw, "- Did you hear?\n- No, tell me.");
        assert_eq!(cue.text, "- Did you hear? - No, tell me.");
    }

    fn t0() -> Instant {
        Instant::now()
    }

    fn after(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn tracker() -> CueTracker {
        CueTracker::new(Config::default())
    }

    #[test]
    fn emits_a_line_once_it_settles() {
        let (mut c, t) = (tracker(), t0());
        c.observe("Yeah. It was a six-hour surgery.", t);
        assert!(
            c.poll(after(t, 50)).is_none(),
            "must not emit before settle"
        );
        let cue = c.poll(after(t, 200)).expect("should emit after settle");
        assert_eq!(cue.text, "Yeah. It was a six-hour surgery.");
    }

    #[test]
    fn does_not_repeat_a_still_subtitle() {
        let (mut c, t) = (tracker(), t0());
        c.observe("Hello there.", t);
        assert!(c.poll(after(t, 200)).is_some());
        // Same text re-read a second later: already spoken.
        c.observe("Hello there.", after(t, 1000));
        assert!(c.poll(after(t, 1500)).is_none());
    }

    #[test]
    fn progressive_reveal_is_spoken_once_when_complete() {
        let (mut c, t) = (tracker(), t0());
        c.observe("Yeah. It", t);
        c.observe("Yeah. It was a", after(t, 60));
        c.observe("Yeah. It was a six-hour surgery.", after(t, 120));
        // The settle timer restarted on each change.
        assert!(c.poll(after(t, 200)).is_none());
        let cue = c.poll(after(t, 320)).expect("emits the finished line");
        assert_eq!(cue.text, "Yeah. It was a six-hour surgery.");
        // Lag is measured from when the first fragment appeared, not the last.
        assert_eq!(cue.first_seen, t);
    }

    #[test]
    fn ocr_jitter_does_not_cause_a_second_reading() {
        let (mut c, t) = (tracker(), t0());
        c.observe("It was a six-hour surgery.", t);
        assert!(c.poll(after(t, 200)).is_some());
        // Same cue, one character misread as it fades.
        c.observe("It was a six-hour surgerv.", after(t, 900));
        assert!(
            c.poll(after(t, 1200)).is_none(),
            "near-identical text is the same subtitle"
        );
    }

    #[test]
    fn a_genuinely_new_line_is_spoken() {
        let (mut c, t) = (tracker(), t0());
        c.observe("It was a six-hour surgery.", t);
        assert!(c.poll(after(t, 200)).is_some());
        c.observe("And how did it go?", after(t, 2000));
        let cue = c.poll(after(t, 2300)).expect("different line is new");
        assert_eq!(cue.text, "And how did it go?");
    }

    #[test]
    fn blank_screen_clears_the_candidate_but_not_the_history() {
        let (mut c, t) = (tracker(), t0());
        c.observe("Something.", t);
        c.observe("", after(t, 50));
        assert!(
            c.poll(after(t, 400)).is_none(),
            "candidate dropped when the subtitle disappeared"
        );

        c.observe("Something else.", after(t, 500));
        assert!(c.poll(after(t, 800)).is_some());
        // The earlier line was never spoken, so it is still allowed later.
        c.observe("Something.", after(t, 2000));
        assert!(c.poll(after(t, 2300)).is_some());
    }

    #[test]
    fn multi_line_cues_are_joined_into_one_utterance() {
        let (mut c, t) = (tracker(), t0());
        c.observe("I don't know what\nyou're talking about.", t);
        let cue = c.poll(after(t, 300)).unwrap();
        assert_eq!(cue.text, "I don't know what you're talking about.");
    }

    #[test]
    fn single_character_noise_is_ignored() {
        let (mut c, t) = (tracker(), t0());
        c.observe("|", t);
        assert!(c.poll(after(t, 400)).is_none());
    }

    #[test]
    fn reset_allows_the_same_line_again() {
        let (mut c, t) = (tracker(), t0());
        c.observe("Replay me.", t);
        assert!(c.poll(after(t, 200)).is_some());
        c.reset();
        c.observe("Replay me.", after(t, 1000));
        assert!(c.poll(after(t, 1200)).is_some());
    }
}
