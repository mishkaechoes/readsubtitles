---
date: 2026-11-13
topic: spoken-subtitles-macos
type: research
project: readsubtitles (empty repo, greenfield)
status: draft
prompt: |
  Objective: A mac software that watches a portion of the screen for subtitles (or full screen) // ideal use case is youtube and netflix (this should just be an overlay that sits above the browser and takes in raw screen input) Think loom drawing a box. Subtitles are then read out load using a defined eleven labs voice. This is to support the blind

  /research for solutions. This will not be widely distributed but just to help my grandparents so no money will made from this. Think of it as a research project

  Short cut keys start it and close it and it lives in the taskbar for settings
---

# Spoken subtitles on macOS: screen-region OCR to speech

## TL;DR

The idea works, and the single biggest risk turned out to be a non-risk.
A screenshot taken during this session shows that macOS blanks the DRM-protected *video* layer to black while the player's subtitle overlay survives capture as white text on pure black, which is close to the best input an OCR engine can get.
The recommended build is a Rust menu-bar app on ScreenCaptureKit for region capture, Apple's Vision framework for on-device OCR (free, no network, no API key), and a pluggable voice backend, with pixel-hash change detection so OCR and speech only fire when the subtitle actually changes.
On the two trade-offs that needed deciding: ElevenLabs costs roughly $2.40 of API spend per two-hour film and about $99/month at two hours a day, so v1 defaults to the free on-device Apple voice and keeps ElevenLabs as an opt-in setting behind the same interface.
The remaining structural constraint is that a blind primary user cannot see macOS permission dialogs, which makes setup a sighted-helper task and makes all of the app's own feedback necessarily audible.

## Context and motivation

The goal is a personal tool so that blind grandparents can follow foreign-language film and television, where the dialogue is in a language they do not speak and the only translation on offer is burned-in or player-rendered subtitles they cannot read.
This is an established accessibility need with an established name: *spoken subtitles*, or audio subtitling.
Dutch public broadcasting has shipped it since 2002, originally driven by the blind federation FNB with NOS, and later moved to an OCR pipeline that converts in-vision subtitles to speech in real time ([Verboom & Crombie, "Spoken Subtitles: Making Subtitled TV Programmes Accessible", ICCHP 2002](https://link.springer.com/chapter/10.1007/3-540-45491-8_62)).
What is missing is a consumer-side version that works on whatever the person is actually watching, which on a Mac means Netflix and YouTube in a browser.

Explicit constraints from the request: Mac, a Loom-style drag-a-box region picker, a defined ElevenLabs voice, start/stop hotkeys, a menu-bar item for settings, personal use only, no distribution, no revenue.
A session hook sets the implementation language to Rust.

## Project fit analysis

The repository is empty.
No git history, no manifests, no existing conventions to honour or conflict with, so every structural decision here is unconstrained.
That cuts both ways: there is no refactoring impact to weigh, and also no in-house pattern to borrow, so the project inherits its conventions from the Rust and macOS ecosystems rather than from the repo.

Two project-level constraints come from the platform rather than the code:

The app must ship as a real `.app` bundle, not a bare `cargo build` binary.
The `screencapturekit` crate's README is blunt about this: ScreenCaptureKit is purely TCC-gated, there is no entitlement that grants capture access, and "the system terminates apps that trigger the Screen Recording TCC prompt without" `NSScreenCaptureUsageDescription` in `Info.plist` ([doom-fish/screencapturekit-rs](https://github.com/doom-fish/screencapturekit-rs)).
A separate report on macOS 26.1 says unbundled executables do not even appear in the Screen Recording list ([trycua/cua#870](https://github.com/trycua/cua/issues/870)).
So bundling is a day-one concern, not a packaging afterthought.

TCC grants are keyed to code signature.
Re-signing with a fresh ad-hoc identity on every build resets the grant and re-prompts.
Creating one self-signed certificate in the login keychain and signing every build with it keeps the grant stable across rebuilds, which matters a lot when the end user cannot see the dialog.

The natural layout, with no legacy to accommodate:

```
readsubtitles/
  Cargo.toml                  # workspace
  crates/
    capture/                  # ScreenCaptureKit stream, region crop, frame hashing
    ocr/                      # Vision VNRecognizeTextRequest via objc2-vision
    cues/                     # change detection, dedupe, cue assembly, speak queue
    voice/                    # ElevenLabs streaming client + on-disk cache + rodio sink
    app/                      # AppKit shell: menu bar, hotkeys, region picker, settings
  docs/research/2026-11-13-spoken-subtitles-macos.md
  bundle/Info.plist
```

`cues` is the only crate with real logic worth unit-testing in isolation, and it is deliberately free of macOS types so it can be tested on fixtures.

## Findings by source category

### Empirical, from this session

A screenshot of DRM-protected playback captured on this machine (macOS 27.0, build 26A428) shows a fully black video area with the subtitle line "Yeah. It was a six-hour surgery." rendered crisp and white across the bottom.
This is the single most load-bearing finding in the document, and it is first-hand rather than sourced.
It tells us three things.

Subtitles are composited outside the protected video surface, so they survive capture.
That matches how the web players work: Netflix fetches a TTML subtitle file and renders cues into a `div` above the video rather than burning them into the video stream ([wavesurfer.js, "Netflix subtitles"](https://wavesurfer-js.org/blog/netflix-subs.html)).
The capture-time background is pure black rather than moving video, which removes the hardest part of subtitle OCR.
And the project does not need to touch DRM at all, since it reads pixels the system has already chosen to show.

### Official documentation

ScreenCaptureKit is the supported capture path and is TCC-gated with no entitlement escape hatch; `NSScreenCaptureUsageDescription` is mandatory ([screencapturekit-rs README](https://github.com/doom-fish/screencapturekit-rs)).

Apple's Vision framework exposes `VNRecognizeTextRequest` with two recognition levels.
WWDC19's session on the framework describes `.fast` as character-detection based and `.accurate` as neural-network based, materially better on non-standard fonts ([WWDC19-234 notes](https://wwdcnotes.com/documentation/wwdc19-234-text-recognition-in-vision-framework/)).
The Objective-C `VNRecognizeTextRequest` API is not deprecated, which matters for Rust: Apple's newer Swift-only Vision API is unreachable from `objc2`, while the older one is fully bound.

ElevenLabs lists Flash v2.5 at roughly 100 ms latency, 32 languages, a 40,000-character request limit, and "50% lower price per character for API generations" ([ElevenLabs TTS capabilities](https://elevenlabs.io/docs/capabilities/text-to-speech)).
Output formats include PCM S16LE at 16–44.1 kHz, which matters because PCM skips MP3 decode latency entirely.
The API pricing page puts Flash/Turbo at $0.04 per 1K characters against $0.08 for the multilingual models, with a free tier of 20,000 Flash characters per month and Creator at $22/month for 550,000 ([ElevenLabs API pricing](https://elevenlabs.io/pricing/api)).

### Open source projects

`screencapturekit` 11.x is the maintained Rust binding.
It supports macOS 12.3+ with feature-gated capabilities, requires Rust 1.82 and the macOS 13 SDK to build, exposes frames through `SCStreamOutputTrait` or a closure handler, recommends BGRA, and offers zero-copy `IOSurface` access ([README](https://github.com/doom-fish/screencapturekit-rs)).
It also churns: 9.0 reworked lifecycles, 10.0 revised safety contracts, 11.0 turned panics into `Result`s.
That is healthy maintenance, but it means pinning an exact version and reading `docs/MIGRATION.md` before any bump.

`objc2-vision` 0.3.2 exposes `VNRecognizeTextRequest`, `VNImageRequestHandler` and `VNRecognizedTextObservation` with no deprecation markers on those types ([docs.rs](https://docs.rs/objc2-vision/latest/objc2_vision/)).
Revisions 1 and 2 of the text request are deprecated; revision 3 is not.
Two real projects call it from Rust today: a Kadra PR wraps it at `src/ocr.rs` with a fixed language list and `.accurate` level ([mbr6300/Kadra#5](https://github.com/mbr6300/Kadra/pull/5)), and garage-rag runs it in-process as a `vision::Vision` engine ([lwm-luminx/garage-rag#297](https://github.com/lwm-luminx/garage-rag/pull/297)).
So the binding is used in anger, not just published.

The closest prior art to this project is not an accessibility tool but the live game-subtitle translators, which solved the same capture → change-detect → OCR → act loop.
`Genshin-Subtitles` runs OCR only when its frame detector sees the text change, on user-selected regions ([qew21/Genshin-Subtitles](https://github.com/qew21/Genshin-Subtitles)).
`RSTGameTranslation` adds two refinements worth copying directly: a text-similarity threshold that skips work when new OCR output is close to the previous result, and a "settle time" that waits for animating text to stop before reading it ([RSTGameTranslation](https://thanhkeke97.github.io/RSTGameTranslation/)).
`language-translator-for-games` excludes its own overlay from capture so it does not read its own output, which is an easy bug to ship and an easy one to avoid ([Dolapderelisemih](https://github.com/Dolapderelisemih/language-translator-for-games)).

On macOS specifically, `geragarazagh/ocr` is a Swift menu-bar app that does hotkey → drag region → Vision OCR → clipboard ([GitHub](https://github.com/geragarazagh/ocr)).
It is the region-picker half of this project, already built, in Swift, and worth reading for the interaction model even though we are writing Rust.

For the AppKit shell: a non-activating `NSPanel` still takes key focus on click unless `becomesKeyOnlyIfNeeded` is set, a fix shipped against `objc2` 0.6.4 / `objc2-app-kit` 0.3.2 ([OpenLogi#2152](https://github.com/AprilNEA/OpenLogi/pull/2152)), and panels hide on app deactivation by default unless that is turned off ([Unwait, "Building a macOS overlay that never steals focus"](https://unwait.ai/blog/macos-overlay-that-never-steals-focus)).

### Community discussion and practitioner write-ups

Screen Recording permission re-prompts periodically on recent macOS.
Sources disagree on cadence: one says Tahoe kept Sequoia's periodic re-authorisation but removed the "allow for one month" button ([hop.tools](https://hop.tools/blog/the-screen-recording-permission-that-keeps-coming-back/)), another says weekly ([Skilly](https://tryskilly.app/learn/enable-screen-recording-permission-macos/)), and Sequoia-era reporting said monthly.
A Hacker News thread claims the `SCContentSharingPicker` exemption is narrower than commonly described, with a commenter reporting prompts even through a newer screenshot call ([HN 41598112](https://news.ycombinator.com/item?id=41598112)).
MDM can bypass it via `forceBypassScreenCaptureAlert`, introduced around macOS 15.1 ([Jamf community](https://community.jamf.com/t5/jamf-pro/monthly-tccc-prompts-macos-v15/m-p/339150)).
None of this is from Apple documentation, and the disagreement is itself the finding: assume a recurring visual prompt will appear, design for it, and verify the actual cadence on the target machine.

Vision OCR timings in the wild are anecdotal and scattered.
One developer reports `.fast` returning almost instantly against `.accurate` taking seconds; WWDC19 cites roughly 0.25 s versus 2 s on a small note image.
A Maccy issue clocks `.fast` at about 0.009 s ([p0deje/Maccy#1514](https://github.com/p0deje/Maccy/issues/1514)).
Accuracy comparisons conflict, with at least one practitioner finding `.fast` beating `.accurate` on their content.
Nobody benchmarks a short, high-contrast, single-line strip, which is exactly our case, so this has to be measured rather than assumed.

There is no public API to duck another app's audio.
`AVSpeechSynthesizer` ducks only your own app's audio.
The two workable routes are scripting the system output volume, which also attenuates your own voice, or Core Audio process taps on macOS 14.2+, which give genuine per-process control at the cost of a System Audio Recording permission ([daily.dev on per-app volume](https://daily.dev/posts/macos-still-won-t-let-you-turn-down-one-app-so-i-built-it-myself--spld9z3cr), [tts-mcp#43](https://github.com/bborbe/tts-mcp/pull/43)).

Feeding a streamed MP3 into rodio is a known rough edge: a non-seekable `ReadOnlySource` through symphonia fails at runtime, and the community answer is the `stream-download` crate ([rodio#580](https://github.com/RustAudio/rodio/issues/580), [somafm rodio backend](https://docs.rs/crate/somafm/0.4.0/source/src/audio/rodio.rs)).
Requesting PCM from ElevenLabs instead of MP3 sidesteps the whole problem.

### Research papers

The Dutch spoken-subtitles work is the foundational reference and is explicit that the audience is wider than blind users, covering elderly viewers and people with aphasia or dyslexia ([ICCHP 2002](https://link.springer.com/chapter/10.1007/3-540-45491-8_62)).
Audio subtitling is positioned in the localisation literature as a distinct modality from audio description, and a cheaper one, which is why smaller-language countries adopted it first ([Braun & Orero, "Audio description with audio subtitling", 2010](https://www.researchgate.net/publication/254334326_Audio_description_with_audio_subtitling_-_an_emergent_modality_of_audiovisual_localisation)).
A European Blind Union survey found blind and partially sighted viewers underserved by both modalities, with no country legislating audio subtitling ([EBU survey report](https://www.viziris.nl/lobbytrajecten/informatie-en-lectuur/3-werkterreinen_3-3-ilv_lobbytraject-toeg-digi-tv_ebu-serveyreport.doc)).

## Step 1 spike results (measured 2026-11-13)

Every number below came from running the spike in `spike/` on this machine (macOS 27.0 build 26A428, Apple silicon, Rust 1.100.0, `screencapturekit` 11.1.2, `objc2-vision` 0.3.2).
They replace the estimates and open questions the desk research left behind.

### Vision OCR is far faster than the published anecdotes suggested

On the real DRM subtitle fixture (`fixtures/netflix-drm-subtitle.png`, 873x289, white text on black), 30 iterations each:

| Level | Language correction | Median | Min | Max | Text |
|---|---|---|---|---|---|
| `.fast` | off | 6.6 ms | 4.7 ms | 20.7 ms | exact |
| `.fast` | on | 6.5 ms | 5.6 ms | 20.6 ms | exact |
| `.accurate` | off | 35.9 ms | 27.2 ms | 44.1 ms | exact |
| `.accurate` | on | 34.1 ms | 29.1 ms | 57.6 ms | exact |

All four configurations returned `Yeah. It was a six-hour surgery.` character-for-character, hyphen and full stops included.
The practitioner reports of `.accurate` taking one to two seconds were measured on full-resolution photographs; on a subtitle strip it is 35 ms.
`.accurate` is affordable, so there is no reason to trade accuracy for speed. **Resolved: use `.accurate`.**

### The model compile is a one-time 26-second stall, and it must not happen mid-film

The first `.accurate` call in a fresh install took 28.1 s, and the first call against a differently-sized image took 26.5 s again.
Subsequent calls in the same process and in later processes settle at 35–250 ms depending on region size.
This is Vision compiling its model for the device, cached afterwards.
**Consequence for the app:** warm the recognizer during setup and at launch with a frame the size of the configured region, before the user ever presses the start hotkey. A 26-second silence on the first subtitle of the first film would read as a broken app.

### `source_rect` crops in the compositor, confirmed

`SCStreamConfiguration::with_source_rect` exists in `screencapturekit` 11.1.2 (`src/stream/configuration/dimensions.rs`) and works.
Requesting a 768x540 region off a 960x540 display delivered a 768x540 buffer, not a full-display frame.
**Resolved: no in-process cropping needed.**

### ScreenCaptureKit already does coarse change detection

Configured at 6 fps against a near-static screen, the stream delivered 1.4–1.7 frames per second.
ScreenCaptureKit does not send duplicate frames for unchanged content, so the requested fps is a ceiling rather than a rate.
This is free work in our favour: idle cost is near zero without any effort on our side.

Our own sparse-grid change signature costs **0.015–0.043 ms per frame**, which is noise next to everything else.

### Two hard constraints discovered by hitting them

Both of these are the kind of thing no amount of desk research surfaces.

**Vision request objects are neither `Send` nor `Sync`.** `Retained<VNRecognizeTextRequest>` cannot be captured by a `SCStream` output handler closure, because the crate requires `Fn + Send + Sync + 'static`. The compiler catches this one.

**Calling Vision synchronously from the ScreenCaptureKit dispatch callback never returns.** It does not error and does not panic; the call simply does not come back, and the frame is silently lost. This one is invisible until you instrument it.

Together they force the architecture: the capture callback does a signature check and a `memcpy` into a bounded channel, and a dedicated worker thread owns the Vision request and does the recognition. The bounded channel is also where the backlog policy belongs, since dropping a stale frame is better than reading out a subtitle that left the screen seconds ago. The spike drops frames on a full channel and counts them.

### Build and environment notes

An unbundled binary captured successfully with no TCC prompt and no termination, inheriting the parent terminal's Screen Recording grant. That is a development convenience only; the shipped `.app` still needs `NSScreenCaptureUsageDescription`.

On a machine with Command Line Tools but no full Xcode, `screencapturekit` fails to link, because its build script emits a Swift search path under `Toolchains/XcodeDefault.xctoolchain` that does not exist. It then fails at launch with `Library not loaded: @rpath/libswift_Concurrency.dylib`. Both are fixed by `.cargo/config.toml`:

```toml
[target.aarch64-apple-darwin]
rustflags = [
  "-L", "/Library/Developer/CommandLineTools/usr/lib/swift/macosx",
  "-C", "link-arg=-Wl,-rpath,/usr/lib/swift",
]
```

### The Vision model compile is per region size, and it is permanent

Refining the earlier finding. The 26-second compile is not once per machine, it is once per distinct input size:

| Region | First call | Later calls, same size |
|---|---|---|
| 873x289 | 28.1 s | 35 ms |
| 768x540 | 26.5 s | 85-250 ms |
| 873x393 | 25.5 s | 180 ms |

Once a size has been compiled it stays cached across processes and reboots.
**Consequence:** warm up with the exact configured region size immediately after the sighted helper draws the box, and say out loud that the first run takes about half a minute. Changing the region later pays the cost again.

### Shutdown trap: the stream's handler owns the frame sender

Calling `stop_capture()` does not release the output handler, so the channel sender inside it stays alive, the consuming thread's `for frame in rx` never ends, and the process hangs on join. The capture object has to be dropped, not just stopped. Cost us a 300-second hang before it was spotted.

### End-to-end latency, measured

From the pixel changing on screen to a settled cue ready to speak: **181-196 ms**, of which 180 ms is the deliberate settle window. Recognition and capture together are under 20 ms. Adding the on-device voice, which starts speaking with no network round trip, puts spoken output roughly a fifth of a second behind the subtitle appearing. That is comfortably inside what a listener reads as "in sync".

### The DRM blanking is capture-side only

Worth stating plainly because it is easy to misread the black screenshot as a problem: FairPlay blanks protected video in screen captures and screenshots, not on the physical display. Anyone in the room watching the film sees it normally. Only our capture stream sees black behind the subtitles, which is exactly the input we want.

### Still open after the spike

`CGDisplayPixelsWide` returned 960 on a display running a 2x scaled mode over a 1920x1080 panel, so it reports points rather than backing pixels and cannot be used to derive the Retina scale factor. `CGDisplayModeGetPixelWidth` is the likely answer and has not been tested. Capturing at 1x on a Retina panel halves the pixels Vision sees, which is the one thing in the pipeline that could still cost accuracy.

OCR accuracy against subtitles over moving video (the YouTube case) is untested. The Netflix case is settled by the fixture, and it is the easy one, because DRM gives us pure black behind the text.


## Approaches

### Recommended: Rust menu-bar app, ScreenCaptureKit → Vision → pluggable voice

Capture a cropped region at 4–6 fps through `screencapturekit` in BGRA.
Hash each frame of the region cheaply and only wake the rest of the pipeline on change, which is what every game-translator project converged on and what keeps idle CPU near zero.
Wait a short settle window after a change so animating or progressively-revealed text is read once, not three times.
Run Vision `VNRecognizeTextRequest` on the stable frame, feeding the `CVPixelBuffer` straight to `VNImageRequestHandler` with no intermediate encode.
Normalise the result, compare against the last spoken cue with a similarity threshold, and enqueue on genuine change.
Speak through a pluggable backend: on-device `AVSpeechSynthesizer` by default, ElevenLabs Flash v2.5 streamed as PCM S16LE into a `rodio` sink when the setting is switched on.
Cache synthesised ElevenLabs audio on disk keyed by hash of text plus voice plus model, which makes repeat lines and re-watches free.

Why this shape: it is the only option that covers Netflix, YouTube, Disney+, local files, and anything else on the screen with one code path, because it reads pixels rather than integrating per-site.
The DRM screenshot finding means the hardest-looking target is in fact the easiest input.
OCR is free and on-device, so the only recurring cost is TTS.

Why Rust specifically is a cost, honestly stated: the AppKit shell (status item, non-activating panel, drag-to-select overlay, run loop) is the part where Rust buys nothing and costs the most, since every line is `objc2` ceremony against APIs designed for Swift.
The capture, hashing, cue logic, HTTP streaming and audio pipeline are where Rust is genuinely pleasant.

### Alternative A: Swift app

Everything above, in the language Apple designed for it.
Perhaps a third of the code, access to the newer Swift-only Vision API, and no binding-churn exposure.
Rejected here only because Rust is a stated constraint, not because it is worse on the merits.

### Alternative B: Rust core with a thin Swift shell

Keep `cues`, `voice` and the HTTP/audio work in Rust, expose a small C ABI, and write the AppKit shell in Swift.
This removes the worst of the `objc2` ceremony while keeping most of the project in Rust.
Costs a build-system seam and a second toolchain.

### Alternative C: Browser extension reading captions from the DOM

No screen capture, no OCR, no Screen Recording permission, and perfectly accurate text, because Netflix and YouTube both render cues as DOM elements.
Fails on anything outside the browser, needs per-site selectors that break when players change (Netflix moved to v4 manifests and rebuilds its subtitle DOM during navigation, per [captionary#8](https://github.com/mxggle/captionary/pull/8)), and an extension cannot easily own a native audio pipeline or menu-bar UI.
Worth keeping as a possible later fast path for YouTube, not as the foundation.

### Alternative D: Fetch the subtitle track instead of reading the screen

For YouTube, pull the timed-text track directly.
Attractive in principle and fragile in practice: the `timedtext` endpoint is undocumented, aggressively rate-limited with 429s reported through 2026, and may now require a BotGuard Proof-of-Origin Token ([dev.to, 2026](https://dev.to/jamhimself/why-your-youtube-transcript-scraper-started-returning-empty-strings-and-how-to-fix-it-in-2026-20ed)).
No equivalent exists for Netflix.
Also solves a different problem, since it gives you the whole transcript rather than what is on screen right now, so you would still need playback-position sync.

### Comparison

| | Screen OCR (recommended) | Swift rewrite | Rust core + Swift shell | Browser extension | Subtitle-track fetch |
|---|---|---|---|---|---|
| Netflix coverage | Yes, DRM-safe | Yes | Yes | Yes, until DOM changes | No |
| YouTube coverage | Yes | Yes | Yes | Yes | Fragile (429/PoToken) |
| Non-browser coverage | Yes | Yes | Yes | No | No |
| Rust fit (stated constraint) | Full | None | Partial | None | Partial |
| Build cost | High (AppKit via objc2) | Low | Medium | Low | Medium |
| Maintenance burden | Binding churn, Apple TCC changes | Apple changes only | Two toolchains | Per-site breakage, high | Endpoint breakage, high |
| Recurring cost | ElevenLabs only | ElevenLabs only | ElevenLabs only | ElevenLabs only | ElevenLabs only |
| Permissions needed | Screen Recording | Screen Recording | Screen Recording | None | None |
| Accuracy ceiling | OCR errors possible | Same | Same | Exact text | Exact text |
| Reversibility | Two-way door | Two-way door | Two-way door | One-way-ish | Two-way door |

## Risks and unknowns

**Apple changes DRM compositing so subtitles get blanked too.**
The whole project rests on an observed behaviour, not a documented guarantee.
- Keep a documented fallback to Alternative C for Netflix and YouTube, since DOM captions are unaffected by compositing changes.
- Pin an OS version on the grandparents' machine and do not auto-update it without re-testing the screenshot case.
- Add a startup self-check that captures one frame and reports "no text found in region" audibly, so a regression announces itself instead of failing silently.

**Screen Recording permission re-prompts, and a blind user cannot see the dialog.**
Cadence is disputed across sources and none are Apple's.
- Have the sighted helper grant permission during setup and verify the prompt interval empirically on the actual machine.
- Detect capture failure and speak a clear instruction rather than dying silently.
- Investigate whether an MDM profile with `forceBypassScreenCaptureAlert` is viable on a personally-owned machine; it is designed for managed fleets but the machine is yours to manage.

**OCR accuracy and latency are unmeasured for this exact input.**
Practitioner timings for `.accurate` range from 0.25 s to 2 s on full images, with no data on small high-contrast strips.
- Benchmark both recognition levels on real captured strips in week one; this is a ~1 hour experiment that de-risks the core loop.
- Constrain `recognitionLanguages` and set `usesLanguageCorrection` deliberately, since subtitle text is ordinary prose and correction should help.
- If `.accurate` is too slow, run `.fast` first and only escalate when confidence is low.

**TTS falls behind rapid dialogue.**
Subtitle cues can land every 1.5 s while a natural reading of the line takes 2.5 s.
- Implement an explicit backlog policy: drop the oldest unspoken cue rather than queueing indefinitely, so speech stays aligned with the picture.
- Expose a speaking-rate multiplier in settings; experienced screen-reader users commonly run well above 1.0x.
- Merge cues that arrive inside the settle window into one utterance rather than two.

**ElevenLabs cost at daily-viewing volume.**
A two-hour foreign-language film runs roughly 60,000 subtitle characters, which is about $2.40 at $0.04/1K.
Two hours a day is roughly 1.8M characters a month, which is above the Creator plan's 550,000 and lands near Pro at $99/month.
Largely defused by the decision to default to the on-device Apple voice, which makes ElevenLabs an opt-in preference rather than a dependency.
- Cache aggressively on disk by content hash; re-watches and recurring lines become free.
- Add a monthly character budget with an audible warning, and fall back to the Apple voice rather than going silent mid-film.
- Measure actual characters over one full film before choosing a plan; the 60,000 figure is an estimate, not a measurement.

**TTS voice competes with the film's own audio.**
Decided: v1 does not touch the film's audio at all.
The helper sets the browser volume once during setup.
- If real use shows this is a problem, scripting system output volume during utterances is the cheap next step, with output gain to compensate for it attenuating the TTS too.
- The clean solution is a Core Audio process tap (macOS 14.2+) ducking only the browser, at the cost of another permission the blind user cannot see. Deliberately out of scope for v1.

**Region selection breaks when the window moves or fullscreen toggles.**
- Store the region relative to the display and expect fullscreen playback, which is the realistic usage and is stable.
- Make the hotkey re-open the picker so a sighted helper can fix it in seconds.
- Consider auto-detecting the subtitle band by running OCR across the lower third once at start and locking onto wherever text is found.

**Rust binding churn.**
`screencapturekit` has had three consecutive majors with breaking changes.
- Pin exact versions in `Cargo.toml` and commit `Cargo.lock`.
- Keep all `screencapturekit` and `objc2-vision` calls behind the `capture` and `ocr` crate boundaries so a bump touches two files.
- Read `docs/MIGRATION.md` before any upgrade; do not upgrade without a reason.

**Reading its own output.**
If the app ever draws on-screen text, capture will read it back.
- Exclude the app's own windows from the `SCContentFilter`, as `language-translator-for-games` does.
- Keep the running-state UI audible and in the menu bar rather than on the video.

## Demand validation

Worth including here because the end user is the point of the project.
The need is documented rather than assumed: Dutch public broadcasting has run spoken subtitles since 2002 on blind-federation initiative, later automating it with real-time OCR of in-vision subtitles, and the literature is explicit that the beneficiaries extend to elderly viewers and people with dyslexia or aphasia ([ICCHP 2002](https://link.springer.com/chapter/10.1007/3-540-45491-8_62)).
A European Blind Union survey found no country legislates audio subtitling and that viewers are underserved by both it and audio description.
Streaming services have not closed that gap for foreign-language catalogue content on a per-title basis, which is precisely the case here.
The demand question for a two-person user base is not "is there a market" but "will they actually use it", and the honest answer depends more on latency and operability than on the feature existing.

## Market and competitive landscape

Thin, which is itself informative.
On macOS, the closest shipped tools are region-OCR-to-clipboard utilities such as `geragarazagh/ocr` with no speech, and general screen readers like VoiceOver which read UI chrome rather than a video overlay region.
On Windows there are region-OCR read-aloud tools (`Z3n1thh/Speech`, Capture2Text) and a cluster of live game-subtitle translators that are technically the closest relatives to this project.
Broadcast-side spoken subtitling exists in the Netherlands and a few other European markets but is delivered as an audio track by the broadcaster, not as something a viewer can apply to Netflix.
Nothing found does screen-region subtitle OCR to high-quality TTS on macOS for streaming video, so there is no product to copy and no product to buy instead.

## Open questions

Vision OCR latency and accuracy on a short, white-on-black subtitle strip, `.fast` versus `.accurate`, measured on the target hardware.
No source benchmarks this case.

Whether `screencapturekit` 11.x exposes `SCStreamConfiguration.sourceRect` so the compositor does the cropping, or whether the stream must deliver full frames to be cropped in-process.
This changes the per-frame cost materially and was not resolvable from the README.

Whether ElevenLabs PCM output formats require a paid tier.
The capabilities page notes higher-quality options need paid plans without saying which formats that covers.

The actual Screen Recording re-prompt cadence on macOS 27.0, and whether any exemption path applies.
Sources contradict each other and none are Apple's.

Whether Netflix's subtitle overlay remains outside the protected surface across all titles and both Safari and Chrome, or whether the observed behaviour is title- or browser-specific.
One screenshot is one data point.

How reliably the subtitle band can be auto-detected, versus requiring a manual box every session.
## Sources

| URL | Title | Author / org | Published | Accessed | Category | Contribution |
|---|---|---|---|---|---|---|
| https://github.com/doom-fish/screencapturekit-rs | screencapturekit-rs | doom-fish | v11.x current | 2026-11-13 | OSS | Rust capture binding: version, macOS gates, TCC/Info.plist requirements, API shape |
| https://docs.rs/objc2-vision/latest/objc2_vision/ | objc2-vision 0.3.2 | madsmtm / objc2 | current | 2026-11-13 | docs | Confirms VNRecognizeTextRequest et al. are bound and undeprecated from Rust |
| https://github.com/mbr6300/Kadra/pull/5 | Texterkennung mit Apple Vision | mbr6300 | 2026 | 2026-11-13 | OSS | Working Rust `src/ocr.rs` using objc2-vision; closest reference implementation |
| https://github.com/lwm-luminx/garage-rag/pull/297 | garage-ocr (Vision, Tesseract, swift-rs) | rickmark | 2026 | 2026-11-13 | OSS | Second in-production Rust caller of Vision OCR |
| https://elevenlabs.io/docs/capabilities/text-to-speech | Text to Speech | ElevenLabs | current | 2026-11-13 | docs | Flash v2.5 latency, languages, char limits, PCM output formats, streaming |
| https://elevenlabs.io/pricing/api | API pricing | ElevenLabs | current | 2026-11-13 | docs | $0.04/1K Flash vs $0.08 multilingual; plan tiers and included characters |
| https://link.springer.com/chapter/10.1007/3-540-45491-8_62 | Spoken Subtitles: Making Subtitled TV Programmes Accessible | Verboom, Crombie et al. | 2002 (ICCHP) | 2026-11-13 | paper | Foundational precedent; Dutch service, audience beyond blind users |
| https://www.researchgate.net/publication/254334326_Audio_description_with_audio_subtitling_-_an_emergent_modality_of_audiovisual_localisation | Audio description with audio subtitling | Braun & Orero | 2010 | 2026-11-13 | paper | Positions audio subtitling as a distinct, cheaper modality |
| https://www.viziris.nl/lobbytrajecten/informatie-en-lectuur/3-werkterreinen_3-3-ilv_lobbytraject-toeg-digi-tv_ebu-serveyreport.doc | Future of access to television for blind and partially sighted people in Europe | European Blind Union | undated | 2026-11-13 | report | No country legislates audio subtitling; viewers underserved |
| https://wavesurfer-js.org/blog/netflix-subs.html | Netflix subtitles | wavesurfer.js | ~2015 | 2026-11-13 | blog | Netflix fetches TTML and renders cues into a div above the video |
| https://github.com/mxggle/captionary/pull/8 | Fix Netflix subtitle loading | mxggle | 2026 | 2026-11-13 | OSS | Netflix v4 manifests; subtitle DOM is rebuilt during navigation |
| https://dev.to/_funlingo_/how-chrome-extensions-inject-dual-subtitles-into-netflix-and-why-its-harder-than-it-looks-2216 | How Chrome extensions inject dual subtitles into Netflix | funlingo | 2026 | 2026-11-13 | blog | Why DOM-based approaches break; player rebuilds its own DOM |
| https://github.com/qew21/Genshin-Subtitles | Genshin-Subtitles | qew21 | current | 2026-11-13 | OSS | Change-detector gating OCR on user-selected regions |
| https://thanhkeke97.github.io/RSTGameTranslation/ | RSTGameTranslation | thanhkeke97 | current | 2026-11-13 | OSS | Text-similarity skip threshold and settle-time for animating text |
| https://github.com/Dolapderelisemih/language-translator-for-games | language-translator-for-games | Dolapderelisemih | current | 2026-11-13 | OSS | Excludes own overlay from capture to avoid reading its own output |
| https://github.com/geragarazagh/ocr | OCR for Mac | geragarazagh | current | 2026-11-13 | OSS | Swift reference for hotkey + drag region + Vision on macOS |
| https://github.com/Z3n1thh/Speech | Screen Read-Aloud | Z3n1thh | current | 2026-11-13 | OSS | Closest existing region-OCR read-aloud tool (Windows) |
| https://github.com/AprilNEA/OpenLogi/pull/2152 | Keep the macOS actions ring from taking key focus | AprilNEA | 2026 | 2026-11-13 | OSS | `becomesKeyOnlyIfNeeded` fix for non-activating NSPanel via objc2 |
| https://unwait.ai/blog/macos-overlay-that-never-steals-focus | Building a macOS overlay that never steals focus | Unwait | 2026 | 2026-11-13 | blog | Non-activating style mask; disable hide-on-deactivate |
| https://wwdcnotes.com/documentation/wwdc19-234-text-recognition-in-vision-framework/ | Text Recognition in Vision Framework | Apple / WWDCNotes | 2019 | 2026-11-13 | video | `.fast` vs `.accurate` mechanism and rough timing gap |
| https://github.com/p0deje/Maccy/issues/1514 | Image -> text is currently somewhat inaccurate | Maccy | 2026 | 2026-11-13 | community | Real-world Vision timing (~0.009s fast) and accuracy disagreement |
| https://news.ycombinator.com/item?id=41598112 | Disable Sequoia's monthly screen recording permission prompt | HN | 2024 | 2026-11-13 | community | Picker exemption may be narrower than documented |
| https://hop.tools/blog/the-screen-recording-permission-that-keeps-coming-back/ | The screen recording permission that keeps coming back | hop.tools | 2026 | 2026-11-13 | blog | Tahoe kept periodic re-auth; "allow for one month" button removed |
| https://community.jamf.com/t5/jamf-pro/monthly-tccc-prompts-macos-v15/m-p/339150 | Monthly TCC prompts, macOS v15 | Jamf community | 2024–25 | 2026-11-13 | community | `forceBypassScreenCaptureAlert` MDM key |
| https://github.com/trycua/cua/issues/870 | macOS Tahoe: screen capture only shows desktop | trycua | 2026 | 2026-11-13 | community | Unbundled binaries do not appear in the Screen Recording list |
| https://daily.dev/posts/macos-still-won-t-let-you-turn-down-one-app-so-i-built-it-myself--spld9z3cr | macOS still won't let you turn down one app | daily.dev | 2026 | 2026-11-13 | blog | Core Audio process taps as the only real per-app ducking route |
| https://github.com/bborbe/tts-mcp/pull/43 | Duck other apps' audio while the voice speaks | bborbe | 2026 | 2026-11-13 | OSS | Working ducking helper; flags audio-thread blocking risk |
| https://github.com/RustAudio/rodio/issues/580 | Streaming non-seekable source | rodio | 2026 | 2026-11-13 | community | MP3-over-HTTP into rodio fails; argues for PCM output |
| https://docs.rs/crate/somafm/0.4.0/source/src/audio/rodio.rs | somafm rodio backend | somafm | 2026 | 2026-11-13 | OSS | Working `stream-download` + rodio Sink pattern if MP3 is needed |
| https://dev.to/jamhimself/why-your-youtube-transcript-scraper-started-returning-empty-strings-and-how-to-fix-it-in-2026-20ed | Why your YouTube transcript scraper started returning empty strings | dev.to | 2026 | 2026-11-13 | blog | timedtext 429s and PoToken requirement; argues against Alternative D |
| https://humannessindex.vapi.ai/models/elevenlabs-flash-v2-5 | ElevenLabs Flash v2.5: humanness and latency | Vapi | 2026-06 | 2026-11-13 | benchmark | Measured 197ms median to first audio over 50 streams |
| https://deepgram.com/blog/how-elevenlabs-api-works-a-developers-guide | How the ElevenLabs API works | Deepgram | 2026 | 2026-11-13 | blog | Real-world latency estimate 125–225ms including network |

## Confidence

`[well-established]` ScreenCaptureKit is TCC-gated with no entitlement bypass, and `NSScreenCaptureUsageDescription` plus an `.app` bundle are mandatory. Multiple independent sources plus the binding's own README.

`[well-established]` Apple Vision `VNRecognizeTextRequest` is reachable from Rust via `objc2-vision` and is used in shipping projects.

`[well-established]` Netflix renders subtitles as a separate overlay rather than burning them into the video stream.

`[well-established]` There is no public macOS API for ducking another application's audio; process taps or system volume are the only routes.

`[well-established]` Spoken subtitles are an established accessibility modality with a 20-year deployment history in the Netherlands.

`[emerging]` ElevenLabs Flash v2.5 real-world first-audio latency sits around 125–225 ms rather than the advertised ~75–100 ms. Two independent sources, one of them a measured benchmark.

`[emerging]` macOS re-prompts for Screen Recording on a recurring schedule on current versions. Agreed in direction, disputed on cadence, no Apple source.

`[single-source]` The claim that `.fast` can beat `.accurate` on real content rests on individual practitioner reports that conflict with Apple's own framing.

`[single-source]` The YouTube `timedtext` PoToken requirement comes from one 2026 blog post.

`[speculative]` That the DRM-blanking behaviour observed in the session screenshot holds across all Netflix titles and both Safari and Chrome. One observation, one title, one browser. It is the project's central assumption and deserves a second test before the build starts.

`[speculative]` The ~60,000 characters per two-hour film estimate, and therefore the ~$2.40 per film and ~$99/month daily-viewing figures. Derived from typical subtitle density, not measured.

### Most likely to go stale within six months

ElevenLabs pricing, plan allotments and model names. The pricing page did not even use the name "Flash v2.5", listing "Flash / Turbo" instead, and third-party sources disagreed between $20/M and $50/M because of rolling promotions. Re-check before committing to a plan.

`screencapturekit` crate API. Three consecutive majors with breaking changes; 11.0 converted panics to `Result`s.

macOS Screen Recording permission behaviour. It has changed in nearly every release since Sequoia.

Netflix player DOM structure, which matters only if Alternative C is ever used as a fallback.

YouTube `timedtext` access, which is actively degrading.

---

## Open questions, answered by building (2026-11-13)

Recorded against the "Open questions" section above, which was written before any code existed.

**Vision OCR latency and accuracy on a subtitle strip.**
Answered, and the published anecdotes were misleading.
`.accurate` runs at roughly 25-35 ms on a 1536x231 strip, not the 1-2 s the WWDC-era figures suggested, and both recognition levels read the DRM fixture exactly.
`.accurate` was chosen because it costs nothing meaningful at this size.
The real cost is elsewhere: Vision compiles its model the first time it sees a given input size, which takes about 26 s and is then cached permanently.
That is why the app warms up with the configured region size at startup and says "Preparing the recognizer" before it says "Reading subtitles".

**Does `screencapturekit` 11.x expose `sourceRect`.**
Yes, and the cropping happens in the compositor, so no in-process crop is needed.
Confirmed by the delivered buffer matching the requested region rather than the full display.

**Whether ElevenLabs PCM output requires a paid tier.**
Not answered, and it turned out not to be the blocker.
The supplied key reached ElevenLabs and was rejected for scope, not for plan: `{"code":"unauthorized","message":"The API key you used is missing the permission text_to_speech to execute this operation.","status":"missing_permissions"}`.
The fallback to the on-device voice worked exactly as designed under that failure, which is the behaviour that actually mattered.

**Screen Recording re-prompt cadence on macOS 27.**
Still unanswered. It needs elapsed time to observe and cannot be measured in one session.
The mitigation is in place: capture failure is spoken as an instruction rather than failing silently.

**Whether the DRM blanking behaviour holds across titles and browsers.**
Still one title, one browser.
It held across every frame of a live playback session rather than a single screenshot, which is a stronger data point than the original one, but not a general answer.

### What the build added that the research did not anticipate

**Vision cannot be called from the ScreenCaptureKit callback.**
It does not error or panic. It never returns, and the frame is silently lost.
Combined with `VNRecognizeTextRequest` not being `Send`, this forces the thread layout the app now has: the capture callback does nothing but hash and memcpy, a dedicated thread owns the recognizer, and a third owns the voice.
This is the single most important implementation constraint in the project and no source mentioned it.

**Stopping the capture does not release it.**
`SCStream::stop` leaves the output handler, and therefore the frame sender, alive.
The capture must be dropped before joining the OCR thread or the process hangs on a channel that never closes.

**Player chrome lands in the subtitle band.**
When the controls are visible, the chapter title and timecode are recognised alongside the dialogue and spliced into the middle of the spoken sentence: `10 He talked about the opera singer's surgery as if he was the one who operated Monster E1 CHAPTER 1`.
Separating them by text content is unreliable.
Separating them by size and position is not: subtitles are set large and centred, chrome is small and clings to the edges.
The filter leans on both, and falls back to reading everything when it would otherwise silence a frame, because dropping dialogue is worse than reading a stray label.

### End-to-end latency with the ElevenLabs voice (measured 2026-11-13)

Measured on live DRM-protected Netflix playback, once the key had the `text_to_speech` permission.

| Stage | Measured | Note |
|---|---|---|
| Subtitle on screen to cue emitted | 183-208 ms | Includes the deliberate 180 ms settle window |
| ElevenLabs first byte | 171-189 ms | Three runs, Flash v2.5, `pcm_22050` |
| ElevenLabs full response | 245-253 ms | What the app actually waits for today |
| **Total to audible, uncached** | **~445 ms** | |
| **Total to audible, cached** | **~195 ms** | Repeat lines and re-watches |

Comfortably inside the "within about a second" acceptance criterion, and close enough to the on-device voice (~195 ms) that the choice between them can be made on voice quality rather than lag.

The research estimated 125-225 ms for ElevenLabs first audio, which held: 171-189 ms measured.

One easy improvement is left on the table. The client reads the whole response before playing rather than streaming it into the sink, which spends the 60-70 ms between first byte and last byte waiting instead of playing. Not worth the added complexity at 445 ms total, but it is where the next win is if the lag ever becomes a complaint.

---

## Alternative C, built (2026-11-13)

The research kept DOM caption reading as a documented fallback rather than a foundation, on the grounds that it only works inside a browser and breaks when a player's markup changes. Both remain true. It is now built anyway, as a second selectable source rather than a replacement, for a reason the research did not weigh heavily enough: **it needs no Screen Recording permission at all.**

That turned out to matter more in practice than on paper. The permission is the one part of setup a blind user cannot do alone, it re-prompts on a cadence nobody can state with confidence, and during development every rebuild re-prompted because ad-hoc signatures change with the binary. A source that sidesteps the whole mechanism is worth having even with narrower reach.

### Shape

A browser extension watches the caption element and posts each change to a loopback HTTP listener in the app. Both sources emit the same `(text, when)` pairs, so settling, dedupe, the backlog policy and the voice are shared.

The post happens in the extension's service worker rather than its content script. A content script runs in the page's origin, so a request to `127.0.0.1` would be cross-origin and subject to CORS; the service worker holds the host permission and is not.

A shared token guards the endpoint. Without one, any page the user visited could issue a "simple" cross-origin POST and make the app say whatever it liked, which for a tool a blind person relies on to know what is happening is worse than it first sounds.

### Measured

| | Screen source | Browser source |
|---|---|---|
| Lag to cue | 183-208 ms | 60 ms |
| Text | OCR, can misread | Exact |
| Permission | Screen Recording | None |

The browser source is faster because it needs almost no settle window: OCR has to wait for a subtitle to finish fading in, whereas the DOM delivers finished text. 60 ms against 180 ms.

### The permission problem, diagnosed

The repeated prompts during development were not the OS re-authorisation cadence the research worried about. TCC keys its grant to the code signature, and an ad-hoc signature is derived from the binary, so every rebuild presented itself to macOS as a different app. Signing every build with one self-signed certificate fixes it, and the certificate does not need to be trusted by anyone: `codesign` will sign with an untrusted self-signed identity, and TCC only cares that the identity is stable. That means no administrator password and no `add-trusted-cert` step. `bundle/make-cert.sh` creates it.

One wrinkle worth recording: OpenSSL 3 writes a PKCS#12 MAC that macOS's `security import` cannot verify, and fails with a misleading "wrong password?" error. The legacy `-keypbe`, `-certpbe` and `-macalg` options are required.
