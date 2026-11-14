//! Separating subtitle text from player furniture.
//!
//! When the player's controls appear, the captured band picks up the title,
//! chapter name and timecode alongside the subtitle, and they get spliced into
//! the middle of the sentence. Observed on Netflix:
//!
//! ```text
//! 10 Well, being used is proof that you have the skills. Monster E1 CHAPTER 1
//! ```
//!
//! Two properties separate dialogue from chrome reliably enough to act on,
//! without needing to know anything about a particular player:
//!
//! - **Size.** Subtitles are set large for legibility. Control-bar labels and
//!   timecodes are small. Comparing each region against the tallest region in
//!   the same frame keeps this independent of resolution and band height.
//! - **Position.** Subtitles are centred. Chrome clings to the edges.
//!
//! Both are deliberately loose. Dropping a real line of dialogue is far worse
//! than occasionally reading a stray word, so the thresholds err towards
//! keeping text.

/// One OCR region reduced to what the filter needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub text: String,
    /// Horizontal centre, 0.0 at the left edge of the band, 1.0 at the right.
    pub centre_x: f64,
    /// Height as a fraction of the band.
    pub height: f64,
}

#[derive(Debug, Clone)]
pub struct FilterConfig {
    /// Keep regions at least this fraction as tall as the tallest one present.
    pub min_relative_height: f64,
    /// Keep regions whose centre falls within this margin of the band edges.
    pub edge_margin: f64,
    /// Never filter when this few regions are present.
    ///
    /// Relative height needs something to compare against, which two regions
    /// already provide: one line of dialogue beside one piece of chrome is the
    /// commonest contaminated frame there is, and the earlier threshold of
    /// three let every one of them through untouched. With a single region
    /// there is genuinely nothing to compare, so it is always kept.
    pub min_regions_to_filter: usize,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            min_relative_height: 0.6,
            edge_margin: 0.12,
            min_regions_to_filter: 2,
        }
    }
}

/// Keep the regions that look like dialogue, in the order given.
#[must_use]
pub fn keep_dialogue(regions: &[Candidate], config: &FilterConfig) -> Vec<String> {
    if regions.len() < config.min_regions_to_filter {
        return regions.iter().map(|r| r.text.clone()).collect();
    }

    let tallest = regions.iter().map(|r| r.height).fold(0.0_f64, f64::max);
    if tallest <= 0.0 {
        return regions.iter().map(|r| r.text.clone()).collect();
    }

    let kept: Vec<String> = regions
        .iter()
        .filter(|r| {
            let big_enough = r.height >= tallest * config.min_relative_height;
            let away_from_edges =
                r.centre_x >= config.edge_margin && r.centre_x <= 1.0 - config.edge_margin;
            big_enough && away_from_edges
        })
        .map(|r| r.text.clone())
        .collect();

    // If the filter would silence the frame entirely it has misjudged, so fall
    // back to everything rather than dropping a line.
    if kept.is_empty() {
        return regions.iter().map(|r| r.text.clone()).collect();
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_of_dialogue_beside_one_piece_of_chrome_is_cleaned() {
        // Observed live on Netflix: the episode title sat in the band next to
        // the dialogue and was read out mid-scene. Two regions, so the old
        // threshold of three skipped the filter entirely.
        let regions = [
            Candidate {
                text: "How is the patient?".into(),
                centre_x: 0.50,
                height: 0.42,
            },
            Candidate {
                text: "Monster E3 CHAPTER 3".into(),
                centre_x: 0.78,
                height: 0.20,
            },
        ];
        assert_eq!(
            keep_dialogue(&regions, &FilterConfig::default()),
            ["How is the patient?"]
        );
    }

    #[test]
    fn two_lines_of_one_subtitle_both_survive() {
        // The same two-region case, but both regions are dialogue. Dropping
        // either would cut a sentence in half, which is the risk the lower
        // threshold has to not introduce.
        let regions = [
            Candidate {
                text: "I don't know what".into(),
                centre_x: 0.50,
                height: 0.40,
            },
            Candidate {
                text: "you're talking about.".into(),
                centre_x: 0.48,
                height: 0.40,
            },
        ];
        assert_eq!(keep_dialogue(&regions, &FilterConfig::default()).len(), 2);
    }

    #[test]
    fn a_lone_region_is_always_kept_even_if_it_looks_like_chrome() {
        // Nothing to compare against, and silence is worse than a stray word.
        let regions = [Candidate {
            text: "Monster E3 CHAPTER 3".into(),
            centre_x: 0.78,
            height: 0.20,
        }];
        assert_eq!(keep_dialogue(&regions, &FilterConfig::default()).len(), 1);
    }

    fn c(text: &str, centre_x: f64, height: f64) -> Candidate {
        Candidate {
            text: text.into(),
            centre_x,
            height,
        }
    }

    #[test]
    fn a_lone_subtitle_passes_through_untouched() {
        let r = [c("Just the dialogue.", 0.5, 0.4)];
        assert_eq!(
            keep_dialogue(&r, &FilterConfig::default()),
            ["Just the dialogue."]
        );
    }

    #[test]
    fn small_edge_chrome_is_dropped_from_around_the_subtitle() {
        // The real Netflix case: skip-back control, subtitle, chapter label.
        let r = [
            c("10", 0.04, 0.12),
            c(
                "Well, being used is proof that you have the skills.",
                0.5,
                0.40,
            ),
            c("Monster E1 CHAPTER 1", 0.93, 0.10),
        ];
        assert_eq!(
            keep_dialogue(&r, &FilterConfig::default()),
            ["Well, being used is proof that you have the skills."]
        );
    }

    #[test]
    fn both_lines_of_a_two_line_subtitle_survive() {
        let r = [
            c("I don't know what", 0.5, 0.38),
            c("you're talking about.", 0.5, 0.40),
            c("00:14:02", 0.03, 0.09),
        ];
        assert_eq!(
            keep_dialogue(&r, &FilterConfig::default()),
            ["I don't know what", "you're talking about."]
        );
    }

    #[test]
    fn centred_chrome_of_the_same_size_is_not_separable_and_is_kept() {
        // Documents a known limit: the filter leans on size and position, so
        // large centred chrome is indistinguishable from dialogue.
        let r = [
            c("PAUSED", 0.5, 0.40),
            c("Some dialogue.", 0.5, 0.40),
            c("x", 0.02, 0.05),
        ];
        assert_eq!(
            keep_dialogue(&r, &FilterConfig::default()),
            ["PAUSED", "Some dialogue."]
        );
    }

    #[test]
    fn a_frame_of_only_chrome_is_passed_through_rather_than_silenced() {
        // Better to read a stray label than to risk swallowing dialogue the
        // heuristics misjudged.
        let r = [
            c("10", 0.02, 0.10),
            c("00:14:02", 0.04, 0.10),
            c("Monster E1", 0.97, 0.10),
        ];
        assert_eq!(keep_dialogue(&r, &FilterConfig::default()).len(), 3);
    }

    #[test]
    fn chrome_beside_dialogue_is_dropped_even_with_only_two_regions() {
        // This case used to be left alone, which is how `10` from the module's
        // own worked example ended up spliced onto the front of a sentence.
        // Small and hard against the left edge: chrome on both counts.
        let r = [c("Dialogue here.", 0.5, 0.4), c("10", 0.02, 0.1)];
        assert_eq!(
            keep_dialogue(&r, &FilterConfig::default()),
            ["Dialogue here."]
        );
    }
}
