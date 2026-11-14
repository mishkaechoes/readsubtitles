//! Which voice reads which character.
//!
//! The one property that matters more than the choice itself is *stability*. A
//! character whose voice changes between scenes is worse than no separation at
//! all: the listener has to re-learn who is who every few minutes, and will
//! reasonably conclude two characters are one. So an assignment, once made, is
//! never revised for the life of the session.

use std::collections::HashMap;

/// Hands out voices to speakers and remembers what it gave them.
#[derive(Debug, Clone)]
pub struct Cast {
    pool: Vec<String>,
    assigned: HashMap<String, String>,
    /// Index of the next unused voice. Assignment is in order rather than
    /// random so a given film sounds the same each time it is watched.
    next: usize,
    /// The voice for lines carrying no speaker signal at all, which is most of
    /// them on an ordinary subtitle track. Fixed, and never handed to a named
    /// character, so unattributed dialogue stays in one familiar voice.
    narrator: Option<String>,
}

impl Cast {
    /// `pool` is the voices available for characters, best first.
    #[must_use]
    pub fn new(pool: Vec<String>) -> Self {
        Self {
            pool,
            assigned: HashMap::new(),
            next: 0,
            narrator: None,
        }
    }

    /// Reserve a voice for unattributed dialogue.
    ///
    /// Called with whatever the user chose as their main voice, so the bulk of
    /// the film still sounds like the voice they picked and only identifiable
    /// characters sound different.
    pub fn set_default_voice(&mut self, identifier: impl Into<String>) {
        let identifier = identifier.into();
        // Also take it out of the character pool, or the first named character
        // would sound identical to unattributed dialogue.
        self.pool.retain(|v| *v != identifier);
        self.assigned.insert("default".into(), identifier.clone());
        self.narrator = Some(identifier);
    }

    /// The voice for a speaker key, assigning one on first sight.
    ///
    /// Returns `None` when there are no voices to give out, which the caller
    /// treats as "use whatever is already selected".
    pub fn voice_for(&mut self, key: &str) -> Option<&str> {
        self.voice_for_preferring(key, &[])
    }

    /// As [`Self::voice_for`], but trying the listed voices first.
    ///
    /// `preferred` is how the caller says "this speaker sounds like a woman",
    /// having measured that from the audio. Reading a woman in a man's voice is
    /// worse than reading everyone the same way, because the listener draws a
    /// conclusion from it and the conclusion is wrong.
    ///
    /// It is a preference and not a requirement: when every suitable voice is
    /// already taken, an unsuitable one still beats having no separation, and
    /// an existing assignment is never revised, because a character changing
    /// voice mid-film is the one thing worse than a mismatched voice.
    pub fn voice_for_preferring(&mut self, key: &str, preferred: &[String]) -> Option<&str> {
        if !self.assigned.contains_key(key) {
            let chosen = self
                .take_preferred(preferred)
                .or_else(|| self.take_next())?;
            self.assigned.insert(key.to_string(), chosen);
        }
        self.assigned.get(key).map(String::as_str)
    }

    /// Take an unused voice from `preferred`, if any is still free.
    fn take_preferred(&mut self, preferred: &[String]) -> Option<String> {
        let taken: Vec<&String> = self.assigned.values().collect();
        let chosen = preferred
            .iter()
            .find(|v| self.pool.contains(v) && !taken.contains(v))?
            .clone();
        // Advance past it so the ordinary round robin does not hand the same
        // voice out again as if it were still free.
        if let Some(i) = self.pool.iter().position(|v| *v == chosen) {
            self.pool.remove(i);
        }
        Some(chosen)
    }

    fn take_next(&mut self) -> Option<String> {
        if self.pool.is_empty() {
            return None;
        }
        // Wrapping round is better than running out: with more characters than
        // voices, two of them share, which is still more informative than
        // everybody sharing one.
        let voice = self.pool[self.next % self.pool.len()].clone();
        self.next += 1;
        Some(voice)
    }

    /// How many distinct speakers have been given a voice.
    #[must_use]
    pub fn assigned_count(&self) -> usize {
        self.assigned.len()
    }

    #[must_use]
    pub fn pool_size(&self) -> usize {
        self.pool.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cast() -> Cast {
        Cast::new(vec!["alpha".into(), "beta".into(), "gamma".into()])
    }

    #[test]
    fn a_speaker_keeps_the_same_voice_for_the_whole_film() {
        let mut c = cast();
        let first = c.voice_for("name:tenma").unwrap().to_string();
        // Dozens of other characters speak in between.
        for i in 0..50 {
            c.voice_for(&format!("name:extra{i}"));
        }
        assert_eq!(c.voice_for("name:tenma").unwrap(), first);
    }

    #[test]
    fn different_speakers_get_different_voices_while_voices_remain() {
        let mut c = cast();
        let a = c.voice_for("name:tenma").unwrap().to_string();
        let b = c.voice_for("name:eva").unwrap().to_string();
        let d = c.voice_for("name:gunther").unwrap().to_string();
        assert_ne!(a, b);
        assert_ne!(b, d);
        assert_ne!(a, d);
    }

    #[test]
    fn more_characters_than_voices_share_rather_than_falling_silent() {
        let mut c = Cast::new(vec!["only".into()]);
        assert_eq!(c.voice_for("name:a").unwrap(), "only");
        assert_eq!(c.voice_for("name:b").unwrap(), "only");
    }

    #[test]
    fn an_empty_pool_yields_nothing_rather_than_panicking() {
        let mut c = Cast::new(Vec::new());
        assert_eq!(c.voice_for("name:a"), None);
    }

    #[test]
    fn unattributed_dialogue_keeps_the_voice_the_user_chose() {
        let mut c = cast();
        c.set_default_voice("beta");
        assert_eq!(c.voice_for("default").unwrap(), "beta");
    }

    #[test]
    fn the_chosen_voice_is_not_also_given_to_a_character() {
        // Otherwise the first named character is indistinguishable from the
        // narration, which defeats the point.
        let mut c = cast();
        c.set_default_voice("alpha");
        for key in ["name:a", "name:b", "name:c"] {
            assert_ne!(c.voice_for(key).unwrap(), "alpha");
        }
    }

    #[test]
    fn a_preferred_voice_is_used_when_one_is_free() {
        // The point of the whole thing: a speaker measured as a woman gets a
        // voice the user labelled as a woman's.
        let mut c = cast();
        let got = c.voice_for_preferring("speaker:0", &["gamma".to_string()]);
        assert_eq!(got, Some("gamma"));
    }

    #[test]
    fn a_preferred_voice_already_taken_falls_back_rather_than_doubling_up() {
        let mut c = cast();
        c.voice_for_preferring("speaker:0", &["gamma".to_string()]);
        let second = c.voice_for_preferring("speaker:1", &["gamma".to_string()]);
        assert!(second.is_some());
        assert_ne!(second, Some("gamma"), "two speakers were given one voice");
    }

    #[test]
    fn a_preference_never_changes_an_existing_assignment() {
        // A character who already has a voice keeps it, even if a later and
        // better-informed reading suggests a different one.
        let mut c = cast();
        let first = c.voice_for("speaker:0").unwrap().to_string();
        let later = c.voice_for_preferring("speaker:0", &["gamma".to_string()]);
        assert_eq!(later, Some(first.as_str()));
    }

    #[test]
    fn an_unknown_preference_is_ignored_rather_than_failing() {
        let mut c = cast();
        assert!(
            c.voice_for_preferring("speaker:0", &["nonexistent".to_string()])
                .is_some()
        );
    }

    #[test]
    fn the_narration_voice_is_never_handed_out_as_a_preference() {
        let mut c = cast();
        c.set_default_voice("alpha");
        let got = c.voice_for_preferring("speaker:0", &["alpha".to_string()]);
        assert_ne!(got, Some("alpha"), "a character took the narration voice");
    }

    #[test]
    fn assignment_order_is_the_same_every_run() {
        let mut first = cast();
        let mut second = cast();
        for key in ["name:x", "name:y", "name:z"] {
            assert_eq!(first.voice_for(key), second.voice_for(key));
        }
    }
}
