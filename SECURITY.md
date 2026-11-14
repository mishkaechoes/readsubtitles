# Security

## Reporting

Report a vulnerability privately through [GitHub's security advisory form](https://github.com/mishkaechoes/readsubtitles/security/advisories/new), not a public issue.

## What this software touches

Worth stating plainly, because the permissions it asks for are broad and a reader deserves to know what they are for.

**It can see your screen.** The screen source captures a rectangle you choose, at a few frames a second. Frames are recognised on-device by Apple's Vision framework, kept only long enough to compare one against the next, and never written to disk or sent anywhere. The `--save-frames` flag is the single exception, and only when you pass it.

**It can hear your soundtrack.** Speaker diarization reads the system audio to tell voices apart. The audio lives in an eight-second ring buffer in memory, is reduced to a 256-number voice fingerprint, and is never recorded or transmitted. It rides the same Screen Recording permission as video, so macOS does not ask twice. Leave **Voice → Tell voices apart by listening** off and no audio is captured at all.

**It can send subtitle text to ElevenLabs.** Only when you opt in by configuring a voice, and only the subtitle text itself. The on-device voice is the default and never touches the network.

A caution that follows from the first two: this reads whatever is in the region you point it at. If you start it and then switch to your email, it will read your email aloud. That is the nature of a tool that works on anything, and it is why the region is yours to draw.

## Where secrets live

| Secret | Location | In the repo? |
|---|---|---|
| ElevenLabs API key | macOS login keychain, service `readsubtitles-elevenlabs` | Never |
| Browser bridge token | `~/Library/Application Support/readsubtitles/bridge-token`, mode 600 | Never |
| Extension copy of the token | `extension/config.js`, generated per machine | Git-ignored |

The bridge listens on loopback only and requires the token, so a web page cannot reach it.

If you have ever pasted an API key into a terminal, a chat window, or a screenshot, rotate it. Treat that as the default assumption rather than the exception.
