//! The ElevenLabs backend: an opt-in upgrade over the on-device voice.
//!
//! Trade-offs this makes explicit, because they are the reason it is not the
//! default: it costs roughly $0.04 per thousand characters, it needs the
//! network, and it sends the subtitle text off the machine. The on-device voice
//! has none of those properties. Synthesised audio is cached on disk so a line
//! is never paid for twice.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use rodio::{OutputStream, OutputStreamBuilder, Sink, buffer::SamplesBuffer};

use crate::cache::AudioCache;
use crate::{Speaker, VoiceError};

/// Keychain service name holding the API key.
pub const KEYCHAIN_SERVICE: &str = "readsubtitles-elevenlabs";

/// Flash is half the price of the multilingual models and the lowest latency
/// one on offer, which is what matters when speech has to track the picture.
pub const DEFAULT_MODEL: &str = "eleven_flash_v2_5";

/// Raw signed 16-bit mono. Asking for PCM rather than MP3 avoids a decode step
/// and the known trouble feeding a non-seekable MP3 stream into rodio.
const SAMPLE_RATE: u32 = 22_050;
const OUTPUT_FORMAT: &str = "pcm_22050";

/// Read the API key from the login keychain, falling back to the environment.
///
/// The key never belongs in the repo or in a config file next to it.
pub fn api_key_from_keychain() -> Option<String> {
    if let Ok(k) = std::env::var("ELEVENLABS_API_KEY")
        && !k.is_empty()
    {
        return Some(k);
    }
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let key = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!key.is_empty()).then_some(key)
}

pub struct ElevenLabsVoice {
    api_key: String,
    voice_id: String,
    model: String,
    name: String,
    rate: f32,
    cache: AudioCache,
    /// Held so the audio device stays open for the life of the speaker.
    _stream: OutputStream,
    sink: Arc<Sink>,
    /// Work queued for the synthesis thread.
    ///
    /// Synthesis is a network round trip of 200ms or more. Doing it inline
    /// would block the caller, which is the loop that watches for new
    /// subtitles, so a busy scene would push the spoken line further and
    /// further behind the picture: measured at over ten seconds before this
    /// was moved off the caller's thread.
    work: Sender<Job>,
    /// Bumped by `stop`. The worker discards anything older, so audio from a
    /// cue that has already been superseded is never played.
    generation: Arc<AtomicU64>,
    /// Jobs accepted but not yet played, so `is_speaking` can account for
    /// synthesis that has not produced audio yet.
    outstanding: Arc<AtomicU64>,
    /// Characters sent to the API this session, for budget reporting.
    billed_chars: Arc<AtomicU64>,
    /// Extra voice ids available for named characters.
    cast: Vec<String>,
}

impl ElevenLabsVoice {
    pub fn new(voice_id: &str, model: Option<&str>) -> Result<Self, VoiceError> {
        let api_key = api_key_from_keychain().ok_or_else(|| {
            VoiceError::Remote(format!(
                "no API key; add one with: security add-generic-password -U -s {KEYCHAIN_SERVICE} -a $USER -w"
            ))
        })?;
        let stream = OutputStreamBuilder::open_default_stream()
            .map_err(|e| VoiceError::Audio(e.to_string()))?;
        let sink = Arc::new(Sink::connect_new(stream.mixer()));

        let generation = Arc::new(AtomicU64::new(0));
        let outstanding = Arc::new(AtomicU64::new(0));
        let billed_chars = Arc::new(AtomicU64::new(0));

        let (work, jobs) = channel::<Job>();
        let worker = Worker {
            api_key: api_key.clone(),
            model: model.unwrap_or(DEFAULT_MODEL).to_string(),
            cache: AudioCache::new("readsubtitles/elevenlabs"),
            sink: sink.clone(),
            generation: generation.clone(),
            outstanding: outstanding.clone(),
            billed_chars: billed_chars.clone(),
        };
        std::thread::spawn(move || worker.run(&jobs));

        Ok(Self {
            api_key,
            voice_id: voice_id.to_string(),
            model: model.unwrap_or(DEFAULT_MODEL).to_string(),
            name: format!("ElevenLabs {voice_id}"),
            rate: 1.0,
            cache: AudioCache::new("readsubtitles/elevenlabs"),
            _stream: stream,
            sink,
            work,
            generation,
            outstanding,
            billed_chars,
            cast: Vec::new(),
        })
    }

    /// Characters actually sent for synthesis, excluding cache hits.
    #[must_use]
    pub fn billed_chars(&self) -> u64 {
        self.billed_chars.load(Ordering::Relaxed)
    }

    /// Verify the key and voice without spending much: synthesises one word.
    /// Extra voices this backend may read characters in.
    ///
    /// ElevenLabs bills per character regardless of how many voices are used,
    /// so a cast costs no more than a single voice. Each distinct voice does
    /// get its own cache entries, so a re-watch is slightly less free.
    pub fn set_cast(&mut self, cast: Vec<String>) {
        // The main voice stays reserved for unattributed dialogue, so drop it
        // from the character pool if it also appears there.
        self.cast = cast.into_iter().filter(|v| *v != self.voice_id).collect();
    }

    pub fn check(&self) -> Result<(), VoiceError> {
        synthesize(
            &self.api_key,
            &self.cache,
            &self.model,
            "Ready.",
            &self.voice_id,
        )
        .map(|_| ())
    }
}

/// Minimal JSON for the request body.
///
/// Only two fields are needed, so a serialiser dependency would be more weight
/// than the problem deserves. Escaping covers what subtitle text can contain.
/// Fetch the audio for one line, from the cache when possible.
///
/// A free function rather than a method so the synthesis worker can call it
/// without holding a reference to the speaker it belongs to.
fn synthesize(
    api_key: &str,
    cache: &AudioCache,
    model: &str,
    text: &str,
    voice_id: &str,
) -> Result<Vec<u8>, VoiceError> {
    // The cache is keyed on the voice as well as the text, so the same line
    // read by two characters is two cache entries rather than one wrong one.
    if let Some(pcm) = cache.get(text, voice_id, model) {
        return Ok(pcm);
    }

    let url = format!(
        "https://api.elevenlabs.io/v1/text-to-speech/{voice_id}/stream?output_format={OUTPUT_FORMAT}"
    );
    let body = serde_body(text, model);

    // Handle the status ourselves rather than letting ureq turn it into an
    // opaque error: on a refusal the body carries the only text that says
    // what is actually wrong, and "http status: 401" alone sends people
    // hunting for a bad key when the key is fine and only its scope is not.
    let response = ureq::post(&url)
        .config()
        .http_status_as_error(false)
        .build()
        .header("xi-api-key", api_key)
        .header("content-type", "application/json")
        .send(&body)
        .map_err(|e| VoiceError::Remote(e.to_string()))?;

    let status = response.status().as_u16();
    let mut pcm = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut pcm)
        .map_err(|e| VoiceError::Remote(e.to_string()))?;

    if status != 200 {
        let detail = String::from_utf8_lossy(&pcm);
        return Err(VoiceError::Remote(format!(
            "HTTP {status}: {}",
            api_message(&detail)
        )));
    }

    if pcm.is_empty() {
        return Err(VoiceError::Remote("empty audio response".into()));
    }

    let _ = cache.put(text, voice_id, model, &pcm);
    Ok(pcm)
}

/// One line waiting to be synthesised.
struct Job {
    text: String,
    voice_id: String,
    rate: f32,
    generation: u64,
}

/// Turns queued lines into audio, off the caller's thread.
struct Worker {
    api_key: String,
    model: String,
    cache: AudioCache,
    sink: Arc<Sink>,
    generation: Arc<AtomicU64>,
    outstanding: Arc<AtomicU64>,
    billed_chars: Arc<AtomicU64>,
}

impl Worker {
    fn run(&self, jobs: &Receiver<Job>) {
        for job in jobs {
            // Checked before and after synthesis: a stop can land while the
            // request is in flight, and playing the result then would speak a
            // line for a subtitle that has already gone.
            if job.generation != self.generation.load(Ordering::Relaxed) {
                self.finish();
                continue;
            }

            match synthesize(
                &self.api_key,
                &self.cache,
                &self.model,
                &job.text,
                &job.voice_id,
            ) {
                Ok(pcm) => {
                    self.billed_chars
                        .fetch_add(job.text.chars().count() as u64, Ordering::Relaxed);
                    if job.generation == self.generation.load(Ordering::Relaxed) {
                        // Rate is applied by resampling: a faster read is the
                        // same samples presented at a higher rate.
                        let rate = (SAMPLE_RATE as f32 * job.rate).round().max(8000.0) as u32;
                        self.sink
                            .append(SamplesBuffer::new(1, rate, pcm_to_samples(&pcm)));
                        self.sink.play();
                    }
                }
                Err(e) => eprintln!("speech failed: {e}"),
            }
            self.finish();
        }
    }

    /// Mark one job done, whether it played, was retired, or failed.
    ///
    /// Every accepted job reaches here exactly once, which is what keeps the
    /// count honest without any saturating arithmetic.
    fn finish(&self) {
        self.outstanding.fetch_sub(1, Ordering::Relaxed);
    }
}

fn serde_body(text: &str, model: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if (c as u32) < 0x20 => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    format!(r#"{{"text":"{escaped}","model_id":"{model}"}}"#)
}

/// Signed 16-bit little-endian bytes to the float samples rodio mixes.
/// Pull the human-readable sentence out of an ElevenLabs error body.
///
/// Their errors nest it as `{"detail":{"message":"..."}}`. Rather than take on
/// a JSON parser for one field, find the key and read the string after it, and
/// fall back to the raw body whenever that does not work, so this can only ever
/// improve the message and never hide one.
fn api_message(body: &str) -> String {
    const KEY: &str = "\"message\":\"";
    let Some(start) = body.find(KEY).map(|i| i + KEY.len()) else {
        return body.trim().to_string();
    };
    let rest = &body[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return out,
            // Keep an escaped quote inside the sentence instead of ending early.
            '\\' => out.push(chars.next().unwrap_or('\\')),
            _ => out.push(c),
        }
    }
    body.trim().to_string()
}

fn pcm_to_samples(pcm: &[u8]) -> Vec<f32> {
    pcm.chunks_exact(2)
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / f32::from(i16::MAX))
        .collect()
}

impl Speaker for ElevenLabsVoice {
    /// Queue a line and return at once.
    ///
    /// Returning before the audio exists is deliberate: the caller is the loop
    /// watching for the next subtitle, and it must not be held up by a network
    /// round trip.
    fn speak_as(&mut self, text: &str, voice: Option<&str>) -> Result<(), VoiceError> {
        self.outstanding.fetch_add(1, Ordering::Relaxed);
        self.work
            .send(Job {
                text: text.to_string(),
                voice_id: voice.unwrap_or(&self.voice_id).to_string(),
                rate: self.rate,
                generation: self.generation.load(Ordering::Relaxed),
            })
            .map_err(|_| VoiceError::Audio("synthesis worker stopped".into()))
    }

    fn stop(&mut self) {
        // Anything already queued belongs to a subtitle that is no longer on
        // screen, so retire this generation rather than letting it arrive late.
        self.generation.fetch_add(1, Ordering::Relaxed);
        // `outstanding` is deliberately left alone: the worker still drains and
        // decrements every retired job, so zeroing it here would underflow.
        self.sink.clear();
        self.sink.play();
    }

    fn is_speaking(&self) -> bool {
        !self.sink.empty() || self.outstanding.load(Ordering::Relaxed) > 0
    }

    fn set_rate(&mut self, rate: f32) {
        self.rate = rate.clamp(0.5, 3.0);
    }

    fn character_voices(&self) -> Vec<String> {
        self.cast.clone()
    }

    fn selected_voice(&self) -> Option<String> {
        Some(self.voice_id.clone())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_permission_error_is_reduced_to_its_sentence() {
        let body = r#"{"detail":{"type":"authentication_error","code":"unauthorized","message":"The API key you used is missing the permission text_to_speech to execute this operation.","status":"missing_permissions"}}"#;
        assert_eq!(
            api_message(body),
            "The API key you used is missing the permission text_to_speech to execute this operation."
        );
    }

    #[test]
    fn a_body_with_no_message_field_is_passed_through_whole() {
        assert_eq!(api_message("  Bad Gateway  "), "Bad Gateway");
    }

    #[test]
    fn an_escaped_quote_does_not_truncate_the_sentence() {
        let body = r#"{"message":"the \"voice\" was not found"}"#;
        assert_eq!(api_message(body), r#"the "voice" was not found"#);
    }

    #[test]
    fn an_unterminated_message_falls_back_to_the_raw_body() {
        let body = r#"{"message":"truncated"#;
        assert_eq!(api_message(body), body);
    }

    #[test]
    fn body_is_well_formed_json() {
        assert_eq!(
            serde_body("Hi there.", "eleven_flash_v2_5"),
            r#"{"text":"Hi there.","model_id":"eleven_flash_v2_5"}"#
        );
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        let b = serde_body(r#"He said "go\now"."#, "m");
        assert!(b.contains(r#"He said \"go\\now\"."#), "got {b}");
    }

    #[test]
    fn newlines_do_not_break_the_body() {
        assert!(serde_body("two\nlines", "m").contains(r"two\nlines"));
    }

    #[test]
    fn control_characters_are_escaped() {
        assert!(serde_body("bell\u{7}", "m").contains(r"\u0007"));
    }

    #[test]
    fn pcm_decodes_to_the_expected_sample_count_and_range() {
        // i16 min, 0, i16 max, little-endian.
        let pcm = [0x00, 0x80, 0x00, 0x00, 0xff, 0x7f];
        let s = pcm_to_samples(&pcm);
        assert_eq!(s.len(), 3);
        assert!(s[0] < -0.99 && s[1] == 0.0 && s[2] > 0.99);
    }

    #[test]
    fn a_trailing_odd_byte_is_ignored_rather_than_panicking() {
        assert_eq!(pcm_to_samples(&[0, 0, 1]).len(), 1);
    }
}
