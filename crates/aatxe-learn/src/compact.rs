//! Bounded keep-best-N compaction.
//!
//! Called at the end of every harvest cycle:
//!
//! 1. Recompute the score on every entry against the current "now".
//! 2. Drop entries whose score is below `min_score` (decayed-to-nothing).
//! 3. Sort by score descending.
//! 4. Truncate to `max_entries`.
//!
//! Persisting the score back into [`LearnedEntry::score`] is intentional:
//! downstream tools (`aatxe learn show`, the rendered corpus summary) can
//! read it without rerunning the scoring algorithm and without depending
//! on this crate's particular weights.

use crate::score::{score_entry, ScoringOptions};
use crate::types::LearningCorpus;
use time::OffsetDateTime;

/// Tunables for compaction. Defaults keep a corpus bounded to ≤ 100
/// entries and evict anything that's decayed below 0.1.
#[derive(Debug, Clone, Copy)]
pub struct CompactOptions {
    pub max_entries: usize,
    pub min_score: f64,
    pub scoring: ScoringOptions,
}

impl Default for CompactOptions {
    fn default() -> Self {
        Self {
            max_entries: 100,
            min_score: 0.1,
            scoring: ScoringOptions::default(),
        }
    }
}

/// In-place compaction. Returns the number of entries evicted.
pub fn compact(corpus: &mut LearningCorpus, opts: &CompactOptions, now: OffsetDateTime) -> usize {
    let before = corpus.entries.len();

    // 1. Score every entry.
    for e in corpus.entries.iter_mut() {
        e.score = score_entry(e, now, &opts.scoring);
    }

    // 2. Drop below-threshold.
    corpus.entries.retain(|e| e.score >= opts.min_score);

    // 3. Sort by score desc, then by last_seen desc as a tiebreak. Stable
    //    so equal-score entries keep their relative order.
    corpus.entries.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.last_seen.cmp(&a.last_seen))
            .then_with(|| a.id.cmp(&b.id))
    });

    // 4. Truncate to cap.
    if corpus.entries.len() > opts.max_entries {
        corpus.entries.truncate(opts.max_entries);
    }

    before - corpus.entries.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LearnedEntry, SignalKind};

    fn now() -> OffsetDateTime {
        use time::format_description::well_known::Rfc3339;
        OffsetDateTime::parse("2026-06-02T00:00:00Z", &Rfc3339).unwrap()
    }

    fn e(id: &str, kind: SignalKind, confirms: u32, last_seen: &str) -> LearnedEntry {
        LearnedEntry {
            id: id.into(),
            guidance: id.into(),
            file_glob: None,
            category: None,
            strongest_kind: kind,
            source_pr: 1,
            source_comment_id: None,
            confirmations: confirms,
            refutations: 0,
            first_seen: last_seen.into(),
            last_seen: last_seen.into(),
            score: 0.0,
        }
    }

    #[test]
    fn compact_drops_below_min_score() {
        let mut c = LearningCorpus::empty("x/y");
        c.entries.push(e(
            "stale",
            SignalKind::InferredIgnored,
            0,
            "2024-01-01T00:00:00Z",
        )); // ancient + low source authority → near zero
        c.entries.push(e(
            "fresh",
            SignalKind::UserDirective,
            0,
            "2026-06-01T00:00:00Z",
        ));
        let evicted = compact(&mut c, &CompactOptions::default(), now());
        assert_eq!(evicted, 1);
        assert_eq!(c.entries.len(), 1);
        assert_eq!(c.entries[0].id, "fresh");
    }

    #[test]
    fn compact_orders_by_score_desc() {
        let mut c = LearningCorpus::empty("x/y");
        c.entries.push(e(
            "low",
            SignalKind::InferredApplied,
            1,
            "2026-06-01T00:00:00Z",
        ));
        c.entries.push(e(
            "high",
            SignalKind::UserDirective,
            5,
            "2026-06-01T00:00:00Z",
        ));
        c.entries
            .push(e("mid", SignalKind::Confirmed, 2, "2026-06-01T00:00:00Z"));
        compact(&mut c, &CompactOptions::default(), now());
        let ids: Vec<&str> = c.entries.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, vec!["high", "mid", "low"]);
    }

    #[test]
    fn compact_truncates_to_cap() {
        let mut c = LearningCorpus::empty("x/y");
        for i in 0..150 {
            c.entries.push(e(
                &format!("e{i:03}"),
                SignalKind::Confirmed,
                1,
                "2026-06-01T00:00:00Z",
            ));
        }
        let opts = CompactOptions {
            max_entries: 50,
            ..CompactOptions::default()
        };
        let evicted = compact(&mut c, &opts, now());
        assert_eq!(c.entries.len(), 50);
        assert_eq!(evicted, 100);
    }

    #[test]
    fn compact_writes_scores_back_for_observability() {
        let mut c = LearningCorpus::empty("x/y");
        c.entries
            .push(e("x", SignalKind::UserDirective, 2, "2026-06-01T00:00:00Z"));
        compact(&mut c, &CompactOptions::default(), now());
        assert!(c.entries[0].score > 0.0);
    }
}
