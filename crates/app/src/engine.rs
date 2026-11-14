//! The capture-to-speech pipeline, as something that can be started and
//! stopped by a hotkey.
//!
//! Thread layout, forced by the platform rather than chosen:
//!
//! ```text
//!   ScreenCaptureKit callback ──▶ OCR thread ──▶ speech thread
//!   (signature + memcpy only)     (owns Vision)  (owns the voice)
//! ```
//!
//! Vision's request objects are not `Send`, and calling Vision from the capture
//! callback never returns, so each stage owns its own non-`Send` resources and
//! they communicate over channels.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use readsubtitles_browser::CaptionServer;
use readsubtitles_capture::{Band, Capture, Target};
use readsubtitles_cues::{
    Candidate, Config as CueConfig, CueTracker, DEFAULT_SETTLE, FilterConfig, keep_dialogue,
};
use readsubtitles_ocr::{Level, Recognizer};
use readsubtitles_voice::{AppleVoice, Cast, ElevenLabsVoice, Speaker};

use crate::settings::Source;

/// How often the cue tracker is asked whether a line has settled.
///
/// A timer is required rather than a per-frame check: ScreenCaptureKit sends
/// nothing while a subtitle sits still, so the settle deadline would otherwise
/// never be noticed.
const TICK: Duration = Duration::from_millis(25);

pub struct EngineConfig {
    pub source: Source,
    pub target: Target,
    pub band: Band,
    pub language: String,
    pub level: Level,
    pub fps: u32,
    pub bridge_port: u16,
    /// Read speakers in different voices where the subtitles identify them.
    pub multi_voice: bool,
    /// Also tell speakers apart by listening to the film's own soundtrack.
    pub diarize: bool,
    /// How long to listen after a subtitle appears before reading it.
    ///
    /// A subtitle appears as its line begins, so at that instant the line has
    /// not been spoken yet and the recent audio belongs to whoever spoke last.
    /// Reading immediately would attribute every line to the previous speaker.
    /// Waiting buys the current voice at the cost of latency, and that trade is
    /// the user's to make, so it is a setting rather than a constant.
    pub listen_ahead: Duration,
    /// Characters pinned to a specific voice, keyed by lower-case name.
    pub cast_overrides: Vec<(String, String)>,
    /// Told when the browser source has heard nothing for a while. The menu bar
    /// app says it aloud; the CLI prints it and passes `None`.
    pub on_browser_silence: Option<FallbackNotice>,
    /// Print every OCR region with its position. Screen source only.
    pub debug_regions: bool,
    /// Print each settled line. Off in the menu bar app, on in the CLI.
    pub verbose: bool,
}

/// Captions arriving from the browser are already exact text, so they need far
/// less settling than OCR output does. The screen path waits for a subtitle to
/// finish fading in; the browser path only has to absorb the extension's own
/// debounce.
const BROWSER_SETTLE: Duration = Duration::from_millis(60);

/// How much audio to identify a voice from.
///
/// Long enough to hold a phrase: a shorter window describes a syllable rather
/// than a person, and the embedding wanders.
const LISTEN_WINDOW: f64 = 2.0;

/// How long the browser source waits for its first caption before saying that
/// nothing is arriving.
///
/// The browser source has a failure mode the screen source does not: the
/// extension may simply not be running, in which case the app sits in perfect
/// silence looking exactly like it is working. Long enough to cover a quiet
/// opening scene, short enough that nobody sits through a whole film wondering.
const FIRST_CAPTION_GRACE: Duration = Duration::from_secs(25);

/// Builds the voice on the thread that will own it, because neither backend is
/// safe to move between threads.
pub type SpeakerFactory = Box<dyn FnOnce() -> Option<Box<dyn Speaker>> + Send>;

/// Told, with the reason, when the preferred voice could not be used and the
/// on-device one took over. The menu bar app says it out loud; the CLI prints
/// it and passes `None`.
pub type FallbackNotice = Box<dyn Fn(&str) + Send>;

/// The shared rule for choosing a voice.
///
/// Returns a closure rather than a voice because neither backend is `Send`:
/// the speech thread has to build its own. That also means a slow ElevenLabs
/// handshake happens off the caller's thread.
///
/// ElevenLabs is tried first when configured, and any failure falls back to the
/// on-device voice. A remote outage or an exhausted quota must never leave a
/// blind listener in silence.
pub fn speaker_factory(
    elevenlabs: Option<String>,
    elevenlabs_cast: Vec<String>,
    voice: Option<String>,
    language: String,
    rate: f32,
    on_fallback: Option<FallbackNotice>,
) -> SpeakerFactory {
    Box::new(move || {
        if let Some(id) = &elevenlabs {
            match ElevenLabsVoice::new(id, None).and_then(|v| v.check().map(|()| v)) {
                Ok(mut v) => {
                    v.set_rate(rate);
                    v.set_cast(elevenlabs_cast);
                    return Some(Box::new(v) as Box<dyn Speaker>);
                }
                Err(e) => {
                    eprintln!("ElevenLabs unavailable ({e}); using the on-device voice");
                    if let Some(notify) = &on_fallback {
                        notify(&e.to_string());
                    }
                }
            }
        }
        let mut v = match &voice {
            Some(id) => {
                AppleVoice::with_identifier(id).or_else(|_| AppleVoice::best_for(&language))
            }
            None => AppleVoice::best_for(&language),
        }
        .ok()?;
        v.set_rate(rate);
        Some(Box::new(v) as Box<dyn Speaker>)
    })
}

#[derive(Default)]
pub struct EngineStats {
    pub spoken: AtomicUsize,
    pub dropped: AtomicUsize,
}

/// A running pipeline. Dropping it stops everything.
pub struct Running {
    stop: Arc<AtomicBool>,
    ocr: Option<JoinHandle<()>>,
    speech: Option<JoinHandle<()>>,
    pub stats: Arc<EngineStats>,
    /// What this engine is watching, in words, for status display. The two
    /// sources have nothing in common to report, so they each describe
    /// themselves rather than being squeezed into one shape.
    pub watching: String,
}

impl Running {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The OCR thread owns the capture and drops it on exit, which releases
        // the stream's output handler and with it the frame sender. Without
        // that drop the channel stays open and the thread never finishes.
        if let Some(h) = self.ocr.take() {
            let _ = h.join();
        }
        if let Some(h) = self.speech.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start watching and speaking. Returns once the pipeline is live.
pub fn start(
    mut config: EngineConfig,
    make_speaker: SpeakerFactory,
) -> Result<Running, Box<dyn std::error::Error>> {
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(EngineStats::default());
    let (text_tx, text_rx) = channel::<(String, Instant)>();

    // Both sources end up writing `(text, when)` into the same channel, so
    // everything downstream of here - settling, dedupe, backlog policy, the
    // voice - is shared and does not know which source is in use.
    let (producer, watching, settle) = match config.source {
        Source::Browser => {
            let (server, captions) = CaptionServer::start(config.bridge_port, bridge_token()?)?;
            let port = server.port;
            let browser_stop = stop.clone();
            let on_silence = config.on_browser_silence.take();
            let handle = std::thread::spawn(move || {
                // Moved in so the listener is released when this thread ends.
                let server = server;
                let started = Instant::now();
                let mut heard_anything = false;
                let mut warned = false;

                while !browser_stop.load(Ordering::Relaxed) {
                    match captions.recv_timeout(TICK) {
                        Ok(c) => {
                            heard_anything = true;
                            if text_tx.send((c.text, c.seen_at)).is_err() {
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }

                    if !heard_anything && !warned && started.elapsed() > FIRST_CAPTION_GRACE {
                        warned = true;
                        // Distinguish "nothing at all" from "arriving but
                        // refused": they need completely different fixes.
                        let refused = server.stats.rejected.load(Ordering::Relaxed) > 0;
                        let message = if refused {
                            "The browser extension is reaching me but its token is wrong. \
                             Run make extension again and reload it."
                        } else {
                            "No captions are reaching me from the browser. \
                             The extension may not be loaded, or the video tab may need reloading."
                        };
                        eprintln!("{message}");
                        if let Some(notify) = &on_silence {
                            notify(message);
                        }
                    }
                }
            });
            (
                handle,
                format!("browser captions on port {port}"),
                BROWSER_SETTLE,
            )
        }
        Source::Screen => {
            let (capture, frames) = Capture::start(&config.target, config.band, config.fps, 2)?;
            let region = capture.size;
            let (level, language) = (config.level, config.language.clone());
            let debug_regions = config.debug_regions;
            let ocr_stop = stop.clone();
            let handle = std::thread::spawn(move || {
                run_ocr(
                    OcrJob {
                        capture,
                        frames,
                        level,
                        language,
                        region,
                        debug_regions,
                        text_tx,
                    },
                    &ocr_stop,
                );
            });
            (
                handle,
                format!("a {}x{} region of the screen", region.0, region.1),
                DEFAULT_SETTLE,
            )
        }
    };

    let speech_stop = stop.clone();
    let speech_stats = stats.clone();
    // Failing to listen must not stop the subtitles being read, so a
    // diarizer that will not start is reported and dropped.
    let diarize = if config.diarize && config.multi_voice {
        match Diarizer::start(&config.target) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("listening for speakers is off: {e}");
                None
            }
        }
    } else {
        None
    };

    let job = SpeechJob {
        make_speaker,
        settle,
        verbose: config.verbose,
        multi_voice: config.multi_voice,
        cast_overrides: config.cast_overrides.clone(),
        diarize,
        listen_ahead: config.listen_ahead,
    };
    let speech = std::thread::spawn(move || {
        run_speech(job, &text_rx, &speech_stop, &speech_stats);
    });

    Ok(Running {
        stop,
        ocr: Some(producer),
        speech: Some(speech),
        stats,
        watching,
    })
}

/// Pick the voice for one segment.
///
/// An explicit override always wins: the subtitles say who is speaking, but
/// nothing in them says whether that character should sound female, elderly or
/// villainous, so that judgement stays with the person who set it.
fn voice_for(
    cast: &mut Cast,
    overrides: &[(String, String)],
    voice: &readsubtitles_cues::Voice,
    heard: Option<&str>,
    preferred: &[String],
) -> Option<String> {
    if let readsubtitles_cues::Voice::Named(name) = voice {
        let wanted = name.to_lowercase();
        if let Some((_, id)) = overrides.iter().find(|(n, _)| *n == wanted) {
            return Some(id.clone());
        }
    }
    cast.voice_for_preferring(&identity_key(voice, heard), preferred)
        .map(str::to_string)
}

/// Which identity a line should be read as, from the evidence available.
///
/// Both the subtitles and the soundtrack can say who is speaking, and they are
/// not equally trustworthy, so the order matters:
///
/// 1. A name in the caption. Deliberate, unambiguous, holds across the film.
/// 2. Dashes, which split one cue between two speakers the soundtrack cannot
///    separate, because its window holds both of them at once.
/// 3. The voice heard in the soundtrack, which is the only evidence available
///    when the subtitles are a translation and carry no labels at all.
/// 4. Italics, which mark narration or inner monologue. These rank *below* the
///    soundtrack rather than above it: italics say the line is not spoken
///    aloud, not that somebody else is speaking it. When a character's inner
///    voice can be heard and identified, reading it as them is more useful
///    than reading every character's thoughts in one shared narrator.
/// 5. Nothing, in which case the line keeps the main voice.
///
/// Separated out and tested because the mistake it replaces was invisible from
/// the outside: the soundtrack's answer was computed correctly and then thrown
/// away, because an unlabelled line already had a voice and so never fell
/// through to it. Every line came out in the same voice and nothing looked
/// wrong in the log.
fn identity_key(voice: &readsubtitles_cues::Voice, heard: Option<&str>) -> String {
    use readsubtitles_cues::Voice;
    match (voice, heard) {
        (Voice::Default | Voice::Narration, Some(key)) => key.to_string(),
        _ => voice.key(),
    }
}

/// A short name for a register, for the log.
fn register_name(r: readsubtitles_diarize::Register) -> &'static str {
    use readsubtitles_diarize::Register;
    match r {
        Register::Higher => "higher",
        Register::Lower => "lower",
        Register::Unclear => "?",
    }
}

/// A short prefix for the log, so it is obvious which voice read what.
fn label(voice: &readsubtitles_cues::Voice) -> String {
    match voice {
        readsubtitles_cues::Voice::Named(name) => format!("{name}: "),
        readsubtitles_cues::Voice::Turn(n) => format!("(speaker {}) ", n + 1),
        readsubtitles_cues::Voice::Narration => "(narration) ".into(),
        readsubtitles_cues::Voice::Default => String::new(),
    }
}

/// Where the extension's shared token lives.
///
/// Written by `bundle/make-extension.sh`. Its absence means the browser source
/// was never set up, which is worth saying plainly rather than failing later
/// with a stream of rejected posts.
fn bridge_token() -> Result<String, Box<dyn std::error::Error>> {
    let path = std::path::Path::new(&std::env::var("HOME")?)
        .join("Library/Application Support/readsubtitles/bridge-token");
    let token = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "the browser bridge is not set up yet; run ./bundle/make-extension.sh \
             (expected a token at {})",
            path.display()
        )
    })?;
    Ok(token.trim().to_string())
}

/// Everything the OCR worker needs, gathered so the signature stays readable.
struct OcrJob {
    capture: Capture,
    frames: Receiver<readsubtitles_capture::Frame>,
    level: Level,
    language: String,
    region: (u32, u32),
    debug_regions: bool,
    text_tx: std::sync::mpsc::Sender<(String, Instant)>,
}

fn run_ocr(job: OcrJob, stop: &AtomicBool) {
    let OcrJob {
        capture,
        frames,
        level,
        language,
        region,
        debug_regions,
        text_tx,
    } = job;
    let language = language.as_str();
    let frames = &frames;
    let text_tx = &text_tx;
    let recognizer = Recognizer::new(level, &[language], true);
    // Pay Vision's one-off model compile for this exact region size now,
    // rather than on the first subtitle of the film.
    let warm = recognizer.warm_up(region.0 as usize, region.1 as usize);
    if warm > 1000.0 {
        println!("vision model compiled in {:.1}s (one-off)", warm / 1000.0);
    }
    let filter = FilterConfig::default();

    for frame in frames {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Some(reading) = recognizer.read(&frame.bgra, frame.width, frame.height, frame.stride)
        else {
            continue;
        };
        if debug_regions {
            for r in &reading.regions {
                eprintln!(
                    "    region x={:.2}..{:.2} cx={:.2} ytop={:.2} h={:.2}  {:?}",
                    r.x,
                    r.x + r.width,
                    r.centre_x(),
                    r.centre_from_top(),
                    r.height,
                    r.text
                );
            }
        }
        let candidates: Vec<Candidate> = reading
            .regions
            .iter()
            .map(|r| Candidate {
                text: r.text.clone(),
                centre_x: r.centre_x(),
                height: r.height,
            })
            .collect();
        let text = keep_dialogue(&candidates, &filter).join(" ");
        if text_tx.send((text, frame.seen_at)).is_err() {
            break;
        }
    }
    // Dropping the capture releases the stream's output handler and with it the
    // frame sender. Without that the channel never closes.
    drop(capture);
}

struct SpeechJob {
    make_speaker: SpeakerFactory,
    settle: Duration,
    verbose: bool,
    multi_voice: bool,
    cast_overrides: Vec<(String, String)>,
    diarize: Option<Diarizer>,
    listen_ahead: Duration,
}

/// Voices suited to a speaker of this register, from the user's own labels.
///
/// The labels are free text the user wrote next to each voice id, so matching
/// is by keyword. The ordering below is not cosmetic: "female" contains
/// "male", so testing for a man first would classify every woman's voice as a
/// man's. That is exactly the mistake this function exists to prevent.
fn voices_for_register(
    cast: &[crate::settings::CastEntry],
    register: readsubtitles_diarize::Register,
) -> Vec<String> {
    use readsubtitles_diarize::Register;
    if register == Register::Unclear {
        return Vec::new();
    }
    cast.iter()
        .filter(|e| {
            let label = e.label.to_lowercase();
            let higher =
                label.contains("female") || label.contains("woman") || label.contains("girl");
            // Checked only after ruling out "female", for the reason above.
            let lower = !higher
                && (label.contains("male") || label.contains("man") || label.contains("boy"));
            match register {
                Register::Higher => higher,
                Register::Lower => lower,
                Register::Unclear => false,
            }
        })
        .map(|e| e.voice_id.clone())
        .collect()
}

/// Identifies the voice speaking during a subtitle, from the soundtrack.
struct Diarizer {
    ring: Arc<readsubtitles_capture::audio::AudioRing>,
    fbank: readsubtitles_diarize::Fbank,
    embedder: readsubtitles_diarize::Embedder,
    clusterer: readsubtitles_diarize::Clusterer,
    /// The register each speaker was first confidently heard in.
    ///
    /// Recorded once and never revised, for the same reason a voice assignment
    /// is never revised: a character who changes voice halfway through is worse
    /// than one who was given a slightly wrong voice to begin with.
    registers: std::collections::HashMap<String, readsubtitles_diarize::Register>,
    /// Kept alive so the audio stream keeps running.
    _capture: readsubtitles_capture::audio::AudioCapture,
}

impl Diarizer {
    fn start(target: &Target) -> Result<Self, Box<dyn std::error::Error>> {
        let model = readsubtitles_diarize::model_path();
        let embedder = readsubtitles_diarize::Embedder::load(&model)?;
        let capture = readsubtitles_capture::audio::AudioCapture::start(target)?;
        Ok(Self {
            ring: capture.ring.clone(),
            fbank: readsubtitles_diarize::Fbank::new(),
            embedder,
            clusterer: readsubtitles_diarize::Clusterer::new(
                readsubtitles_diarize::Config::default(),
            ),
            registers: std::collections::HashMap::new(),
            _capture: capture,
        })
    }

    /// Who is speaking in the `seconds` of audio just heard.
    ///
    /// Returns `None` whenever it cannot tell, which the caller must treat as
    /// "use the usual voice" rather than guessing. Being wrong about who is
    /// speaking is worse than not varying the voice at all.
    fn who(&mut self, seconds: f64) -> Option<(String, readsubtitles_diarize::Register)> {
        use readsubtitles_diarize::Register;
        use readsubtitles_diarize::cluster::DEFAULT_MIN_LOUDNESS;

        let audio = self.ring.recent(seconds);
        if !readsubtitles_diarize::has_voice(&audio, DEFAULT_MIN_LOUDNESS) {
            return None;
        }
        let feats = self.fbank.compute(&audio);
        let bins = readsubtitles_diarize::fbank::NUM_BINS;
        let frames = feats.len() / bins;
        if frames == 0 {
            return None;
        }
        let embedding = self.embedder.embed(&feats, frames, bins).ok()?;
        let key = self.clusterer.assign(&embedding).key()?;

        // Measured from the same window, and kept the first time it is clear.
        let measured =
            readsubtitles_diarize::median_pitch(&audio, readsubtitles_capture::audio::SAMPLE_RATE)
                .map_or(Register::Unclear, Register::from_hz);
        let register = *self
            .registers
            .entry(key.clone())
            .and_modify(|held| {
                if *held == Register::Unclear {
                    *held = measured;
                }
            })
            .or_insert(measured);
        Some((key, register))
    }
}

fn run_speech(
    job: SpeechJob,
    text_rx: &Receiver<(String, Instant)>,
    stop: &AtomicBool,
    stats: &EngineStats,
) {
    let SpeechJob {
        make_speaker,
        settle,
        verbose,
        multi_voice,
        cast_overrides,
        mut diarize,
        listen_ahead,
    } = job;
    let mut speaker = make_speaker();

    // Build the cast from whatever the chosen backend can offer. A backend with
    // one voice yields an empty pool, and everything below quietly becomes the
    // single-voice behaviour.
    // The labelled voices, so a measured register can be matched to one.
    let cast_entries = crate::settings::Settings::load().elevenlabs_cast;
    let mut cast = speaker
        .as_ref()
        .map(|s| Cast::new(s.character_voices()))
        .unwrap_or_else(|| Cast::new(Vec::new()));
    if let Some(s) = speaker.as_ref()
        && let Some(main) = s.selected_voice()
    {
        cast.set_default_voice(main);
    }
    if verbose && multi_voice {
        println!("{} voice(s) available for characters", cast.pool_size());
    }
    let mut tracker = CueTracker::new(CueConfig {
        settle,
        ..CueConfig::default()
    });

    while !stop.load(Ordering::Relaxed) {
        match text_rx.recv_timeout(TICK) {
            Ok((text, seen_at)) => tracker.observe(&text, seen_at),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let Some(cue) = tracker.poll(Instant::now()) else {
            continue;
        };
        stats.spoken.fetch_add(1, Ordering::Relaxed);

        // One cue can hold several speakers. Splitting happens after dedupe, so
        // the settle window and repeat suppression still treat the cue as one
        // thing, which is what they are tuned for.
        let segments = if multi_voice {
            readsubtitles_cues::split(&cue.raw)
        } else {
            vec![readsubtitles_cues::Segment {
                voice: readsubtitles_cues::Voice::Default,
                text: cue.text.clone(),
            }]
        };

        // Stay with the picture: if the previous line is still being read when
        // the next subtitle lands, cut it off rather than falling further and
        // further behind what is on screen.
        if let Some(s) = speaker.as_mut()
            && s.is_speaking()
        {
            s.stop();
            stats.dropped.fetch_add(1, Ordering::Relaxed);
        }

        // Listening only helps when the whole cue is one person: a cue holding
        // two speakers produces one window containing both voices, and the
        // embedding of that belongs to neither of them.
        let heard = match diarize.as_mut() {
            Some(d) if segments.len() == 1 => {
                std::thread::sleep(listen_ahead);
                d.who(LISTEN_WINDOW)
            }
            _ => None,
        };

        // Measured after the listening pause rather than before it, so the
        // number reported is the delay the listener actually experiences. Taken
        // earlier it claimed 85ms for a line spoken a second later.
        let lag = cue.first_seen.elapsed().as_millis();

        for segment in &segments {
            if verbose {
                let who = heard.as_ref().map_or(String::new(), |(k, r)| {
                    format!("<{k} {}> ", register_name(*r))
                });
                println!(
                    "[{lag:>4}ms] {who}{}{}",
                    label(&segment.voice),
                    segment.text
                );
            }
            let Some(s) = speaker.as_mut() else { continue };
            let voice = voice_for(
                &mut cast,
                &cast_overrides,
                &segment.voice,
                heard.as_ref().map(|(k, _)| k.as_str()),
                &heard
                    .as_ref()
                    .map(|(_, r)| voices_for_register(&cast_entries, *r))
                    .unwrap_or_default(),
            );
            // Utterances queue in both backends, so consecutive segments play
            // back to back without any scheduling here.
            if let Err(e) = s.speak_as(&segment.text, voice.as_deref()) {
                eprintln!("speech failed: {e}");
            }
        }
    }

    if let Some(s) = speaker.as_mut() {
        s.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use readsubtitles_cues::Voice;

    #[test]
    fn a_caption_name_outranks_the_voice_heard() {
        // The subtitler said who this is. That beats a similarity score.
        assert_eq!(
            identity_key(&Voice::Named("Tenma".into()), Some("speaker:3")),
            Voice::Named("Tenma".into()).key()
        );
    }

    #[test]
    fn the_soundtrack_decides_when_the_caption_says_nothing() {
        // The case this feature exists for: a translated subtitle with no
        // speaker label, where the voice is the only evidence there is.
        assert_eq!(
            identity_key(&Voice::Default, Some("speaker:3")),
            "speaker:3"
        );
    }

    #[test]
    fn an_unlabelled_line_with_nothing_heard_keeps_the_main_voice() {
        assert_eq!(identity_key(&Voice::Default, None), Voice::Default.key());
    }

    #[test]
    fn dashes_outrank_the_voice_heard() {
        // A dashed cue holds two speakers, and the soundtrack cannot separate
        // them: one window contains both voices at once.
        assert_eq!(
            identity_key(&Voice::Turn(1), Some("speaker:3")),
            Voice::Turn(1).key()
        );
    }

    #[test]
    fn an_identified_voice_outranks_italics() {
        // Italics mean the line is not spoken aloud, not that somebody else is
        // speaking it. A character's inner monologue read in that character's
        // voice beats every character's thoughts sharing one narrator.
        assert_eq!(
            identity_key(&Voice::Narration, Some("speaker:3")),
            "speaker:3"
        );
    }

    #[test]
    fn italics_still_get_the_narration_voice_when_nobody_is_identified() {
        assert_eq!(
            identity_key(&Voice::Narration, None),
            Voice::Narration.key()
        );
    }

    #[test]
    fn two_speakers_heard_in_turn_get_two_different_voices() {
        // The regression that prompted this: identity was computed and then
        // dropped, so every line was read in one voice.
        let mut cast = Cast::new(vec!["a".into(), "b".into(), "c".into()]);
        cast.set_default_voice("main");
        let first = voice_for(&mut cast, &[], &Voice::Default, Some("speaker:0"), &[]);
        let second = voice_for(&mut cast, &[], &Voice::Default, Some("speaker:1"), &[]);
        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second, "two speakers were read in the same voice");
        assert_ne!(
            first.as_deref(),
            Some("main"),
            "a character was given the narration voice"
        );
    }

    #[test]
    fn the_same_speaker_heard_again_keeps_their_voice() {
        let mut cast = Cast::new(vec!["a".into(), "b".into()]);
        cast.set_default_voice("main");
        let first = voice_for(&mut cast, &[], &Voice::Default, Some("speaker:0"), &[]);
        voice_for(&mut cast, &[], &Voice::Default, Some("speaker:1"), &[]);
        let again = voice_for(&mut cast, &[], &Voice::Default, Some("speaker:0"), &[]);
        assert_eq!(first, again);
    }

    fn entry(id: &str, label: &str) -> crate::settings::CastEntry {
        crate::settings::CastEntry {
            voice_id: id.to_string(),
            label: label.to_string(),
        }
    }

    #[test]
    fn a_womans_label_is_not_read_as_a_mans() {
        // "female" contains "male". Testing for a man first would classify
        // every woman's voice as a man's and invert the whole feature, which
        // would look like it was working and sound exactly wrong.
        use readsubtitles_diarize::Register;
        let cast = vec![entry("f", "female"), entry("m", "smooth male")];
        assert_eq!(voices_for_register(&cast, Register::Higher), ["f"]);
        assert_eq!(voices_for_register(&cast, Register::Lower), ["m"]);
    }

    #[test]
    fn several_labels_for_one_register_are_all_offered() {
        use readsubtitles_diarize::Register;
        let cast = vec![
            entry("a", "female"),
            entry("b", "smooth female"),
            entry("c", "smooth male"),
        ];
        assert_eq!(voices_for_register(&cast, Register::Higher), ["a", "b"]);
    }

    #[test]
    fn an_unlabelled_voice_belongs_to_no_register() {
        use readsubtitles_diarize::Register;
        let cast = vec![entry("x", "evil characters"), entry("y", "")];
        assert!(voices_for_register(&cast, Register::Higher).is_empty());
        assert!(voices_for_register(&cast, Register::Lower).is_empty());
    }

    #[test]
    fn an_unclear_register_expresses_no_preference() {
        // Guessing is worse than not choosing, so an uncertain reading must
        // not steer the voice at all.
        use readsubtitles_diarize::Register;
        let cast = vec![entry("f", "female"), entry("m", "male")];
        assert!(voices_for_register(&cast, Register::Unclear).is_empty());
    }

    #[test]
    fn labels_match_whatever_the_capitalisation() {
        use readsubtitles_diarize::Register;
        let cast = vec![entry("f", "Smooth FEMALE voice")];
        assert_eq!(voices_for_register(&cast, Register::Higher), ["f"]);
    }

    #[test]
    fn a_woman_is_given_a_womans_voice_end_to_end() {
        use readsubtitles_diarize::Register;
        let entries = vec![entry("man", "smooth male"), entry("woman", "smooth female")];
        let mut cast = Cast::new(vec!["man".into(), "woman".into()]);
        cast.set_default_voice("narration");
        let preferred = voices_for_register(&entries, Register::Higher);
        let got = voice_for(
            &mut cast,
            &[],
            &Voice::Default,
            Some("speaker:0"),
            &preferred,
        );
        assert_eq!(
            got.as_deref(),
            Some("woman"),
            "a speaker measured as a woman was not given a woman's voice"
        );
    }

    #[test]
    fn an_override_still_wins_over_everything() {
        let mut cast = Cast::new(vec!["a".into(), "b".into()]);
        cast.set_default_voice("main");
        let overrides = vec![("tenma".to_string(), "pinned".to_string())];
        assert_eq!(
            voice_for(
                &mut cast,
                &overrides,
                &Voice::Named("Tenma".into()),
                Some("speaker:3"),
                &[]
            )
            .as_deref(),
            Some("pinned")
        );
    }
}
