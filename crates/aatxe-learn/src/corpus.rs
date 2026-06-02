//! Corpus mutation primitives: merging freshly-harvested entries into the
//! existing corpus.
//!
//! [`merge_entries`] is the *only* way new entries should enter the
//! corpus. It dedupes by [`LearnedEntry::id`] — the content-addressable
//! hash from [`crate::types::entry_id`] — so re-harvesting the same PR
//! doesn't double-count reactions.

use crate::types::{LearnedEntry, LearningCorpus, SignalKind};

/// Counters returned by [`merge_entries`] so the CLI can log a one-liner.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MergeStats {
    pub added: usize,
    pub updated: usize,
}

/// Merge a freshly-harvested batch into `corpus`. Existing entries (by id)
/// are *updated*: confirmations/refutations accumulate, `last_seen` is
/// bumped forward (never backwards), `strongest_kind` is promoted to the
/// higher polarity of the two.
pub fn merge_entries(corpus: &mut LearningCorpus, new: Vec<LearnedEntry>) -> MergeStats {
    let mut stats = MergeStats::default();
    for incoming in new {
        if let Some(existing) = corpus.entries.iter_mut().find(|e| e.id == incoming.id) {
            existing.confirmations = existing
                .confirmations
                .saturating_add(incoming.confirmations);
            existing.refutations = existing.refutations.saturating_add(incoming.refutations);
            existing.strongest_kind = stronger(existing.strongest_kind, incoming.strongest_kind);
            if incoming.last_seen > existing.last_seen {
                existing.last_seen = incoming.last_seen;
            }
            if existing.guidance.trim().is_empty() && !incoming.guidance.trim().is_empty() {
                existing.guidance = incoming.guidance;
            }
            if existing.source_comment_id.is_none() {
                existing.source_comment_id = incoming.source_comment_id;
            }
            stats.updated += 1;
        } else {
            corpus.entries.push(incoming);
            stats.added += 1;
        }
    }
    stats
}

/// Pick the higher-polarity (more authoritative) of two signal kinds.
/// `UserDirective` always wins; among signals with equal polarity we
/// keep `a` for stability.
fn stronger(a: SignalKind, b: SignalKind) -> SignalKind {
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

fn rank(s: SignalKind) -> u32 {
    match s {
        SignalKind::UserDirective => 100,
        SignalKind::Confirmed => 50,
        SignalKind::InferredApplied => 25,
        // Refuted and InferredIgnored are explicit negative signals. We
        // *don't* promote a negative kind over a positive one even if the
        // refutation count is higher — refutations show up in the counter,
        // not in the kind.
        SignalKind::InferredIgnored => 10,
        SignalKind::Refuted => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::entry_id;
    use aatxe_council::types::FindingCategory;

    fn e(id: &str, kind: SignalKind, confirms: u32, refutes: u32, ts: &str) -> LearnedEntry {
        LearnedEntry {
            id: id.into(),
            guidance: "g".into(),
            file_glob: None,
            category: None,
            strongest_kind: kind,
            source_pr: 1,
            source_comment_id: None,
            confirmations: confirms,
            refutations: refutes,
            first_seen: ts.into(),
            last_seen: ts.into(),
            score: 0.0,
        }
    }

    #[test]
    fn merge_adds_new_entries() {
        let mut c = LearningCorpus::empty("x/y");
        let stats = merge_entries(
            &mut c,
            vec![
                e("a", SignalKind::Confirmed, 1, 0, "2026-06-01T00:00:00Z"),
                e("b", SignalKind::UserDirective, 1, 0, "2026-06-01T00:00:00Z"),
            ],
        );
        assert_eq!(stats.added, 2);
        assert_eq!(stats.updated, 0);
        assert_eq!(c.entries.len(), 2);
    }

    #[test]
    fn merge_updates_accumulates_counters() {
        let mut c = LearningCorpus::empty("x/y");
        merge_entries(
            &mut c,
            vec![e("a", SignalKind::Confirmed, 1, 0, "2026-06-01T00:00:00Z")],
        );
        let stats = merge_entries(
            &mut c,
            vec![e("a", SignalKind::Confirmed, 2, 1, "2026-06-02T00:00:00Z")],
        );
        assert_eq!(stats.added, 0);
        assert_eq!(stats.updated, 1);
        let only = &c.entries[0];
        assert_eq!(only.confirmations, 3);
        assert_eq!(only.refutations, 1);
        assert_eq!(only.last_seen, "2026-06-02T00:00:00Z");
    }

    #[test]
    fn merge_promotes_strongest_kind_upward_only() {
        let mut c = LearningCorpus::empty("x/y");
        merge_entries(
            &mut c,
            vec![e("a", SignalKind::Confirmed, 1, 0, "2026-06-01T00:00:00Z")],
        );
        merge_entries(
            &mut c,
            vec![e(
                "a",
                SignalKind::UserDirective,
                1,
                0,
                "2026-06-02T00:00:00Z",
            )],
        );
        assert_eq!(c.entries[0].strongest_kind, SignalKind::UserDirective);
        // Now refute it — strongest_kind should NOT downgrade.
        merge_entries(
            &mut c,
            vec![e("a", SignalKind::Refuted, 0, 1, "2026-06-03T00:00:00Z")],
        );
        assert_eq!(c.entries[0].strongest_kind, SignalKind::UserDirective);
        assert_eq!(c.entries[0].refutations, 1);
    }

    #[test]
    fn merge_does_not_walk_last_seen_backward() {
        let mut c = LearningCorpus::empty("x/y");
        merge_entries(
            &mut c,
            vec![e("a", SignalKind::Confirmed, 1, 0, "2026-06-05T00:00:00Z")],
        );
        merge_entries(
            &mut c,
            vec![e("a", SignalKind::Confirmed, 1, 0, "2026-06-01T00:00:00Z")],
        );
        assert_eq!(c.entries[0].last_seen, "2026-06-05T00:00:00Z");
    }

    #[test]
    fn merge_dedup_is_content_addressable_via_entry_id() {
        // Build two entries by id() helper; equal inputs → same id.
        let id1 = entry_id(
            Some("src/**"),
            Some(FindingCategory::Security),
            "guidance one",
        );
        let id2 = entry_id(
            Some("src/**"),
            Some(FindingCategory::Security),
            "guidance one",
        );
        assert_eq!(id1, id2);
        let mut c = LearningCorpus::empty("x/y");
        merge_entries(
            &mut c,
            vec![e(&id1, SignalKind::Confirmed, 1, 0, "2026-06-01T00:00:00Z")],
        );
        merge_entries(
            &mut c,
            vec![e(&id2, SignalKind::Confirmed, 1, 0, "2026-06-02T00:00:00Z")],
        );
        assert_eq!(c.entries.len(), 1, "ids matched → must be one entry");
        assert_eq!(c.entries[0].confirmations, 2);
    }
}
