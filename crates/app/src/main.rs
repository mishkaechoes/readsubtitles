//! Reads the subtitles on screen out loud.
//!
//! Pipeline, one stage per thread boundary:
//!
//! ```text
//!   ScreenCaptureKit ──(changed frames)──▶ OCR thread ──(text)──▶ main thread
//!                                          Vision                 cues + speech
//! ```
//!
//! The split is forced by the platform, not by taste: Vision's request objects
//! are not `Send`, calling Vision from the capture callback never returns, and
//! the speech synthesiser is happiest owned by one thread. See
//! `docs/research/2026-11-13-spoken-subtitles-macos.md`.

mod engine;
mod menubar;
mod picker;
mod settings;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use readsubtitles_capture::{Band, Target};
use readsubtitles_ocr::Level;
use readsubtitles_voice::{AppleVoice, ElevenLabsVoice, Speaker, apple::available_voices};

/// How often the cue tracker is asked whether a line has settled.
///
/// This has to be a timer rather than a per-frame check, because
/// ScreenCaptureKit sends nothing at all while a subtitle sits still.
const TICK: Duration = Duration::from_millis(25);

struct Options {
    target: Target,
    band: Band,
    language: String,
    voice: Option<String>,
    /// When set, use ElevenLabs with this voice id instead of the on-device voice.
    elevenlabs: Option<String>,
    rate: f32,
    level: Level,
    dry_run: bool,
    multi_voice: Option<bool>,
    diarize: Option<bool>,
    debug_regions: bool,
    menubar: bool,
    save: bool,
    check_voice: bool,
    check_region: bool,
    check_bridge: bool,
    check_audio: bool,
    check_speakers: bool,
    threshold: Option<f32>,
    source: settings::Source,
    bridge_port: u16,
    /// Flags the user actually typed. Without this, `--save` writes every
    /// field including the ones left at their defaults, silently clearing
    /// settings the user set earlier by another command.
    given: std::collections::HashSet<String>,
    fps: u32,
    /// Stop after this long. Used by the test harness; 0 means run until Ctrl-C.
    duration: Option<Duration>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            target: Target::Display(0),
            band: Band::default(),
            language: "en-US".into(),
            voice: None,
            elevenlabs: None,
            rate: 1.0,
            level: Level::Accurate,
            dry_run: false,
            multi_voice: None,
            diarize: None,
            debug_regions: false,
            menubar: false,
            save: false,
            check_voice: false,
            check_region: false,
            check_bridge: false,
            check_audio: false,
            check_speakers: false,
            threshold: None,
            source: settings::Source::default(),
            bridge_port: readsubtitles_browser::DEFAULT_PORT,
            given: std::collections::HashSet::new(),
            fps: 8,
            duration: None,
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "\
readsubtitles - read on-screen subtitles aloud

USAGE:
    readsubtitles [OPTIONS]

SOURCE (where captions come from)
    --source screen        Read the screen with OCR. Works on anything that
                           shows text. Needs Screen Recording permission.
                           This is the default.
    --source browser       Read captions from the browser's own DOM. Exact
                           text, no permission needed, but only on sites the
                           extension covers. Set up with bundle/make-extension.sh
    --bridge-port <n>      Port the extension posts to, default 53117.

TARGET (what to watch, screen source only)
    --window <substring>   First on-screen window whose title contains this.
                           Preferred when several displays are in use.
    --display <index>      Display by index (default: 0). See --list.

REGION (fractions of the target's frame; default is the bottom fifth)
    --band <x,y,w,h>       e.g. 0.10,0.78,0.80,0.22

VOICE
    --language <tag>       BCP-47, default en-US. Also the OCR language.
    --voice <identifier>   A specific installed voice (see --list-voices).
    --elevenlabs <id>      Use an ElevenLabs voice instead of the on-device one.
                           Costs about $0.04 per 1000 characters and needs the
                           network; falls back to the on-device voice on failure.
                           Key comes from the login keychain (service
                           readsubtitles-elevenlabs) or ELEVENLABS_API_KEY.
    --rate <multiplier>    1.0 is normal. Raise it if speech lags the picture.

OTHER
    --fast                 Use Vision's fast recognizer (~7ms vs ~35ms).
    --dry-run              Print cues instead of speaking them.
    --multi-voice          Give different characters different voices.
    --no-multi-voice       Read every line in the one voice.
    --diarize              Tell voices apart by listening to the soundtrack.
                           Costs about half a second of extra delay.
    --no-diarize           Decide who is speaking from the subtitles alone.
    --debug-regions        Print each OCR region with its position.
    --fps <n>              Capture ceiling, default 8.
    --duration <seconds>   Stop after this long instead of running until Ctrl-C.
    --list-voices          List installed voices and exit.
    --check-voice          Say a test line through the configured voice and
                           report exactly why it failed, if it did.
    --check-region         Verify that the region picker's screen coordinates
                           line up with what the capture actually watches.
    --check-speakers       Listen to the film and report whether distinct
                           voices can actually be told apart, with timings.
    --check-audio          Listen to the film's own soundtrack and report
                           whether it can be heard, for speaker separation.
    --check-bridge         Listen for captions from the browser extension and
                           print whatever arrives, to tell a dead extension
                           apart from one that is running but finding nothing.
    --list                 List displays and windows and exit.
    --menubar              Run as the menu bar app (the default with no flags).
    --save                 Write the given options to the settings file and exit.

HOTKEYS (menu bar mode)
    Control+Option+Command+S   start / stop reading
    Control+Option+Command+Q   quit
"
    );
    std::process::exit(2)
}

fn parse_args() -> Options {
    let mut o = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        o.given.insert(arg.clone());
        match arg.as_str() {
            "--window" => o.target = Target::WindowTitle(value()),
            "--display" => o.target = Target::Display(value().parse().unwrap_or(0)),
            "--band" => {
                let v: Vec<f64> = value()
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
                if v.len() != 4 {
                    eprintln!("--band needs four comma-separated fractions");
                    std::process::exit(2);
                }
                o.band = Band {
                    x: v[0],
                    y: v[1],
                    width: v[2],
                    height: v[3],
                };
            }
            "--language" => o.language = value(),
            "--voice" => o.voice = Some(value()),
            "--elevenlabs" => o.elevenlabs = Some(value()),
            "--rate" => o.rate = value().parse().unwrap_or(1.0),
            "--fps" => o.fps = value().parse().unwrap_or(8),
            "--duration" => {
                o.duration = value().parse().ok().map(Duration::from_secs);
            }
            "--fast" => o.level = Level::Fast,
            "--dry-run" => o.dry_run = true,
            // The menu bar has had these two toggles all along; the CLI had
            // no way to say either, so a scripted or headless run was stuck
            // with whatever was last saved.
            "--multi-voice" => o.multi_voice = Some(true),
            "--no-multi-voice" => o.multi_voice = Some(false),
            "--diarize" => o.diarize = Some(true),
            "--no-diarize" => o.diarize = Some(false),
            "--debug-regions" => o.debug_regions = true,
            "--source" => {
                let v = value();
                o.source = settings::Source::parse(&v).unwrap_or_else(|| {
                    eprintln!("unknown source {v:?}; expected 'screen' or 'browser'");
                    std::process::exit(2)
                });
            }
            "--bridge-port" => o.bridge_port = value().parse().unwrap_or(0),
            "--menubar" => o.menubar = true,
            "--save" => o.save = true,
            "--list-voices" => {
                for v in available_voices() {
                    println!(
                        "{:<9} {:<8} {:<22} {}",
                        v.quality, v.language, v.name, v.identifier
                    );
                }
                std::process::exit(0);
            }
            // Checking the ElevenLabs key needs the voice id, which may be
            // given on this same command line, so the check runs after parsing
            // rather than here.
            "--check-voice" => o.check_voice = true,
            "--check-region" => o.check_region = true,
            "--check-bridge" => o.check_bridge = true,
            "--check-audio" => o.check_audio = true,
            "--check-speakers" => o.check_speakers = true,
            "--threshold" => {
                o.threshold = args.next().and_then(|v| v.parse().ok());
            }
            "--list" => {
                readsubtitles_capture::print_targets().unwrap_or_else(|e| {
                    eprintln!("could not list targets: {e}");
                    std::process::exit(1);
                });
                std::process::exit(0);
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown option: {other}");
                usage()
            }
        }
    }
    o
}

/// Speak a test line through whichever voice is configured, and say plainly
/// what went wrong when it does not work.
///
/// Exists because the fallback to the on-device voice is deliberately quiet
/// about its cause during playback, which leaves "why is this not the voice I
/// chose" very hard to answer. This answers it in one command.
fn check_voice(opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let settings = settings::Settings::load();
    let voice_id = opts
        .elevenlabs
        .clone()
        .or(settings.elevenlabs_voice)
        .filter(|v| !v.is_empty());

    let Some(voice_id) = voice_id else {
        println!("no ElevenLabs voice configured; the on-device voice will be used");
        let mut v = AppleVoice::best_for(&opts.language)?;
        println!("on-device voice: {}", v.name());
        v.speak("The built in voice is working.")?;
        while v.is_speaking() {
            std::thread::sleep(Duration::from_millis(50));
        }
        return Ok(());
    };

    if readsubtitles_voice::api_key_from_keychain().is_none() {
        println!("FAIL  no API key found");
        println!();
        println!("Store one with:");
        println!("  security add-generic-password -U -s readsubtitles-elevenlabs -a \"$USER\" -w");
        std::process::exit(1);
    }

    print!("checking ElevenLabs voice {voice_id} ... ");
    use std::io::Write;
    std::io::stdout().flush().ok();

    match ElevenLabsVoice::new(&voice_id, None).and_then(|v| v.check().map(|()| v)) {
        Ok(mut v) => {
            println!("OK");
            v.set_rate(opts.rate);
            v.speak("The Eleven Labs voice is working.")?;
            while v.is_speaking() {
                std::thread::sleep(Duration::from_millis(50));
            }
            println!("billed {} characters for this check", v.billed_chars());
            Ok(())
        }
        Err(e) => {
            println!("FAIL");
            println!();
            println!("  {e}");
            println!();
            println!("A 401 here almost always means the key exists but lacks the");
            println!("text_to_speech permission. Fix it in the ElevenLabs dashboard");
            println!("under API Keys, then store the new key with:");
            println!(
                "  security add-generic-password -U -s readsubtitles-elevenlabs -a \"$USER\" -w"
            );
            println!();
            println!("Playback still works meanwhile: it falls back to the on-device voice.");
            std::process::exit(1);
        }
    }
}

impl Options {
    fn was_given(&self, flag: &str) -> bool {
        self.given.contains(flag)
    }

    /// Copy across only the settings the user named on this command line.
    fn apply_to(&self, s: &mut settings::Settings) {
        if self.was_given("--source") {
            s.source = self.source;
        }
        if self.was_given("--bridge-port") && self.bridge_port > 0 {
            s.bridge_port = self.bridge_port;
        }
        if self.was_given("--window")
            && let Target::WindowTitle(w) = &self.target
        {
            s.window = Some(w.clone());
        }
        if self.was_given("--display")
            && let Target::Display(d) = self.target
        {
            s.display = d;
            // A display and a window title are alternatives, so naming one
            // has to clear the other or the window silently wins.
            s.window = None;
        }
        if self.was_given("--band") {
            s.band = self.band;
        }
        if self.was_given("--language") {
            s.language = self.language.clone();
        }
        if self.was_given("--voice") {
            s.voice = self.voice.clone();
        }
        if self.was_given("--elevenlabs") {
            s.elevenlabs_voice = self.elevenlabs.clone();
        }
        if self.was_given("--rate") {
            s.rate = self.rate;
        }
        if self.was_given("--fps") {
            s.fps = self.fps;
        }
    }
}

/// Verify the region picker's coordinate handling end to end.
///
/// Two things are checked, because each can be wrong on its own. First the
/// AppKit-to-CoreGraphics flip, against the pointer position as reported by two
/// independent APIs. Then that a region drawn over the bottom of the target
/// really does come out as the bottom of the target, which catches a mistake in
/// how the selection is placed within the window.
fn check_region() -> Result<(), Box<dyn std::error::Error>> {
    use objc2::MainThreadMarker;
    let mtm = MainThreadMarker::new().ok_or("must run on the main thread")?;

    println!("coordinate flip");
    match picker::verify_coordinate_flip(mtm) {
        Ok(report) => {
            for line in report.lines() {
                println!("  {line}");
            }
            println!("  PASS");
        }
        Err(report) => {
            for line in report.lines() {
                println!("  {line}");
            }
            println!("  FAIL");
            std::process::exit(1);
        }
    }

    let settings = settings::Settings::load();
    let target = match &settings.window {
        Some(w) if !w.is_empty() => Target::WindowTitle(w.clone()),
        _ => Target::Display(settings.display),
    };

    println!("\nselection placement");
    let area = match readsubtitles_capture::target_area(&target) {
        Ok(a) => a,
        Err(e) => {
            println!("  SKIP ({e})");
            return Ok(());
        }
    };
    println!(
        "  target at ({:.0},{:.0}) {:.0}x{:.0} in capture coordinates",
        area.x, area.y, area.width, area.height
    );

    // The bottom fifth of the target, expressed the way the picker would see
    // it: AppKit y grows upward, so the bottom edge is the low value.
    let main_height = picker::main_screen_height(mtm).ok_or("no screens")?;
    let appkit_bottom = main_height - (area.y + area.height);
    let selection = picker::flip_to_capture(
        objc2_foundation::NSRect::new(
            objc2_foundation::NSPoint::new(area.x, appkit_bottom),
            objc2_foundation::NSSize::new(area.width, area.height * 0.2),
        ),
        main_height,
    );
    let band = picker::band_within(selection, area);
    println!(
        "  a box over the bottom fifth becomes band {:.3},{:.3},{:.3},{:.3}",
        band.x, band.y, band.width, band.height
    );

    // The bottom fifth must start four fifths of the way down. Anything near
    // zero means the vertical axis is inverted somewhere.
    let ok = (band.y - 0.8).abs() < 0.01 && (band.height - 0.2).abs() < 0.01;
    println!("  expected y=0.800 height=0.200");
    println!("  {}", if ok { "PASS" } else { "FAIL (axis inverted)" });
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Watch the browser bridge and narrate what happens.
///
/// The browser source has three ways to be silent and they need completely
/// different fixes: the extension is not loaded, the extension is loaded but
/// its token is wrong, or the extension is running and finding no caption text.
/// Guessing between them wastes a lot of time, so this just shows which it is.
/// Measure whether voices in the film can actually be told apart.
///
/// Everything upstream of this is machinery: capture works, the model loads,
/// features are computed. None of that says the embeddings separate one
/// character from another on a real soundtrack, with music and effects under
/// the dialogue. That is what this measures, and it is the question the whole
/// approach stands or falls on.
///
/// It reports the raw match scores rather than a verdict, because the
/// similarity threshold has to be chosen against real numbers.
fn check_speakers(opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    use readsubtitles_capture::audio::AudioCapture;
    use readsubtitles_diarize::{Assignment, Clusterer, Config, Fbank, has_voice};
    use std::time::{Duration, Instant};

    let model = readsubtitles_diarize::model_path();
    println!("loading {}", model.display());
    let mut embedder = readsubtitles_diarize::Embedder::load(&model)?;
    let fbank = Fbank::new();
    let threshold = opts
        .threshold
        .unwrap_or(readsubtitles_diarize::cluster::DEFAULT_THRESHOLD);
    let mut clusterer = Clusterer::new(Config {
        threshold,
        ..Config::default()
    });
    println!("similarity threshold: {threshold:.2}");

    let seconds = opts.duration.map_or(60, |d| d.as_secs()).max(5);
    println!("listening for {seconds}s - play a scene with people talking\n");
    let capture = AudioCapture::start(&opts.target.clone())?;

    // A window per step, long enough to hold a phrase. Speech needs more than a
    // moment to identify: too short and the embedding describes a syllable
    // rather than a voice.
    let window = 2.0;
    // Stepped so windows do not overlap. Overlapping windows share audio and
    // therefore look alike whoever is speaking, which quietly flatters the
    // clustering and hides a threshold that is too permissive.
    let step = Duration::from_millis(2100);
    let started = Instant::now();
    let mut skipped = 0usize;

    while started.elapsed().as_secs() < seconds {
        std::thread::sleep(step);
        let audio = capture.ring.recent(window);
        if !has_voice(&audio, readsubtitles_diarize::cluster::DEFAULT_MIN_LOUDNESS) {
            skipped += 1;
            continue;
        }

        let t = Instant::now();
        let feats = fbank.compute(&audio);
        let frames = feats.len() / readsubtitles_diarize::fbank::NUM_BINS;
        if frames == 0 {
            skipped += 1;
            continue;
        }
        let features_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        let embedding = embedder.embed(&feats, frames, readsubtitles_diarize::fbank::NUM_BINS)?;
        let embed_ms = t.elapsed().as_secs_f64() * 1000.0;

        let (assignment, score) = clusterer.assign_scored(&embedding);
        let who = match assignment {
            Assignment::Speaker(n) => format!("speaker {n}"),
            Assignment::Unknown => "unknown".to_string(),
        };
        println!(
            "  [{:>5.1}s] {who:<10} match {score:.3}   features {features_ms:.1}ms  embed {embed_ms:.1}ms",
            started.elapsed().as_secs_f64()
        );
    }

    println!();
    println!("clusters:              {}", clusterer.speakers());
    println!("confirmed characters:  {}", clusterer.confirmed());
    println!("windows per voice:     {:?}", clusterer.counts());
    println!("windows skipped as too quiet: {skipped}");
    match clusterer.closest_pair() {
        Some(worst) => {
            println!("closest two voices:    {worst:.3} (threshold {threshold:.2})");
            if worst > threshold - 0.05 {
                println!("  these two are nearly merged; the threshold may be too low.");
            }
        }
        None => println!("only one voice was heard, so there is nothing to compare."),
    }
    Ok(())
}

/// Confirm the soundtrack can be heard, and how loud it is.
///
/// The question this answers is whether ScreenCaptureKit audio rides the Screen
/// Recording grant already given, or asks for a second permission. A second one
/// would be a serious problem: the person this is built for cannot see a
/// dialog, so every extra grant is another thing a sighted helper must do.
fn check_audio(opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    use readsubtitles_capture::audio::AudioCapture;
    use std::sync::atomic::Ordering;

    let target = opts.target.clone();
    println!("listening to system audio for 10s - play something with sound now\n");

    let capture = AudioCapture::start(&target)?;
    for second in 1..=10 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        println!(
            "  [{second:>2}s] buffers {:<6} samples {:<9} loudness {:.4}",
            capture.stats.buffers.load(Ordering::Relaxed),
            capture.stats.samples.load(Ordering::Relaxed),
            capture.ring.rms(1.0),
        );
    }

    let samples = capture.stats.samples.load(Ordering::Relaxed);
    let unreadable = capture.stats.unreadable.load(Ordering::Relaxed);
    let loudness = capture.ring.rms(2.0);
    println!();

    if samples == 0 {
        println!("no audio arrived at all.");
        println!("the capture started, so this is not a permission problem;");
        println!("most likely nothing was playing.");
    } else if loudness < 0.0005 {
        println!("audio is arriving but it is silent ({samples} samples).");
        println!("check that the film is actually playing and not muted.");
    } else {
        println!("audio works: {samples} samples, loudness {loudness:.4}.");
    }
    if unreadable > 0 {
        println!("warning: {unreadable} buffer(s) arrived in a layout we could not read.");
    }
    Ok(())
}

fn check_bridge(opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let settings = settings::Settings::load();
    let port = if opts.bridge_port > 0 {
        opts.bridge_port
    } else {
        settings.bridge_port
    };

    let token_path = std::path::Path::new(&std::env::var("HOME")?)
        .join("Library/Application Support/readsubtitles/bridge-token");
    let Ok(token) = std::fs::read_to_string(&token_path) else {
        println!("FAIL  no bridge token at {}", token_path.display());
        println!("      run ./bundle/make-extension.sh first");
        std::process::exit(1);
    };
    let token = token.trim().to_string();

    let config_path = std::path::Path::new("extension/config.js");
    match std::fs::read_to_string(config_path) {
        Ok(c) if c.contains(&token) => println!("extension/config.js matches the stored token"),
        Ok(_) => println!(
            "WARNING  extension/config.js has a different token; re-run ./bundle/make-extension.sh \
             and reload the extension"
        ),
        Err(_) => {
            println!("WARNING  extension/config.js is missing; run ./bundle/make-extension.sh")
        }
    }

    let (server, captions) = readsubtitles_browser::CaptionServer::start(port, token)?;
    println!("listening on 127.0.0.1:{}", server.port);
    println!("play something with subtitles now; Ctrl-C to stop\n");

    install_interrupt_handler();
    let deadline = opts.duration.map_or_else(
        || Instant::now() + Duration::from_secs(60),
        |d| Instant::now() + d,
    );

    let mut seen = 0usize;
    while RUNNING.load(Ordering::Relaxed) && Instant::now() < deadline {
        if let Ok(caption) = captions.recv_timeout(Duration::from_millis(200)) {
            seen += 1;
            println!("  [{seen:>3}] {:?}", caption.text);
        }
    }

    let accepted = server.stats.accepted.load(Ordering::Relaxed);
    let rejected = server.stats.rejected.load(Ordering::Relaxed);
    println!("\n{accepted} caption(s) accepted, {rejected} rejected");

    if accepted == 0 && rejected == 0 {
        println!();
        println!("Nothing reached the app at all. In order of likelihood:");
        println!("  1. The extension is not loaded. Open chrome://extensions in the");
        println!("     browser you watch in, enable Developer mode, Load unpacked,");
        println!(
            "     and choose {}/extension",
            std::env::current_dir()?.display()
        );
        println!("  2. The extension is loaded but the page was open beforehand.");
        println!("     Reload the video tab; content scripts only attach on load.");
        println!("  3. The site is not in the extension's match list (see manifest.json).");
    } else if rejected > 0 && accepted == 0 {
        println!();
        println!("Posts arrived but were refused: the extension's token does not match.");
        println!("Re-run ./bundle/make-extension.sh, then reload the extension.");
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // No arguments means the menu bar app, which is how the end user runs it.
    // The flags exist for setup and for testing the pipeline without the shell.
    if std::env::args().nth(1).is_none() {
        return menubar::run(settings::Settings::load());
    }

    let opts = parse_args();
    if opts.check_voice {
        return check_voice(&opts);
    }
    if opts.check_region {
        return check_region();
    }
    if opts.check_speakers {
        return check_speakers(&opts);
    }
    if opts.check_audio {
        return check_audio(&opts);
    }
    if opts.check_bridge {
        return check_bridge(&opts);
    }
    if opts.menubar {
        let mut s = settings::Settings::load();
        opts.apply_to(&mut s);
        if opts.save {
            s.save()?;
            println!("settings saved to {}", settings::settings_path().display());
        }
        return menubar::run(s);
    }
    if opts.save {
        let mut s = settings::Settings::load();
        opts.apply_to(&mut s);
        s.save()?;
        println!("settings saved to {}", settings::settings_path().display());
        return Ok(());
    }

    // The CLI drives the same engine the menu bar app does. Keeping one
    // pipeline matters more than the convenience of a bespoke loop here: a
    // second copy is where the two quietly drift apart, and it is how
    // `--source` came to be silently ignored in CLI mode.
    // Multi-voice settings have no command-line flags; they live in the
    // settings file because a cast list is not something anyone types twice.
    let settings_now = settings::Settings::load();
    let config = engine::EngineConfig {
        source: opts.source,
        bridge_port: opts.bridge_port.max(1),
        target: opts.target.clone(),
        band: opts.band,
        language: opts.language.clone(),
        level: opts.level,
        fps: opts.fps,
        multi_voice: opts.multi_voice.unwrap_or(settings_now.multi_voice),
        diarize: opts.diarize.unwrap_or(settings_now.diarize),
        listen_ahead: std::time::Duration::from_millis(settings_now.listen_ahead_ms),
        cast_overrides: settings_now.cast_overrides.clone(),
        debug_regions: opts.debug_regions,
        on_browser_silence: None,
        verbose: true,
    };

    let speaker: engine::SpeakerFactory = if opts.dry_run {
        Box::new(|| None)
    } else {
        engine::speaker_factory(
            opts.elevenlabs
                .clone()
                .or_else(|| settings_now.elevenlabs_voice.clone()),
            settings_now
                .elevenlabs_cast
                .iter()
                .map(|e| e.voice_id.clone())
                .collect(),
            opts.voice.clone(),
            opts.language.clone(),
            opts.rate,
            None,
        )
    };

    let mut running = engine::start(config, speaker)?;
    println!("watching {}", running.watching);

    install_interrupt_handler();
    let deadline = opts.duration.map(|d| Instant::now() + d);
    while RUNNING.load(Ordering::Relaxed) {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        std::thread::sleep(TICK);
    }

    let spoken = running.stats.spoken.load(Ordering::Relaxed);
    let cut_short = running.stats.dropped.load(Ordering::Relaxed);
    running.stop();
    println!("\nspoke {spoken} line(s); {cut_short} cut short");
    Ok(())
}

static RUNNING: AtomicBool = AtomicBool::new(true);

/// Stop cleanly on Ctrl-C without taking on a dependency for it.
fn install_interrupt_handler() {
    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    extern "C" fn on_interrupt(_: i32) {
        RUNNING.store(false, Ordering::Relaxed);
    }
    const SIGINT: i32 = 2;
    let handler: extern "C" fn(i32) = on_interrupt;
    unsafe { signal(SIGINT, handler as usize) };
}
