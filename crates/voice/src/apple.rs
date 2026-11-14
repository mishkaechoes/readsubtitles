//! The on-device voice, via `AVSpeechSynthesizer`.
//!
//! This is the default backend. It is free, works offline, and begins speaking
//! with no network round trip, which removes both the per-film cost and the
//! "silent because the Wi-Fi dropped" failure mode from the critical path.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_avf_audio::{
    AVSpeechBoundary, AVSpeechSynthesisVoice, AVSpeechSynthesisVoiceQuality, AVSpeechSynthesizer,
    AVSpeechUtterance,
};
use objc2_foundation::NSString;

use crate::{Speaker, VoiceError};

/// A voice installed on this machine.
#[derive(Debug, Clone)]
pub struct VoiceInfo {
    pub identifier: String,
    pub name: String,
    pub language: String,
    /// Default, Enhanced or Premium. The higher tiers are a large step up in
    /// naturalness and are a free download in System Settings, so it is worth
    /// telling the user when only Default voices are installed.
    pub quality: &'static str,
}

/// Every installed voice, best quality first.
#[must_use]
pub fn available_voices() -> Vec<VoiceInfo> {
    let mut out: Vec<_> = unsafe { AVSpeechSynthesisVoice::speechVoices() }
        .iter()
        .map(|v| unsafe {
            VoiceInfo {
                identifier: v.identifier().to_string(),
                name: v.name().to_string(),
                language: v.language().to_string(),
                quality: match v.quality() {
                    AVSpeechSynthesisVoiceQuality::Premium => "Premium",
                    AVSpeechSynthesisVoiceQuality::Enhanced => "Enhanced",
                    _ => "Default",
                },
            }
        })
        .collect();
    out.sort_by_key(|v| match v.quality {
        "Premium" => 0,
        "Enhanced" => 1,
        _ => 2,
    });
    out
}

pub struct AppleVoice {
    synth: Retained<AVSpeechSynthesizer>,
    voice: Option<Retained<AVSpeechSynthesisVoice>>,
    /// Voices resolved by identifier, kept so switching between characters does
    /// not re-resolve the same handful on every line.
    resolved: std::collections::HashMap<String, Option<Retained<AVSpeechSynthesisVoice>>>,
    language: String,
    name: String,
    rate: f32,
}

impl AppleVoice {
    /// Build a speaker using the best installed voice for `language`.
    ///
    /// Prefers Premium, then Enhanced, then Default, because the quality gap is
    /// wide and the better voices are already on disk on most machines.
    pub fn best_for(language: &str) -> Result<Self, VoiceError> {
        let best = available_voices()
            .into_iter()
            .find(|v| v.language.eq_ignore_ascii_case(language))
            .or_else(|| {
                let prefix = language
                    .split('-')
                    .next()
                    .unwrap_or(language)
                    .to_lowercase();
                available_voices()
                    .into_iter()
                    .find(|v| v.language.to_lowercase().starts_with(&prefix))
            })
            .ok_or_else(|| VoiceError::NoSuchVoice(language.to_string()))?;
        Self::with_identifier(&best.identifier)
    }

    pub fn with_identifier(identifier: &str) -> Result<Self, VoiceError> {
        let voice =
            unsafe { AVSpeechSynthesisVoice::voiceWithIdentifier(&NSString::from_str(identifier)) }
                .ok_or_else(|| VoiceError::NoSuchVoice(identifier.to_string()))?;
        let name = unsafe { voice.name() }.to_string();
        let language = unsafe { voice.language() }.to_string();
        Ok(Self {
            synth: unsafe { AVSpeechSynthesizer::new() },
            voice: Some(voice),
            resolved: std::collections::HashMap::new(),
            language,
            name,
            rate: 1.0,
        })
    }
}

impl AppleVoice {
    /// Look up a voice by identifier, remembering the answer including a miss.
    fn resolve(&mut self, identifier: &str) -> Option<Retained<AVSpeechSynthesisVoice>> {
        if let Some(found) = self.resolved.get(identifier) {
            return found.clone();
        }
        let found =
            unsafe { AVSpeechSynthesisVoice::voiceWithIdentifier(&NSString::from_str(identifier)) };
        self.resolved.insert(identifier.to_string(), found.clone());
        found
    }
}

/// Voices worth reading dialogue in, best first.
///
/// macOS ships a pile of novelty voices (Bells, Boing, Bubbles, Zarvox and
/// friends) that are unusable for speech, and several legacy ones that are
/// merely unpleasant. Handing a character one of those would make the film
/// worse, not better, so the legacy synthesiser family is excluded wholesale
/// and the modern families are preferred in quality order.
#[must_use]
pub fn dialogue_voices(language: &str) -> Vec<VoiceInfo> {
    let prefix = language
        .split('-')
        .next()
        .unwrap_or(language)
        .to_lowercase();
    available_voices()
        .into_iter()
        .filter(|v| v.language.to_lowercase().starts_with(&prefix))
        // `com.apple.speech.synthesis.voice.*` is the pre-2010 set: the
        // novelties live there, and so do Fred, Albert and Ralph, which are
        // intelligible but jarring next to a modern voice.
        .filter(|v| {
            !v.identifier
                .starts_with("com.apple.speech.synthesis.voice.")
        })
        .collect()
}

impl Speaker for AppleVoice {
    fn speak_as(&mut self, text: &str, voice: Option<&str>) -> Result<(), VoiceError> {
        let utterance = unsafe {
            AVSpeechUtterance::initWithString(AVSpeechUtterance::alloc(), &NSString::from_str(text))
        };
        // An explicitly requested voice wins; otherwise the selected one.
        // Looked-up voices are cached because resolving an identifier is not
        // free and the same handful recur for the whole film.
        let chosen = match voice {
            Some(id) => self.resolve(id).or_else(|| self.voice.clone()),
            None => self.voice.clone(),
        };
        if let Some(v) = &chosen {
            unsafe { utterance.setVoice(Some(v)) };
        }
        // `AVSpeechUtteranceDefaultSpeechRate` is 0.5, and the usable range is
        // roughly 0.0 to 1.0, so a relative rate of 1.0 maps to 0.5 here.
        unsafe { utterance.setRate((self.rate * 0.5).clamp(0.0, 1.0)) };
        unsafe { self.synth.speakUtterance(&utterance) };
        Ok(())
    }

    fn stop(&mut self) {
        unsafe {
            self.synth
                .stopSpeakingAtBoundary(AVSpeechBoundary::Immediate)
        };
    }

    fn is_speaking(&self) -> bool {
        unsafe { self.synth.isSpeaking() }
    }

    fn set_rate(&mut self, rate: f32) {
        self.rate = rate;
    }

    fn character_voices(&self) -> Vec<String> {
        dialogue_voices(&self.language)
            .into_iter()
            .map(|v| v.identifier)
            .collect()
    }

    fn selected_voice(&self) -> Option<String> {
        self.voice
            .as_ref()
            .map(|v| unsafe { v.identifier() }.to_string())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_has_at_least_one_voice() {
        assert!(!available_voices().is_empty());
    }

    #[test]
    fn best_quality_voices_sort_first() {
        let v = available_voices();
        let rank = |q: &str| match q {
            "Premium" => 0,
            "Enhanced" => 1,
            _ => 2,
        };
        assert!(
            v.windows(2)
                .all(|w| rank(w[0].quality) <= rank(w[1].quality))
        );
    }

    #[test]
    fn an_english_voice_can_be_selected() {
        let v = AppleVoice::best_for("en-US").expect("en-US voice should exist");
        assert!(!v.name().is_empty());
    }

    #[test]
    fn an_unknown_language_is_reported_rather_than_panicking() {
        assert!(matches!(
            AppleVoice::best_for("zz-ZZ"),
            Err(VoiceError::NoSuchVoice(_))
        ));
    }
}
