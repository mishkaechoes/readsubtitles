# Contributing

This started as a tool for two specific people: a pair of blind grandparents who wanted to watch foreign-language film and television.
That origin still decides most questions here.
If you are weighing a change and the guidance below does not settle it, ask what it does for someone who cannot see the screen.

## The constraint that shapes everything

**The person using this cannot see it.** There is no "look at the menu bar to check", no error dialog they can read, no visual cue that something went wrong.

So:

- Every state change is spoken aloud. Started, stopped, region lost, permission missing, voice failed over.
- A failure must say what happened and what to do about it. Silence is the worst possible failure mode, because it is indistinguishable from a film with no dialogue.
- Nothing may require a mouse during normal use. Setup can need a sighted helper; watching a film cannot.

## Getting set up

```sh
./bundle/make-cert.sh     # once: a stable signing certificate
./bundle/make-app.sh      # build and bundle
cargo test --workspace
```

macOS 13 or later, Rust 1.85 or later. `bundle/make-cert.sh` is worth the one-off minute: without a stable certificate, macOS re-prompts for Screen Recording after every single rebuild, because the permission is keyed to the code signature.

## Before you open a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same three. Warnings are errors here, including in tests.

## Where tests belong

The pure crates carry the behavioural tests, because that is where the judgement calls live.

- `cues` decides when a line has settled, whether two readings are the same subtitle, and which text on screen is dialogue rather than player furniture.
- `diarize` decides whether two seconds of audio are the same person as the last two seconds, and whether a voice is higher or lower.
- `voice` decides which character keeps which voice.

None of them depend on macOS types, so all of them run on fixtures in milliseconds. The capture and OCR crates are thin wrappers over Apple frameworks; keep logic out of them so it stays testable.

**Name a test after the behaviour, not the function.** `a_womans_label_is_not_read_as_a_mans` tells a future reader what broke. `test_voices_for_register` does not.

**A bug fix starts with a failing test that reproduces it end to end.** Several defects in this codebase looked correct in isolation and only showed up in the whole pipeline: a cue that lost its line breaks before anything could split it, a diarized speaker computed perfectly and then discarded one function later. A unit test that passes either way would have caught neither.

## Comments

Explain **why**, never what. The code already says what.

The comments worth writing here are the ones that record a decision someone would otherwise undo:

```rust
// "female" contains "male". Testing for a man first would classify every
// woman's voice as a man's and invert the whole feature, which would look
// like it was working and sound exactly wrong.
```

That earns its place. `// loop over the voices` does not.

## Things that will be asked about in review

**Latency is a feature.** Spoken output runs a few hundred milliseconds behind the subtitle appearing. Anything that adds to that needs to buy something real, and the cost belongs in the pull request description. Synthesis once blocked the cue loop and pushed lag past eleven seconds; it is now on a worker thread and runs at sixty-five milliseconds.

**A character's voice may not change mid-film.** Once a speaker has been given a voice, they keep it, even if later evidence suggests a better match. A voice that switches halfway through is more confusing than one that was slightly wrong from the start.

**Guessing is worse than not choosing.** When the evidence is weak, fall back to the main voice rather than picking. Pitch classification deliberately leaves a band in the middle where it declines to decide.

**No secrets in the repo.** The API key belongs in the keychain and the bridge token in Application Support. `extension/config.js` is generated per machine and git-ignored.

## Style

- Plain dash, never an em dash.
- One sentence per line in Markdown, so diffs stay readable.
- Commit messages say why the change was needed, not which files moved.
