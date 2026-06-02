//! Score function for [`LearnedEntry`].
//!
//! Score is a single floating-point number used by:
//! * [`crate::compact::compact`] to decide which entries to evict when the
//!   corpus exceeds [`crate::compact::CompactOptions::max_entries`].
//! * [`crate::inject::build_guidance`] to rank which entries get included
//!   in the prompt prefix.
//!
//! The function is deliberately simple and explainable — a reviewer
//! looking at a stale entry should be able to predict whether it'll survive
//! the next compaction by reading the entry's counters and timestamps.
//!
//! ```text
//!   score = source_authority(strongest_kind)
//!         + confirmations
//!         - refutations
//!         + (1 if pr_recent_enough else 0)
//!         × recency_decay(last_seen, now)
//! ```
//!
//! Refutations are intentionally amplified — one explicit "false positive"
//! reaction outweighs two "thumbs up" reactions. The bias is towards
//! *precision over recall*: a corpus that confidently asserts the wrong
//! thing pollutes every future review, but a corpus that's missing some
//! true positives just means we don't get a head start on those areas.

use crate::types::{LearnedEntry, SignalKind};
use time::OffsetDateTime;

/// Tunables for the score function. The defaults match the values
/// documented in the module-level comment.
#[derive(Debug, Clone, Copy)]
pub struct ScoringOptions {
    /// Days at which an entry's recency multiplier falls to 0.5. Default
    /// 60. Larger half-life = older entries retain more weight.
    pub recency_half_life_days: f64,
    /// Confirmation weight (positive signals × `confirmation_weight`).
    pub confirmation_weight: f64,
    /// Refutation weight (negative signals × `refutation_weight`). Default
    /// 2.0 — refutations cost twice what confirmations earn.
    pub refutation_weight: f64,
    /// Base score added by the strongest signal source. `UserDirective`
    /// alone is enough to keep an entry alive even with zero
    /// confirmations.
    pub source_authority: SourceAuthority,
}

#[derive(Debug, Clone, Copy)]
pub struct SourceAuthority {
    pub user_directive: f64,
    pub confirmed: f64,
    pub inferred_applied: f64,
    pub refuted: f64,
    pub inferred_ignored: f64,
}

impl Default for SourceAuthority {
    fn default() -> Self {
        Self {
            user_directive: 5.0,
            confirmed: 2.0,
            inferred_applied: 1.0,
            refuted: 0.0,
            inferred_ignored: 0.5,
        }
    }
}

impl Default for ScoringOptions {
    fn default() -> Self {
        Self {
            recency_half_life_days: 60.0,
            confirmation_weight: 1.0,
            refutation_weight: 2.0,
            source_authority: SourceAuthority::default(),
        }
    }
}

/// Compute the score of a single entry. Pure — no IO, no globals.
pub fn score_entry(entry: &LearnedEntry, now: OffsetDateTime, opts: &ScoringOptions) -> f64 {
    let base = match entry.strongest_kind {
        SignalKind::UserDirective => opts.source_authority.user_directive,
        SignalKind::Confirmed => opts.source_authority.confirmed,
        SignalKind::InferredApplied => opts.source_authority.inferred_applied,
        SignalKind::Refuted => opts.source_authority.refuted,
        SignalKind::InferredIgnored => opts.source_authority.inferred_ignored,
    };
    let net_signals = (entry.confirmations as f64) * opts.confirmation_weight
        - (entry.refutations as f64) * opts.refutation_weight;
    let raw = base + net_signals;
    let decay = recency_decay(&entry.last_seen, now, opts.recency_half_life_days);
    // Floor at zero — a never-confirmed-and-twice-refuted entry should
    // sort to the bottom, not go negative and wraparound bizarrely.
    (raw * decay).max(0.0)
}

/// Returns a number in `(0, 1]`. `0.5` at exactly one half-life ago, `1.0`
/// at the present, asymptotically zero as the entry ages.
pub fn recency_decay(last_seen_iso: &str, now: OffsetDateTime, half_life_days: f64) -> f64 {
    use time::format_description::well_known::Rfc3339;
    let last = match OffsetDateTime::parse(last_seen_iso, &Rfc3339) {
        Ok(t) => t,
        // If the timestamp is unparseable we treat it as ancient — equivalent
        // to a corpus that's been left to rot. We don't want to silently
        // grant an unparseable timestamp full weight.
        Err(_) => return 0.1,
    };
    let secs = (now - last).whole_seconds() as f64;
    if secs <= 0.0 {
        return 1.0;
    }
    let days = secs / 86_400.0;
    // Exponential half-life: 0.5^(days / half_life)
    0.5_f64.powf(days / half_life_days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LearnedEntry;

    fn ts(s: &str) -> OffsetDateTime {
        use time::format_description::well_known::Rfc3339;
        OffsetDateTime::parse(s, &Rfc3339).unwrap()
    }

    fn entry(kind: SignalKind, confirms: u32, refutes: u32, last_seen: &str) -> LearnedEntry {
        LearnedEntry {
            id: "x".into(),
            guidance: "g".into(),
            file_glob: None,
            category: None,
            strongest_kind: kind,
            source_pr: 1,
            source_comment_id: None,
            confirmations: confirms,
            refutations: refutes,
            first_seen: last_seen.into(),
            last_seen: last_seen.into(),
            score: 0.0,
        }
    }

    #[test]
    fn user_directive_outscores_inferred() {
        let now = ts("2026-06-02T00:00:00Z");
        let opts = ScoringOptions::default();
        let direct = entry(SignalKind::UserDirective, 0, 0, "2026-06-01T00:00:00Z");
        let inferred = entry(SignalKind::InferredApplied, 0, 0, "2026-06-01T00:00:00Z");
        assert!(score_entry(&direct, now, &opts) > score_entry(&inferred, now, &opts));
    }

    #[test]
    fn refutations_double_count() {
        let now = ts("2026-06-02T00:00:00Z");
        let opts = ScoringOptions::default();
        // 4 confirms - 2 refutes at confirmation_weight=1, refutation_weight=2
        // net signals = 4 - 4 = 0 → only the base survives.
        let mixed = entry(SignalKind::Confirmed, 4, 2, "2026-06-01T00:00:00Z");
        let pure_confirms = entry(SignalKind::Confirmed, 4, 0, "2026-06-01T00:00:00Z");
        assert!(score_entry(&mixed, now, &opts) < score_entry(&pure_confirms, now, &opts));
    }

    #[test]
    fn old_entries_decay() {
        let now = ts("2026-06-02T00:00:00Z");
        let opts = ScoringOptions::default();
        let fresh = entry(SignalKind::Confirmed, 5, 0, "2026-06-01T00:00:00Z");
        // 120 days ago at half-life 60 → 0.25 multiplier.
        let stale = entry(SignalKind::Confirmed, 5, 0, "2026-02-02T00:00:00Z");
        let s_fresh = score_entry(&fresh, now, &opts);
        let s_stale = score_entry(&stale, now, &opts);
        assert!(s_stale < 0.3 * s_fresh);
        // But still positive — the corpus shouldn't *delete* old entries
        // unconditionally; compaction does that based on size pressure.
        assert!(s_stale > 0.0);
    }

    #[test]
    fn unparseable_timestamp_decays_to_floor() {
        let now = ts("2026-06-02T00:00:00Z");
        let opts = ScoringOptions::default();
        let bad = entry(SignalKind::Confirmed, 10, 0, "not-a-timestamp");
        let s = score_entry(&bad, now, &opts);
        // Should still be > 0 but heavily penalised.
        assert!(s > 0.0);
        assert!(s < 5.0);
    }

    #[test]
    fn future_timestamp_clamps_to_present() {
        let now = ts("2026-06-02T00:00:00Z");
        let opts = ScoringOptions::default();
        let future = entry(SignalKind::Confirmed, 1, 0, "2099-01-01T00:00:00Z");
        // Just shouldn't blow up; should be ≥ a same-day entry's score.
        let today = entry(SignalKind::Confirmed, 1, 0, "2026-06-02T00:00:00Z");
        let s_future = score_entry(&future, now, &opts);
        let s_today = score_entry(&today, now, &opts);
        assert!((s_future - s_today).abs() < 1e-9);
    }

    #[test]
    fn score_never_goes_negative() {
        let now = ts("2026-06-02T00:00:00Z");
        let opts = ScoringOptions::default();
        // Heavily refuted, low source authority.
        let e = entry(SignalKind::InferredIgnored, 0, 10, "2026-06-01T00:00:00Z");
        let s = score_entry(&e, now, &opts);
        assert!(s >= 0.0);
    }
}
