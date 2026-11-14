# readsubtitles

[![CI](https://github.com/mishkaechoes/readsubtitles/actions/workflows/ci.yml/badge.svg)](https://github.com/mishkaechoes/readsubtitles/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Platform: macOS](https://img.shields.io/badge/Platform-macOS%2013%2B-lightgrey.svg)

Reads the subtitles on your screen out loud.

Built so that a blind viewer can follow foreign-language film and television, where the dialogue is in a language they do not speak and the only translation on offer is subtitles they cannot read.
This is the consumer-side version of what Dutch public broadcasting has called *spoken subtitles* since 2002.

It watches a rectangle of the screen, recognises the text in it on-device, and speaks each line once as it appears.
Because it reads pixels rather than integrating with any particular service, it works on Netflix, YouTube, a local video file, or anything else that puts text on screen.

## Two ways to read captions

| | Read the screen | Read the browser |
|---|---|---|
| How | Screen capture, then on-device OCR | A browser extension reads the caption elements |
| Works on | Anything that shows text: any site, any app, local files | Only sites the extension covers |
| Text quality | Very good, but OCR can misread | Exact, character for character |
| Screen Recording permission | Required | Not needed at all |
| Lag to spoken word | ~195 ms | ~60 ms |
| Breaks when | Rarely; it reads what you can see | A site rebuilds its player markup |

Reading the screen is the default because it always works.
Reading the browser is better when it applies, and it sidesteps the permission prompt entirely.
Switch between them in the menu bar under **Source**, or with `--source screen` / `--source browser`.

Set the browser source up once:

```sh
./bundle/make-extension.sh
```

It prints the four clicks needed to load the extension, which Chrome deliberately requires a human to do.

## It works on Netflix, and the DRM helps

macOS blanks DRM-protected video out of screen captures.
That sounds fatal and is in fact the best case: players draw subtitles in a separate layer, so the capture sees white text on pure black, which is close to ideal input for OCR.

The blanking is capture-side only. Anyone sitting watching the film sees it normally.

## Install

There is no installer or signed release. You build it, which takes about two minutes.

**Requirements:** macOS 13 or later (developed and measured on macOS 27), Rust 1.85 or later, and the Xcode Command Line Tools.

```sh
xcode-select --install                                     # if you do not have them
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # if you do not have Rust

git clone https://github.com/mishkaechoes/readsubtitles.git
cd readsubtitles

./bundle/make-cert.sh     # once: a stable signing certificate. See below.
./bundle/make-app.sh      # build and bundle
cp -R target/ReadSubtitles.app /Applications/
open /Applications/ReadSubtitles.app
```

**Run `make-cert.sh` before the first build.** It creates one self-signed certificate in your login keychain and takes a minute. macOS keys the Screen Recording permission to an app's code signature, so without a stable certificate every rebuild looks like a brand new app and the permission is asked for again. That is a nuisance for a developer and a real obstacle for a blind user, who cannot see the dialog being raised.

**Bundling is not optional either.** ScreenCaptureKit is gated purely by TCC with no entitlement that grants access, macOS terminates an app that raises the Screen Recording prompt without `NSScreenCaptureUsageDescription` in its `Info.plist`, and on macOS 26 and later an unbundled binary does not even appear in the Screen Recording list.

On first launch, grant **Screen Recording** in System Settings → Privacy & Security.
A sighted helper should do this once, since the prompt is visual and cannot be answered by keyboard alone.

### Optional: read captions from the browser instead

```sh
./bundle/make-extension.sh
```

It generates `extension/config.js` with a token unique to your machine and prints the four clicks Chrome requires a human to perform. This source needs no Screen Recording permission at all, and gives exact text rather than OCR.

## Use

Everything is on two hotkeys, and every state change is spoken aloud:

| Key | Action |
|---|---|
| `Control+Option+Command+S` | start / stop reading |
| `Control+Option+Command+R` | draw a new subtitle region |
| `Control+Option+Command+Q` | quit |

The menu bar shows `◉` while reading and `◎` while idle.

### First run takes about thirty seconds

Apple's Vision framework compiles its recognition model the first time it sees a given region size.
That is a one-off cost per region size, cached permanently afterwards.
The app pays it on start and says "Preparing the recognizer" first, so it never lands mid-film.

## Setup and tuning

The CLI exists for setup and for checking the pipeline without the menu bar.

```sh
# What can be watched
readsubtitles --list

# Try it without speaking, to check the region is right
readsubtitles --window "Netflix" --dry-run

# Save settings for the menu bar app to use
readsubtitles --window "Netflix" --save
```

`--window` is preferred over `--display` because it follows the video between screens.
The region defaults to the bottom fifth of the window, middle 80%, which is where players put subtitles.
Draw a different one with `Control+Option+Command+R`, or set it directly with `--band x,y,w,h` as fractions.

The drawn region is saved as fractions of the watched window, not as screen coordinates, so it keeps pointing at the subtitles when the window is moved, resized, or taken fullscreen.

Run `readsubtitles --help` for the rest.

## Voices

The on-device voice is the default: free, offline, and no network round trip.
Install a Premium or Enhanced voice in System Settings → Accessibility → Spoken Content for a large step up in quality.
`readsubtitles --list-voices` shows what is installed.

### Using your own ElevenLabs voices

Worth the setup if the listener is going to spend hours with this.
The on-device voices are intelligible; the ElevenLabs ones are pleasant, and that difference compounds over a film.

**1. Get an API key.** Create one at [elevenlabs.io](https://elevenlabs.io) under Profile → API Keys. It needs the **text_to_speech** permission, and nothing else.

**2. Put it in the keychain**, not in a file and not in an environment variable:

```sh
security add-generic-password -U -s readsubtitles-elevenlabs -a "$USER" -w
```

It prompts for the key without echoing it. The app reads it from the keychain at launch, so it never touches the repository or your shell history.

**3. Find the voice IDs you want.** Open any voice in the ElevenLabs voice library and copy its ID from the URL or the voice's detail panel. It is a 20-character string of letters and digits.

**4. Set the main voice**, used for narration and for any line the app cannot attribute to a character:

```sh
readsubtitles --elevenlabs <voice-id> --save
```

**5. Add a cast**, if you want different characters to sound different. Open the settings file from the menu bar, or edit it directly at `~/Library/Application Support/readsubtitles/settings.conf`:

```
elevenlabs_voice = <id>                       # narration, and anything unattributed
elevenlabs_cast  = <id>:female, <id>:smooth male, <id>:evil, <id>:smooth female
```

The labels after each colon are not decoration.
`female`, `woman`, `girl` mark a higher voice; `male`, `man`, `boy` mark a lower one.
The app measures the pitch of whoever is speaking and prefers a voice whose label matches, so a woman on screen is read in a woman's voice.
Labels it does not recognise, like `evil`, still work as ordinary cast members; they simply express no preference about pitch.

Check the whole chain at any time:

```sh
readsubtitles --check-voice
```

It speaks a test line, and quotes the API's own explanation if something is wrong rather than making you guess.

**Cost.** Roughly $0.04 per 1,000 characters, which is about $2.40 for a two-hour film.
Synthesised audio is cached on disk by content, so a repeated line is never paid for twice.
If ElevenLabs fails for any reason, the on-device voice takes over mid-film and says so once, so a network problem never leaves the listener in silence.

## Different voices for different characters

Following a film by ear is hardest when several people talk in quick succession and every line arrives in the same voice.

Four different signals can say who is speaking, and they are not equally trustworthy, so the app uses them in order.
It is on by default, under **Voice → Different voices for characters**.

| | Signal | Looks like | Available on |
|---|---|---|---|
| 1 | Speaker label | `[Tenma] I'll do the surgery.` or `TENMA: ...` | SDH and closed-caption tracks only |
| 2 | Dash alternation | `- Did you hear?` / `- No.` | Any track |
| 3 | The voice in the soundtrack | nothing visible; the app listens | Anything with audio |
| 4 | Italics | narration, a phone, off-screen speech, lyrics | The browser source |

A name in the caption wins, because a human deliberately wrote it and it holds for the whole film.
Dashes come next, because they split a single cue between two people and the soundtrack cannot help there: the audio around one subtitle contains both voices at once.
Then what the app hears.
Italics rank last on purpose - they mean the line is not spoken aloud, not that somebody else is speaking it, so when a character's inner monologue can be identified it is better read in that character's voice than in a shared narrator.

### Listening to the soundtrack

Signals 1 and 2 only work when the subtitle author gave you something.
A translated track usually gives you nothing: **Netflix ships closed captions only in a title's original language**, so an English subtitle on a Japanese show carries no speaker labels at all.

So the app can listen instead.

Turn on **Voice → Tell voices apart by listening**. It takes a two-second window of audio around each subtitle, reduces it to a 256-number fingerprint with a neural speaker-embedding model, and clusters those fingerprints as the film goes on. Lines that sound like the same person get the same voice.

Three things make this far easier here than general speaker diarization:

- **The subtitles already supply the boundaries.** A cue appearing *is* a speaker change. The hardest part of diarization is deciding where one speaker stops, and it is free here.
- **A new speaker must be heard twice** before they are given a voice, which stops a door slam or a music sting from becoming a character.
- **Pitch picks the voice.** The same window is measured for fundamental frequency, so a higher voice is matched to a voice you labelled `female` and a lower one to `male`. Between the two it declines to choose rather than guessing.

It is language-independent: the model encodes timbre, not words, and pitch is a property of the vocal folds. It works on Japanese audio with English subtitles, which is the case it was built for.

**The cost is about half a second of extra delay**, because a subtitle appears as its line *begins*, so the app has to wait for enough of that line to be spoken before it can tell who is speaking. Tune it with `listen_ahead_ms` in the settings file, or turn the feature off to get back to roughly 65 ms.

No audio is recorded. It lives in an eight-second ring buffer in memory and is never written to disk or sent anywhere. It uses the Screen Recording permission you already granted, so macOS does not ask a second time.

### Pinning a character

When the automatic assignment gets someone wrong, say so explicitly:

```
cast_overrides = Tenma=<id>, Eva=<id>
```

Names are matched without regard to capitalisation, and an override beats every other signal. This needs a track that carries speaker labels, since it matches on the name in the caption.

Once a character has been given a voice they keep it for the rest of the film, even if later evidence suggests a better match.
A voice that changes halfway through is more confusing than one that was slightly wrong from the beginning.

## Settings

Everything is reachable from the menu bar: **Source**, **Voice**, **Speed**, the subtitle region, and the settings file itself.
Each change is spoken aloud, because the person using this cannot see the menu that was just clicked.

The file lives at `~/Library/Application Support/readsubtitles/settings.conf`.

| Setting | Default | What it does |
|---|---|---|
| `source` | `screen` | `screen` reads pixels, `browser` reads the extension |
| `window` | - | Window title to follow, preferred over `display` |
| `display` | `0` | Display index, when not following a window |
| `band` | `0.1,0.78,0.8,0.22` | Subtitle region as fractions of the window |
| `fps` | `8` | Capture rate ceiling; a still screen costs nothing |
| `language` | `en-US` | Recognition language |
| `rate` | `1` | Speaking rate. Experienced screen-reader users often run well above 1 |
| `elevenlabs_voice` | - | Main voice id; unset means the on-device voice |
| `elevenlabs_cast` | - | `id:label` pairs for characters |
| `cast_overrides` | - | `Name=id` pairs that beat the automatic assignment |
| `multi_voice` | `true` | Different voices for different characters |
| `diarize` | `false` | Tell voices apart by listening to the soundtrack |
| `listen_ahead_ms` | `900` | How long to wait before deciding who is speaking |

The same settings are available from the CLI with `--save`, which updates only the ones you name and leaves the rest alone.

## How it works

```
screen source:   ScreenCaptureKit ──▶ change detection ──▶ Vision OCR ──┐
                   cropped region      sparse signature    ~35ms/frame  │
                                                                        ├─▶ cue tracking ──▶ speech
browser source:  extension ──▶ loopback HTTP ─────────────────────────--┘    settle+dedupe
                 MutationObserver   exact text
```

Both sources produce the same `(text, when)` pairs, so settling, dedupe, the backlog policy and the voice are shared and know nothing about where the text came from.

| Crate | Does |
|---|---|
| `capture` | ScreenCaptureKit stream, compositor-side cropping, change detection |
| `browser` | Loopback listener that receives captions from the extension |
| `ocr` | Apple Vision text recognition, reading-order sorting |
| `cues` | Settle timing, dedupe, separating dialogue from player chrome. Pure logic, no macOS types, so it is testable on fixtures |
| `diarize` | Soundtrack capture, Kaldi-compatible filterbank features, neural speaker embedding, online clustering, pitch. Pure logic, no macOS types |
| `voice` | On-device and ElevenLabs backends behind one trait, with an on-disk cache |
| `app` | Menu bar shell, hotkeys, region picker, settings, and the CLI |

Spoken output runs roughly 200 ms behind the subtitle appearing, most of which is the deliberate settle window that stops a line being read twice while it fades in.

The research behind the design, including measurements and the constraints that forced this architecture, is in [`docs/research/2026-11-13-spoken-subtitles-macos.md`](docs/research/2026-11-13-spoken-subtitles-macos.md).

## Tests

```sh
cargo test --workspace
```

The cue logic carries the behavioural tests, because that is where the judgement calls live: when a line has settled, whether two reads are the same subtitle, and which text on screen is dialogue rather than player furniture.

## Status

Working end to end on Netflix and YouTube: menu bar app, hotkeys, region picker, both caption sources, on-device and ElevenLabs voices, multi-voice separation, and speaker diarization with pitch-matched voice selection.

Known rough edges, stated plainly:

- **Fast dialogue gets cut short.** When subtitles arrive faster than they can be spoken, the app stays with the picture and interrupts the previous line rather than falling behind. In dense scenes that is noticeable. Raising `rate` helps more than anything else.
- **Diarization costs latency.** Roughly 500 ms at the default `listen_ahead_ms`, against 65 ms with it off.
- **Clusters are not named.** The app knows two speakers are different people; it has no idea which is which, unless the track carries speaker labels.
- Per-title region presets are not built.
- No real-world testing yet with a user who has only the hotkeys and no screen. That is the only test that ultimately matters.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The constraint that decides most questions: the person using this cannot see it, so every state change is spoken and silence is the worst failure mode.

Security policy, and a plain account of what this software can see and hear, is in [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE).

---

Created by **Mishka** — [paracorp.io](https://paracorp.io)

Built for two people who wanted to watch films in a language they do not read. *Spoken subtitles* have been an accessibility modality since Dutch public broadcasting shipped them in 2002; this is the consumer-side version.
