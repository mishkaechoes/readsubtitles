//! Saying a line out loud.
//!
//! Two backends sit behind one [`Speaker`] trait. The on-device Apple voice is
//! the default: it costs nothing, needs no network, and starts speaking
//! immediately, which matters more over a two-hour film than the last few
//! percent of naturalness. ElevenLabs is an opt-in upgrade.

use std::fmt;

pub mod apple;
pub mod cache;
pub mod cast;
pub mod elevenlabs;

pub use apple::AppleVoice;
pub use cache::AudioCache;
pub use cast::Cast;
pub use elevenlabs::{ElevenLabsVoice, api_key_from_keychain};

/// A voice that can be asked to say something.
///
/// Implementations are expected to be interruptible: when the next subtitle
/// arrives and the previous line is still being read, the caller decides
/// whether to let it finish or cut it off, and staying in sync with the picture
/// usually means cutting it off.
pub trait Speaker {
    /// Begin speaking `text`. Returns as soon as speech has been queued.
    fn speak(&mut self, text: &str) -> Result<(), VoiceError> {
        self.speak_as(text, None)
    }

    /// Speak in a particular voice, for reading different characters
    /// differently.
    ///
    /// `voice` is whatever identifies a voice to this backend, so a caller that
    /// does not know the backend passes `None` and gets the selected voice.
    /// Backends that cannot vary their voice ignore it, which is why this has a
    /// default rather than forcing every backend to pretend.
    fn speak_as(&mut self, text: &str, voice: Option<&str>) -> Result<(), VoiceError>;

    /// Voices this backend can read characters in, best first. Empty when it
    /// has only one.
    fn character_voices(&self) -> Vec<String> {
        Vec::new()
    }

    /// The voice currently selected, in the form `character_voices` uses.
    ///
    /// Used to keep the user's chosen voice for narration and unattributed
    /// dialogue, rather than handing it to the first character who speaks.
    fn selected_voice(&self) -> Option<String> {
        None
    }

    /// Stop immediately and discard anything pending.
    fn stop(&mut self);

    /// Whether audio is still playing.
    fn is_speaking(&self) -> bool;

    /// Words per minute, relative: 1.0 is the backend's normal pace.
    ///
    /// Subtitles can arrive faster than a natural reading, and experienced
    /// screen-reader users commonly run well above 1.0.
    fn set_rate(&mut self, rate: f32);

    fn name(&self) -> &str;
}

#[derive(Debug)]
pub enum VoiceError {
    /// The requested voice identifier does not exist on this machine.
    NoSuchVoice(String),
    /// Network or API failure from a remote backend.
    Remote(String),
    /// Audio device problem.
    Audio(String),
}

impl fmt::Display for VoiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchVoice(v) => write!(f, "no such voice: {v}"),
            Self::Remote(e) => write!(f, "voice service error: {e}"),
            Self::Audio(e) => write!(f, "audio error: {e}"),
        }
    }
}

impl std::error::Error for VoiceError {}
