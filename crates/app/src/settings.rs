//! Persisted settings.
//!
//! Stored as a small hand-rolled key/value file rather than JSON, because the
//! whole schema is a dozen scalars and a serialiser dependency would be more
//! weight than the problem deserves. Unknown keys are preserved on rewrite so a
//! newer build's settings survive an older one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use readsubtitles_capture::Band;

/// Where captions come from.
///
/// Two genuinely different trade-offs, not two implementations of one thing.
/// Screen reading works on anything that puts text on a display and needs the
/// Screen Recording permission. Browser reading gives exact text and needs no
/// permission at all, but only covers sites the extension runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    Screen,
    Browser,
}

impl Source {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Browser => "browser",
        }
    }

    #[must_use]
    pub fn parse(v: &str) -> Option<Self> {
        match v.trim().to_ascii_lowercase().as_str() {
            "screen" | "ocr" | "capture" => Some(Self::Screen),
            "browser" | "dom" | "extension" => Some(Self::Browser),
            _ => None,
        }
    }

    /// Spoken when the source changes, so the user knows which one is active
    /// without reading a menu.
    #[must_use]
    pub fn spoken_name(self) -> &'static str {
        match self {
            Self::Screen => "reading the screen",
            Self::Browser => "reading captions from the browser",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub source: Source,
    /// Loopback port the browser extension posts captions to.
    pub bridge_port: u16,
    /// Window title substring to watch. Preferred over a display index because
    /// it survives the window moving between screens.
    pub window: Option<String>,
    pub display: usize,
    pub band: Band,
    pub language: String,
    /// `None` means the best installed on-device voice for `language`.
    pub voice: Option<String>,
    /// When set, use this ElevenLabs voice instead of the on-device one.
    /// This is the voice for narration and for any line with no speaker signal,
    /// which on an ordinary subtitle track is most of them.
    pub elevenlabs_voice: Option<String>,
    /// Extra ElevenLabs voices for named characters, with a human label each.
    /// The label is for the person reading the settings file; nothing in the
    /// subtitles says which character is female or villainous, so the matching
    /// is done by `cast_overrides` rather than guessed.
    pub elevenlabs_cast: Vec<CastEntry>,
    /// Characters deliberately pinned to a voice, by caption label.
    pub cast_overrides: Vec<(String, String)>,
    /// Read different speakers in different voices where the subtitles say so.
    pub multi_voice: bool,
    /// Also tell speakers apart by listening to the film's soundtrack.
    pub diarize: bool,
    /// Milliseconds to listen after a subtitle appears before reading it.
    pub listen_ahead_ms: u64,
    pub rate: f32,
    pub fps: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            source: Source::default(),
            bridge_port: readsubtitles_browser::DEFAULT_PORT,
            window: None,
            display: 0,
            band: Band::default(),
            language: "en-US".into(),
            voice: None,
            elevenlabs_voice: None,
            elevenlabs_cast: Vec::new(),
            cast_overrides: Vec::new(),
            multi_voice: true,
            diarize: false,
            listen_ahead_ms: 900,
            rate: 1.0,
            fps: 8,
        }
    }
}

#[must_use]
pub fn settings_path() -> PathBuf {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Library/Application Support/readsubtitles/settings.conf")
}

fn parse(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

impl Settings {
    /// Load from disk, falling back to defaults for anything missing or
    /// malformed. A corrupt settings file must never stop the app starting.
    #[must_use]
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(settings_path()) else {
            return Self::default();
        };
        let map = parse(&text);
        let mut s = Self::default();

        let get = |k: &str| map.get(k).filter(|v| !v.is_empty()).cloned();
        if let Some(v) = get("source").and_then(|v| Source::parse(&v)) {
            s.source = v;
        }
        if let Some(v) = get("bridge_port").and_then(|v| v.parse::<u16>().ok()) {
            // Port 0 would ask the OS to pick, which the extension could not
            // then find, so treat it as unset.
            if v > 0 {
                s.bridge_port = v;
            }
        }
        s.window = get("window");
        s.voice = get("voice");
        s.elevenlabs_voice = get("elevenlabs_voice");
        s.elevenlabs_cast = get("elevenlabs_cast")
            .map(|v| parse_cast(&v))
            .unwrap_or_default();
        s.cast_overrides = get("cast_overrides")
            .map(|v| parse_overrides(&v))
            .unwrap_or_default();
        if let Some(v) = get("diarize") {
            s.diarize = !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "false" | "off" | "0"
            );
        }
        if let Some(v) = get("listen_ahead_ms")
            && let Ok(n) = v.trim().parse::<u64>()
        {
            s.listen_ahead_ms = n.clamp(0, 4000);
        }
        if let Some(v) = get("multi_voice") {
            s.multi_voice = !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "false" | "off" | "0"
            );
        }
        if let Some(v) = get("language") {
            s.language = v;
        }
        if let Some(v) = get("display").and_then(|v| v.parse().ok()) {
            s.display = v;
        }
        if let Some(v) = get("rate").and_then(|v| v.parse::<f32>().ok()) {
            s.rate = v.clamp(0.5, 3.0);
        }
        if let Some(v) = get("fps").and_then(|v| v.parse::<u32>().ok()) {
            s.fps = v.clamp(1, 60);
        }
        if let Some(b) = get("band").and_then(|v| parse_band(&v)) {
            s.band = b;
        }
        s
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Preserve anything a newer build wrote that this one does not know.
        let mut map = std::fs::read_to_string(&path)
            .map(|t| parse(&t))
            .unwrap_or_default();

        let mut set = |k: &str, v: String| {
            if v.is_empty() {
                map.remove(k);
            } else {
                map.insert(k.to_string(), v);
            }
        };
        set("source", self.source.as_str().to_string());
        set("bridge_port", self.bridge_port.to_string());
        set("window", self.window.clone().unwrap_or_default());
        set("display", self.display.to_string());
        set(
            "band",
            format!(
                "{},{},{},{}",
                self.band.x, self.band.y, self.band.width, self.band.height
            ),
        );
        set("language", self.language.clone());
        set("voice", self.voice.clone().unwrap_or_default());
        set(
            "elevenlabs_voice",
            self.elevenlabs_voice.clone().unwrap_or_default(),
        );
        set("elevenlabs_cast", format_cast(&self.elevenlabs_cast));
        set("cast_overrides", format_overrides(&self.cast_overrides));
        set("multi_voice", self.multi_voice.to_string());
        set("diarize", self.diarize.to_string());
        set("listen_ahead_ms", self.listen_ahead_ms.to_string());
        set("rate", self.rate.to_string());
        set("fps", self.fps.to_string());

        let body: String = map.iter().map(|(k, v)| format!("{k} = {v}\n")).collect();
        std::fs::write(path, format!("# readsubtitles settings\n{body}"))
    }
}

/// One voice available for characters, with a note on what it suits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastEntry {
    pub voice_id: String,
    /// Free text, for the person editing the settings. Never interpreted.
    pub label: String,
}

/// `id:label, id:label`. The label may contain spaces but not a comma.
fn parse_cast(v: &str) -> Vec<CastEntry> {
    v.split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let (voice_id, label) = part.split_once(':').unwrap_or((part, ""));
            let voice_id = voice_id.trim();
            if voice_id.is_empty() {
                return None;
            }
            Some(CastEntry {
                voice_id: voice_id.to_string(),
                label: label.trim().to_string(),
            })
        })
        .collect()
}

fn format_cast(cast: &[CastEntry]) -> String {
    cast.iter()
        .map(|e| {
            if e.label.is_empty() {
                e.voice_id.clone()
            } else {
                format!("{}:{}", e.voice_id, e.label)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `Character=voiceid, Character=voiceid`.
fn parse_overrides(v: &str) -> Vec<(String, String)> {
    v.split(',')
        .filter_map(|part| {
            let (name, voice) = part.trim().split_once('=')?;
            let (name, voice) = (name.trim(), voice.trim());
            if name.is_empty() || voice.is_empty() {
                return None;
            }
            Some((name.to_lowercase(), voice.to_string()))
        })
        .collect()
}

fn format_overrides(overrides: &[(String, String)]) -> String {
    overrides
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_band(v: &str) -> Option<Band> {
    let n: Vec<f64> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    if n.len() != 4 {
        return None;
    }
    // A zero-sized or inverted band would make the capture fail at a point far
    // from the cause, so reject it here.
    if n[2] <= 0.0 || n[3] <= 0.0 {
        return None;
    }
    Some(Band {
        x: n[0],
        y: n[1],
        width: n[2],
        height: n[3],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_band_round_trips_through_its_text_form() {
        let b = parse_band("0.1,0.78,0.8,0.22").unwrap();
        assert!((b.x - 0.1).abs() < 1e-9 && (b.height - 0.22).abs() < 1e-9);
    }

    #[test]
    fn a_malformed_band_is_rejected_rather_than_half_applied() {
        assert!(parse_band("0.1,0.2").is_none());
        assert!(parse_band("not,a,band,at all").is_none());
    }

    #[test]
    fn a_zero_sized_band_is_rejected() {
        assert!(parse_band("0.1,0.2,0,0.5").is_none());
        assert!(parse_band("0.1,0.2,0.5,0").is_none());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let m = parse("# a comment\n\n  rate = 1.5  \nvoice=com.apple.x\n");
        assert_eq!(m.get("rate").map(String::as_str), Some("1.5"));
        assert_eq!(m.get("voice").map(String::as_str), Some("com.apple.x"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn every_spelling_of_the_two_sources_is_understood() {
        for v in ["screen", "Screen", " OCR ", "capture"] {
            assert_eq!(Source::parse(v), Some(Source::Screen), "{v}");
        }
        for v in ["browser", "DOM", "extension"] {
            assert_eq!(Source::parse(v), Some(Source::Browser), "{v}");
        }
        assert_eq!(Source::parse("telepathy"), None);
    }

    #[test]
    fn a_source_round_trips_through_its_saved_form() {
        for source in [Source::Screen, Source::Browser] {
            assert_eq!(Source::parse(source.as_str()), Some(source));
        }
    }

    #[test]
    fn a_cast_list_keeps_its_labels() {
        let cast = parse_cast("abc:female, def:evil characters, ghi");
        assert_eq!(cast.len(), 3);
        assert_eq!(cast[0].voice_id, "abc");
        assert_eq!(cast[0].label, "female");
        assert_eq!(cast[1].label, "evil characters");
        assert_eq!(cast[2].label, "");
    }

    #[test]
    fn a_cast_list_round_trips() {
        let text = "abc:female, def:evil characters";
        assert_eq!(format_cast(&parse_cast(text)), text);
    }

    #[test]
    fn a_malformed_cast_entry_is_dropped_rather_than_breaking_the_rest() {
        let cast = parse_cast("abc:female, , :orphan, def:evil");
        assert_eq!(cast.len(), 2);
        assert_eq!(cast[1].voice_id, "def");
    }

    #[test]
    fn overrides_are_matched_without_regard_to_capitalisation() {
        let o = parse_overrides("Tenma=abc, EVA=def");
        assert_eq!(
            o,
            [
                ("tenma".to_string(), "abc".to_string()),
                ("eva".to_string(), "def".to_string())
            ]
        );
    }

    #[test]
    fn an_override_without_a_voice_is_ignored() {
        assert!(parse_overrides("Tenma=, =abc, nonsense").is_empty());
    }

    #[test]
    fn a_line_without_an_equals_sign_is_skipped() {
        assert!(parse("garbage line\n").is_empty());
    }
}
