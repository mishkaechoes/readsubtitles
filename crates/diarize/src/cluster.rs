//! Deciding whether a voice has been heard before.
//!
//! A speaker embedding is a fixed-length vector describing a voice rather than
//! the words in it. Two recordings of one person land close together; two
//! people land apart. Identity is then a matter of distance, and because the
//! film's cast is not known in advance, clusters have to form as it plays.
//!
//! What matters most here is the same thing that matters in `Cast`: stability.
//! A character whose voice changes every few scenes is worse than no separation
//! at all, because the listener has to re-learn who is who. So a cluster, once
//! formed, is never split or renumbered; it only accumulates.
//!
//! The pessimistic cases are handled explicitly, because getting them wrong is
//! what makes this feel broken:
//!
//! - **Not enough voice.** Music, effects or silence under a subtitle must not
//!   invent a speaker. Those return [`Assignment::Unknown`] and the caller
//!   keeps the main voice.
//! - **Two people in one window.** A cue holding two speakers produces a
//!   blended embedding that belongs to neither. The caller is expected not to
//!   ask in that case; see [`Assignment::Unknown`].

/// How similar two embeddings must be to count as the same person.
///
/// Cosine similarity, so 1.0 is identical and 0.0 unrelated. The right value
/// depends on the embedding model and has to be measured on real film audio;
/// this is a starting point, not a tuned figure.
pub const DEFAULT_THRESHOLD: f32 = 0.50;

/// Beyond this many distinct voices, new ones join the nearest cluster instead.
///
/// Films have large casts but only a handful of speaking parts per scene, and
/// there are only so many voices to hand out. Letting clusters grow without
/// limit would spend them all on one-line bit parts.
pub const DEFAULT_MAX_SPEAKERS: usize = 8;

/// Loudness below which a window is treated as having no voice in it.
///
/// Measured as RMS on the same scale the capture produces. Deliberately low:
/// quiet dialogue is still dialogue, and the cost of a false "unknown" is only
/// that the line is read in the main voice.
pub const DEFAULT_MIN_LOUDNESS: f32 = 0.004;

/// Sightings before a cluster is treated as a character rather than a noise.
pub const DEFAULT_MIN_SIGHTINGS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assignment {
    /// The nth distinct voice heard so far. Stable for the session.
    Speaker(usize),
    /// Not enough voice in the window to say. The caller should fall back to
    /// the main voice rather than guessing.
    Unknown,
}

impl Assignment {
    /// The key a voice assignment is remembered under, if there is one.
    #[must_use]
    pub fn key(self) -> Option<String> {
        match self {
            Self::Speaker(n) => Some(format!("speaker:{n}")),
            Self::Unknown => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Centroid {
    /// Running mean of the cluster's embeddings, kept normalised so that a
    /// dot product is a cosine similarity.
    vector: Vec<f32>,
    count: usize,
    /// Stable across merges, unlike the position in the list.
    ///
    /// Clusters get merged when they drift together, which removes entries and
    /// would renumber everything after them. A character's voice is keyed on
    /// this, so it has to survive that.
    id: usize,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub threshold: f32,
    pub max_speakers: usize,
    /// How many times a voice must be heard before it earns its own voice.
    ///
    /// Music, effects and one-word interjections all produce embeddings, and
    /// on a real soundtrack most one-off clusters are not people. Waiting for a
    /// second sighting costs a new character their first line in the main voice
    /// and buys not inventing characters out of the orchestra.
    pub min_sightings: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
            max_speakers: DEFAULT_MAX_SPEAKERS,
            min_sightings: DEFAULT_MIN_SIGHTINGS,
        }
    }
}

/// Groups voice embeddings into speakers as the film plays.
#[derive(Debug, Clone)]
pub struct Clusterer {
    centroids: Vec<Centroid>,
    config: Config,
    /// Source of stable ids. Only ever increases, so a merged-away id is never
    /// handed to a different person later.
    next_id: usize,
}

impl Clusterer {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            centroids: Vec::new(),
            config,
            next_id: 0,
        }
    }

    /// Place an embedding, creating a new speaker if it matches nobody.
    pub fn assign(&mut self, embedding: &[f32]) -> Assignment {
        self.assign_scored(embedding).0
    }

    /// As [`Self::assign`], but also returns how well the embedding matched the
    /// nearest existing speaker.
    ///
    /// The score is what the similarity threshold has to be chosen against, and
    /// it cannot be chosen sensibly without seeing real values from real film
    /// audio, so it is reportable rather than hidden.
    pub fn assign_scored(&mut self, embedding: &[f32]) -> (Assignment, f32) {
        let Some(v) = normalise(embedding) else {
            // A zero or empty vector carries no information; treating it as a
            // voice would put every silent window in one phantom cluster.
            return (Assignment::Unknown, 0.0);
        };

        let best = self
            .centroids
            .iter()
            .enumerate()
            .map(|(i, c)| (i, dot(&c.vector, &v)))
            .max_by(|a, b| a.1.total_cmp(&b.1));

        let score = best.map_or(0.0, |(_, s)| s);
        let index = match best {
            Some((i, s)) if s >= self.config.threshold => {
                self.absorb(i, &v);
                i
            }
            // Out of room: join the nearest rather than refusing, so a crowded
            // scene still separates the people it can.
            Some((i, _)) if self.centroids.len() >= self.config.max_speakers => {
                self.absorb(i, &v);
                i
            }
            _ => {
                let id = self.next_id;
                self.next_id += 1;
                self.centroids.push(Centroid {
                    vector: v,
                    count: 1,
                    id,
                });
                self.centroids.len() - 1
            }
        };

        let id = self.centroids[index].id;
        let count = self.centroids[index].count;
        self.merge_drifted();

        // Heard only once: probably not a person. Say so rather than handing a
        // sound effect a speaking part.
        if count < self.config.min_sightings {
            return (Assignment::Unknown, score);
        }
        (Assignment::Speaker(id), score)
    }

    /// Fold together any two clusters that have drifted within the threshold.
    ///
    /// Centroids move as they absorb, so two clusters that were distinct when
    /// created can end up closer than the distance at which they would have
    /// been considered one person. Leaving them apart means one character is
    /// read in two voices, which is exactly the instability this is meant to
    /// avoid. The older cluster wins, so a character keeps the voice they
    /// already had.
    fn merge_drifted(&mut self) {
        loop {
            let mut pair = None;
            'search: for i in 0..self.centroids.len() {
                for j in (i + 1)..self.centroids.len() {
                    if dot(&self.centroids[i].vector, &self.centroids[j].vector)
                        >= self.config.threshold
                    {
                        pair = Some((i, j));
                        break 'search;
                    }
                }
            }
            let Some((i, j)) = pair else { return };

            let (keep, drop) = if self.centroids[i].id <= self.centroids[j].id {
                (i, j)
            } else {
                (j, i)
            };
            let gone = self.centroids.remove(drop);
            let keep = if drop < keep { keep - 1 } else { keep };

            let c = &mut self.centroids[keep];
            let (a, b) = (c.count as f32, gone.count as f32);
            for (slot, s) in c.vector.iter_mut().zip(&gone.vector) {
                *slot = (*slot * a + s * b) / (a + b);
            }
            c.count += gone.count;
            if let Some(n) = normalise(&c.vector) {
                c.vector = n;
            }
        }
    }

    /// The closest any two speakers sit to each other.
    ///
    /// The headroom the threshold has to work in: a value near the threshold
    /// means two characters are on the verge of being merged.
    #[must_use]
    pub fn closest_pair(&self) -> Option<f32> {
        let mut worst: Option<f32> = None;
        for (i, a) in self.centroids.iter().enumerate() {
            for b in self.centroids.iter().skip(i + 1) {
                let s = dot(&a.vector, &b.vector);
                worst = Some(worst.map_or(s, |w: f32| w.max(s)));
            }
        }
        worst
    }

    /// Fold an embedding into a cluster's running mean.
    fn absorb(&mut self, index: usize, v: &[f32]) {
        let c = &mut self.centroids[index];
        let n = c.count as f32;
        for (slot, s) in c.vector.iter_mut().zip(v) {
            *slot = (*slot * n + s) / (n + 1.0);
        }
        c.count += 1;
        if let Some(renormalised) = normalise(&c.vector) {
            c.vector = renormalised;
        }
    }

    #[must_use]
    pub fn speakers(&self) -> usize {
        self.centroids.len()
    }

    /// Clusters heard often enough to count as characters.
    #[must_use]
    pub fn confirmed(&self) -> usize {
        self.centroids
            .iter()
            .filter(|c| c.count >= self.config.min_sightings)
            .count()
    }

    /// How many windows have been attributed to each speaker, in order.
    ///
    /// Useful for reporting: a cluster with one member is probably noise, and
    /// seeing that is how the threshold gets tuned.
    #[must_use]
    pub fn counts(&self) -> Vec<usize> {
        self.centroids.iter().map(|c| c.count).collect()
    }
}

/// Scale a vector to unit length, so a dot product is a cosine similarity.
///
/// Returns `None` for an empty or zero-length vector, which carries no
/// direction and therefore no identity.
#[must_use]
pub fn normalise(v: &[f32]) -> Option<Vec<f32>> {
    if v.is_empty() {
        return None;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return None;
    }
    Some(v.iter().map(|x| x / norm).collect())
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Whether a window holds enough sound to be worth embedding.
///
/// A cheap energy gate, not voice detection. It exists to stop silence and
/// quiet music beds from being embedded and clustered as if they were people.
#[must_use]
pub fn has_voice(samples: &[f32], min_loudness: f32) -> bool {
    if samples.is_empty() {
        return false;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    rms >= min_loudness
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clusterer that names a speaker the first time it hears them.
    ///
    /// Used for tests about the clustering itself, so the confirmation rule
    /// does not obscure what is being checked. The rule has its own tests.
    fn clusterer() -> Clusterer {
        Clusterer::new(Config {
            min_sightings: 1,
            ..Config::default()
        })
    }

    /// A voice, as a direction in embedding space.
    fn voice(seed: f32) -> Vec<f32> {
        (0..64)
            .map(|i| ((i as f32 * seed).sin() + seed).abs())
            .collect()
    }

    /// The same voice heard again, with a little noise on top.
    fn again(v: &[f32], noise: f32) -> Vec<f32> {
        v.iter()
            .enumerate()
            .map(|(i, x)| x + noise * ((i as f32).cos()))
            .collect()
    }

    #[test]
    fn the_first_voice_becomes_the_first_speaker() {
        let mut c = clusterer();
        assert_eq!(c.assign(&voice(1.0)), Assignment::Speaker(0));
        assert_eq!(c.speakers(), 1);
    }

    #[test]
    fn the_same_voice_again_is_the_same_speaker() {
        let mut c = clusterer();
        let v = voice(1.0);
        let first = c.assign(&v);
        assert_eq!(c.assign(&again(&v, 0.01)), first);
        assert_eq!(c.speakers(), 1, "a near-identical voice made a new cluster");
    }

    #[test]
    fn a_clearly_different_voice_becomes_a_new_speaker() {
        let mut c = clusterer();
        c.assign(&voice(1.0));
        // Orthogonal direction: as different as two vectors get.
        let mut other = vec![0.0; 64];
        other[0] = 1.0;
        let mut first = vec![0.0; 64];
        first[1] = 1.0;
        let mut c2 = clusterer();
        c2.assign(&first);
        assert_eq!(c2.assign(&other), Assignment::Speaker(1));
        assert_eq!(c2.speakers(), 2);
    }

    #[test]
    fn a_speaker_keeps_their_number_across_the_whole_film() {
        // The property that matters most: renumbering mid-film would make the
        // listener re-learn who is who.
        let mut c = clusterer();
        let hero = voice(1.0);
        let first = c.assign(&hero);
        for other in 2..6 {
            c.assign(&voice(other as f32));
        }
        assert_eq!(c.assign(&again(&hero, 0.02)), first);
    }

    #[test]
    fn an_empty_embedding_is_unknown_rather_than_a_speaker() {
        let mut c = clusterer();
        assert_eq!(c.assign(&[]), Assignment::Unknown);
        assert_eq!(c.speakers(), 0);
    }

    #[test]
    fn a_silent_embedding_is_unknown_rather_than_a_phantom_speaker() {
        // Every silent window would otherwise collapse into one cluster and be
        // read as a recurring character who never speaks.
        let mut c = clusterer();
        assert_eq!(c.assign(&vec![0.0; 64]), Assignment::Unknown);
        assert_eq!(c.speakers(), 0);
    }

    #[test]
    fn the_speaker_cap_is_respected() {
        let mut c = Clusterer::new(Config {
            threshold: 0.99,
            max_speakers: 3,
            min_sightings: 1,
        });
        for i in 0..20 {
            let mut v = vec![0.0; 64];
            v[i % 64] = 1.0;
            c.assign(&v);
        }
        assert_eq!(c.speakers(), 3);
    }

    #[test]
    fn a_full_clusterer_still_assigns_rather_than_refusing() {
        let mut c = Clusterer::new(Config {
            threshold: 0.99,
            max_speakers: 2,
            min_sightings: 1,
        });
        let mut a = vec![0.0; 8];
        a[0] = 1.0;
        let mut b = vec![0.0; 8];
        b[1] = 1.0;
        let mut d = vec![0.0; 8];
        d[2] = 1.0;
        c.assign(&a);
        c.assign(&b);
        assert!(matches!(c.assign(&d), Assignment::Speaker(_)));
    }

    #[test]
    fn scale_does_not_change_identity() {
        // A louder recording of a voice is the same voice.
        let mut c = clusterer();
        let v = voice(1.0);
        let loud: Vec<f32> = v.iter().map(|x| x * 8.0).collect();
        let first = c.assign(&v);
        assert_eq!(c.assign(&loud), first);
    }

    #[test]
    fn counts_track_how_often_each_speaker_was_heard() {
        let mut c = clusterer();
        let v = voice(1.0);
        c.assign(&v);
        c.assign(&again(&v, 0.01));
        c.assign(&again(&v, 0.02));
        assert_eq!(c.counts(), [3]);
    }

    #[test]
    fn a_voice_heard_once_is_not_yet_a_character() {
        // A single window of music or a slammed door must not get a speaking
        // part, so a cluster has to be confirmed before it earns a voice.
        let mut c = Clusterer::new(Config::default());
        assert_eq!(c.assign(&voice(1.0)), Assignment::Unknown);
        assert_eq!(c.confirmed(), 0);
    }

    #[test]
    fn a_voice_heard_twice_becomes_a_character() {
        let mut c = Clusterer::new(Config::default());
        let v = voice(1.0);
        assert_eq!(c.assign(&v), Assignment::Unknown);
        assert!(matches!(c.assign(&again(&v, 0.01)), Assignment::Speaker(_)));
        assert_eq!(c.confirmed(), 1);
    }

    #[test]
    fn clusters_that_drift_together_are_merged() {
        // Centroids move as they absorb. Two that end up closer than the
        // threshold are the same person, and leaving them apart reads one
        // character in two voices.
        let mut c = clusterer();
        let mut a = vec![0.0; 8];
        a[0] = 1.0;
        let mut b = vec![0.0; 8];
        b[0] = 0.6;
        b[1] = 0.8;
        c.assign(&a);
        c.assign(&b);
        // Both directions pulled towards each other by shared evidence.
        let middle = vec![0.8, 0.6, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        c.assign(&middle);
        c.assign(&middle);
        assert_eq!(c.speakers(), 1, "drifted clusters were not merged");
    }

    #[test]
    fn merging_keeps_the_older_identity() {
        // The character already has a voice; the merge must not move them to
        // the newer cluster's identity and change how they sound.
        let mut c = clusterer();
        let mut a = vec![0.0; 8];
        a[0] = 1.0;
        let first = c.assign(&a);
        let mut b = vec![0.0; 8];
        b[0] = 0.6;
        b[1] = 0.8;
        c.assign(&b);
        let middle = vec![0.8, 0.6, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        c.assign(&middle);
        c.assign(&middle);
        assert_eq!(c.assign(&a), first, "identity changed after a merge");
    }

    #[test]
    fn no_two_speakers_are_left_closer_than_the_threshold() {
        // The self-consistency the live measurement showed was missing: two
        // centroids sat closer together than the distance at which they would
        // have been called one person.
        let mut c = clusterer();
        for i in 0..40 {
            let t = i as f32 * 0.05;
            c.assign(&[t.cos(), t.sin(), 0.1, 0.0, 0.0, 0.0, 0.0, 0.0]);
        }
        if let Some(worst) = c.closest_pair() {
            assert!(
                worst < Config::default().threshold,
                "two speakers left at {worst}, above the merge threshold"
            );
        }
    }

    #[test]
    fn an_assignment_key_is_stable_and_unknown_has_none() {
        assert_eq!(Assignment::Speaker(2).key().as_deref(), Some("speaker:2"));
        assert_eq!(Assignment::Unknown.key(), None);
    }

    #[test]
    fn silence_does_not_count_as_voice() {
        assert!(!has_voice(&[], DEFAULT_MIN_LOUDNESS));
        assert!(!has_voice(&vec![0.0; 16_000], DEFAULT_MIN_LOUDNESS));
        assert!(!has_voice(&vec![0.0001; 16_000], DEFAULT_MIN_LOUDNESS));
    }

    #[test]
    fn ordinary_speech_levels_count_as_voice() {
        // Around the loudness the live capture measured on a playing film.
        assert!(has_voice(&vec![0.02; 16_000], DEFAULT_MIN_LOUDNESS));
    }

    #[test]
    fn a_vector_of_infinities_is_rejected_rather_than_poisoning_a_cluster() {
        let mut c = clusterer();
        assert_eq!(c.assign(&[f32::INFINITY; 8]), Assignment::Unknown);
        assert!(normalise(&[f32::NAN; 8]).is_none());
    }
}
