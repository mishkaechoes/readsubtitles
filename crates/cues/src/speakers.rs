//! Working out who is speaking, so different characters can be read in
//! different voices.
//!
//! Following a film by ear is hardest when several people talk in quick
//! succession and every line arrives in the same voice. Audio subtitling
//! practice has used voice variation for this since the Dutch broadcasts of the
//! early 2000s, and it is the single biggest comprehension win available here.
//!
//! Subtitles carry three usable signals, in descending order of quality:
//!
//! 1. **Speaker labels.** `[Tenma] I'll do the surgery.` or `TENMA: ...`.
//!    Present on SDH / closed-caption tracks, rare on ordinary ones. This is
//!    the only signal that names a character, so it is the only one that can
//!    keep a voice attached to the same person all film.
//! 2. **Dashes.** `- Did you hear?` / `- No.` is the universal convention for
//!    two speakers in one cue. It says the speaker changed, not who to, so the
//!    best available is to alternate.
//! 3. **Italics.** Narration, phone voices, off-screen speech and song lyrics.
//!    Only the browser source can see this; OCR cannot read italics reliably.
//!
//! None of it is guesswork about the content: every rule here is reading a
//! convention the subtitle author deliberately used.

/// Marks an italic run in text coming from the browser extension.
///
/// A Unicode private-use character, chosen because it cannot occur in real
/// subtitle text. Any visible delimiter (asterisks, underscores) would
/// eventually collide with dialogue that legitimately contains it.
pub const ITALIC_MARK: char = '\u{E000}';

/// Who a line should be read as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Voice {
    /// A named character, from a caption label. Keeps its voice across the film.
    Named(String),
    /// The nth speaker within a single cue, from the dash convention. Only
    /// meaningful inside that cue.
    Turn(usize),
    /// Italicised: narration, a phone, someone off screen, song lyrics.
    Narration,
    /// No signal at all.
    Default,
}

impl Voice {
    /// The key a voice assignment is remembered under.
    ///
    /// Named characters key on the name, so they keep one voice for the whole
    /// film. Turns key on their position, so the second speaker in a cue always
    /// sounds different from the first without pretending to know who they are.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Named(name) => format!("name:{}", name.to_lowercase()),
            Self::Turn(n) => format!("turn:{n}"),
            Self::Narration => "narration".into(),
            Self::Default => "default".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub voice: Voice,
    pub text: String,
}

/// Split a cue into the parts that should be read in different voices.
///
/// Always returns at least one segment for non-empty input: a cue that carries
/// no speaker signal is still a line of dialogue and must be spoken.
#[must_use]
pub fn split(raw: &str) -> Vec<Segment> {
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }

    // A single leading dash is just a dash; two or more mean alternating
    // speakers. Checking this across the whole cue avoids treating a line that
    // happens to start with a hyphen as a speaker change.
    let dashed = lines.iter().filter(|l| starts_with_dash(l)).count();
    let alternating = dashed >= 2;

    let mut segments: Vec<Segment> = Vec::new();
    let mut turn = 0usize;

    for line in lines {
        let (line, new_turn) = match strip_dash(line) {
            Some(rest) if alternating => (rest, true),
            _ => (line, false),
        };
        if new_turn && !segments.is_empty() {
            turn += 1;
        }

        let italic = is_wholly_italic(line);
        let line = line.replace(ITALIC_MARK, "");
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let (name, text) = strip_speaker_label(line);
        let text = text.trim().to_string();
        if text.is_empty() {
            // A cue that is only a label, such as a lone `[Tenma]`, carries no
            // words to read.
            continue;
        }

        let voice = match (name, italic, alternating) {
            (Some(name), _, _) => Voice::Named(name),
            (None, true, _) => Voice::Narration,
            (None, false, true) => Voice::Turn(turn),
            (None, false, false) => Voice::Default,
        };

        // Consecutive lines from the same speaker are one utterance, so a
        // sentence split across two subtitle lines is not read as two.
        match segments.last_mut() {
            Some(last) if last.voice == voice && !new_turn => {
                last.text.push(' ');
                last.text.push_str(&text);
            }
            _ => segments.push(Segment { voice, text }),
        }
    }

    segments
}

fn starts_with_dash(line: &str) -> bool {
    strip_dash(line).is_some()
}

/// Strip a leading speaker dash, if this line has one.
///
/// Requires whitespace after the dash. Without that, a line opening with a
/// hyphenated word or a minus sign would be read as a speaker change.
fn strip_dash(line: &str) -> Option<&str> {
    let line = line.strip_prefix(ITALIC_MARK).unwrap_or(line);
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("– "))
        .or_else(|| line.strip_prefix("— "))?;
    Some(rest.trim_start())
}

fn is_wholly_italic(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with(ITALIC_MARK) && trimmed.ends_with(ITALIC_MARK) && trimmed.len() > 2
}

/// Pull a speaker label off the front of a line.
///
/// Two forms are recognised, and both are deliberately strict, because a false
/// positive deletes words the viewer needed to hear:
///
/// - `[Name]` or `(Name)`, which are unambiguous.
/// - `NAME:`, which is only accepted when the part before the colon is upper
///   case and at most three words. That rules out ordinary prose such as
///   "Listen: I told you already" and timestamps such as "9:30".
fn strip_speaker_label(line: &str) -> (Option<String>, &str) {
    for (open, close) in [('[', ']'), ('(', ')')] {
        if let Some(rest) = line.strip_prefix(open)
            && let Some(end) = rest.find(close)
        {
            let name = rest[..end].trim();
            // A bracketed sound effect such as [door slams] is not a speaker.
            // Labels are short; descriptions run on.
            if !name.is_empty() && name.split_whitespace().count() <= 3 {
                return (Some(name.to_string()), &rest[end + 1..]);
            }
        }
    }

    if let Some(colon) = line.find(':') {
        let (name, rest) = line.split_at(colon);
        let name = name.trim();
        let is_label = !name.is_empty()
            && name.split_whitespace().count() <= 3
            && name.chars().any(char::is_alphabetic)
            // Upper case is what separates a label from prose. Checking that no
            // character is lower case also accepts "MR. SMITH" and "DR. TENMA".
            && !name.chars().any(char::is_lowercase);
        if is_label {
            return (Some(title_case(name)), &rest[1..]);
        }
    }

    (None, line)
}

/// "TENMA" reads better than "T-E-N-M-A" to a synthesiser, and looks better in
/// a log.
fn title_case(name: &str) -> String {
    name.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn italic(s: &str) -> String {
        format!("{ITALIC_MARK}{s}{ITALIC_MARK}")
    }

    #[test]
    fn a_plain_line_is_one_segment_with_no_signal() {
        let s = split("I have an update on the spinal tumor patient.");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].voice, Voice::Default);
        assert_eq!(s[0].text, "I have an update on the spinal tumor patient.");
    }

    #[test]
    fn a_bracketed_label_names_the_speaker_and_is_not_read_aloud() {
        let s = split("[Tenma] I'll do the surgery.");
        assert_eq!(
            s,
            [Segment {
                voice: Voice::Named("Tenma".into()),
                text: "I'll do the surgery.".into()
            }]
        );
    }

    #[test]
    fn an_upper_case_colon_label_names_the_speaker() {
        let s = split("TENMA: I'll do the surgery.");
        assert_eq!(s[0].voice, Voice::Named("Tenma".into()));
        assert_eq!(s[0].text, "I'll do the surgery.");
    }

    #[test]
    fn a_title_in_a_label_survives() {
        assert_eq!(
            split("DR. TENMA: Hello.")[0].voice,
            Voice::Named("Dr. Tenma".into())
        );
    }

    #[test]
    fn ordinary_prose_containing_a_colon_is_not_mistaken_for_a_label() {
        // The failure that matters: treating this as a label would swallow
        // "Listen" and change the meaning of the line.
        let s = split("Listen: I told you already.");
        assert_eq!(s[0].voice, Voice::Default);
        assert_eq!(s[0].text, "Listen: I told you already.");
    }

    #[test]
    fn a_time_is_not_a_speaker_label() {
        assert_eq!(split("Be there at 9:30.")[0].text, "Be there at 9:30.");
        assert_eq!(split("Be there at 9:30.")[0].voice, Voice::Default);
    }

    #[test]
    fn a_sound_effect_is_not_treated_as_a_speaker() {
        // Too many words to be a name; must not become a character.
        let s = split("[a door slams somewhere upstairs] Who's there?");
        assert_eq!(s[0].voice, Voice::Default);
    }

    #[test]
    fn two_dashed_lines_become_two_turns() {
        let s = split("- Did you hear?\n- No.");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].voice, Voice::Turn(0));
        assert_eq!(s[0].text, "Did you hear?");
        assert_eq!(s[1].voice, Voice::Turn(1));
        assert_eq!(s[1].text, "No.");
    }

    #[test]
    fn a_single_dashed_line_is_not_a_speaker_change() {
        // One dash is just punctuation. Splitting here would invent a speaker.
        let s = split("- Well, that settles it.");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].voice, Voice::Default);
    }

    #[test]
    fn a_line_opening_with_a_hyphenated_word_is_not_a_speaker_change() {
        let s = split("-fold increase, he said.\n-fold again.");
        assert_eq!(s.len(), 1, "no space after the dash, so not a turn marker");
    }

    #[test]
    fn a_two_line_sentence_from_one_speaker_is_read_as_one_utterance() {
        let s = split("I don't know what\nyou're talking about.");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].text, "I don't know what you're talking about.");
    }

    #[test]
    fn a_wholly_italic_line_is_narration() {
        let s = split(&italic("Nine years later."));
        assert_eq!(s[0].voice, Voice::Narration);
        assert_eq!(s[0].text, "Nine years later.");
    }

    #[test]
    fn a_named_speaker_outranks_italics() {
        // A labelled line that happens to be italic is still that character.
        let s = split(&italic("[Tenma] Over the phone."));
        assert_eq!(s[0].voice, Voice::Named("Tenma".into()));
    }

    #[test]
    fn narration_and_dialogue_in_one_cue_are_separated() {
        let raw = format!("{}\nWho's there?", italic("Meanwhile, in Düsseldorf..."));
        let s = split(&raw);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].voice, Voice::Narration);
        assert_eq!(s[1].voice, Voice::Default);
    }

    #[test]
    fn dashes_and_names_combine() {
        let s = split("- [Tenma] I'll do it.\n- [Eva] You can't.");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].voice, Voice::Named("Tenma".into()));
        assert_eq!(s[1].voice, Voice::Named("Eva".into()));
    }

    #[test]
    fn the_same_character_keys_the_same_whatever_the_capitalisation() {
        assert_eq!(
            Voice::Named("Tenma".into()).key(),
            Voice::Named("TENMA".into()).key()
        );
    }

    #[test]
    fn a_label_with_nothing_after_it_produces_nothing_to_say() {
        assert!(split("[Tenma]").is_empty());
    }

    #[test]
    fn empty_input_produces_no_segments() {
        assert!(split("").is_empty());
        assert!(split("   \n  \n").is_empty());
    }

    #[test]
    fn the_italic_marker_never_reaches_the_spoken_text() {
        let raw = format!("He said {}then left.", italic("quietly"));
        let spoken = &split(&raw)[0].text;
        assert!(
            !spoken.contains(ITALIC_MARK),
            "marker leaked into {spoken:?}"
        );
    }

    #[test]
    fn real_captured_lines_pass_through_unharmed() {
        // Taken verbatim from a live capture, to catch over-eager rules.
        for line in [
            "I have an update on the spinal tumor patient.",
            "The paralysis seems to have progressed rapidly.",
            "Got it. I'll do the surgery.",
            "Thank you, Chief.",
            "...and I was fighting over that dress with my friend.",
            "One, two, three!",
        ] {
            let s = split(line);
            assert_eq!(s.len(), 1, "{line:?} was split unexpectedly");
            assert_eq!(s[0].text, line, "{line:?} was altered");
        }
    }
}
