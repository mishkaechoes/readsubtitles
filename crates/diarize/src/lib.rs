//! Working out who is speaking from the film's own soundtrack.
//!
//! This exists because the subtitle track usually cannot say. Speaker labels
//! live on closed-caption tracks, and a closed-caption track only exists in a
//! title's original language, so a film watched in translation - the case this
//! project is built for - has no labels in either track.
//!
//! The voice itself is always there, in every language, and does not depend on
//! what a subtitler chose to type.
//!
//! The pipeline is deliberately small because the subtitles do the hard part:
//! general diarization has to find speaker boundaries in continuous audio,
//! whereas here a cue appearing *is* the boundary. The only question left is
//! "whose voice is in this window", which is a much easier one.

pub mod cluster;
pub mod embed;
pub mod fbank;
pub mod pitch;

pub use cluster::{Assignment, Clusterer, Config, has_voice, normalise};
pub use embed::{Embedder, model_path};
pub use fbank::Fbank;
pub use pitch::{Register, median_pitch};
