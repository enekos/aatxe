//! Shared data types for the learning corpus.
//!
//! The on-disk shape is the single JSON file persisted as a GitHub Actions
//! artifact between council runs. Every field that could plausibly drift
//! between schema versions is `#[serde(default)]` so older corpora keep
//! loading after the schema evolves.

use aatxe_council::types::FindingCategory;
use serde::{Deserialize, Serialize};

/// Bumped whenever the on-disk shape changes incompatibly. The loader
/// upgrades older shapes when possible; unrecognised future versions are
/// rejected so we don't silently mis-interpret data.
pub const CORPUS_SCHEMA_VERSION: u32 = 1;

/// Hard cap on the rendered length of a single guidance string, in chars.
/// The corpus content flows verbatim into LLM system prompts, so unbounded
/// length is a prompt-injection / cost-blowup vector — an attacker who can
/// post a comment could otherwise stash MB-scale instructions. 2000 chars
/// is comfortably above the longest legitimate `aatxe: remember <…>` we
/// expect (typically 1–2 sentences) while keeping any single entry under
/// ~500 tokens.
pub const MAX_GUIDANCE_LEN: usize = 2000;

/// Truncate `s` to at most [`MAX_GUIDANCE_LEN`] chars on a UTF-8 boundary,
/// appending a one-char ellipsis when truncated. Idempotent on already-short
/// inputs.
pub fn clamp_guidance(s: &str) -> String {
    if s.chars().count() <= MAX_GUIDANCE_LEN {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_GUIDANCE_LEN - 1).collect();
    out.push('…');
    out
}

/// Where the signal came from. Order matters — the score function weights
/// these by source authority. Explicit user directives are the strongest
/// signal; inferred-from-merge is the weakest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignalKind {
    /// Reviewer wrote `aatxe: remember <…>` (or `@aatxe remember <…>`) in a
    /// PR comment — the highest-authority signal a human can give us.
    UserDirective,
    /// Reviewer confirmed the finding was actionable (👍 reaction, or
    /// `aatxe: good catch on N`).
    Confirmed,
    /// Reviewer marked the finding as a false positive (👎 reaction, or
    /// `aatxe: false-positive on N`). Refutations *don't* delete the entry
    /// — they decrement its score so a sustained pattern of refutations
    /// eventually evicts it.
    Refuted,
    /// Inferred — the PR merged with changes touching the file/line the
    /// council flagged, so the finding was at least *engaged with*.
    InferredApplied,
    /// Inferred — the PR merged without touching the flagged file/line,
    /// so the finding was probably ignored.
    InferredIgnored,
}

impl SignalKind {
    pub fn label(self) -> &'static str {
        match self {
            SignalKind::UserDirective => "user-directive",
            SignalKind::Confirmed => "confirmed",
            SignalKind::Refuted => "refuted",
            SignalKind::InferredApplied => "inferred-applied",
            SignalKind::InferredIgnored => "inferred-ignored",
        }
    }

    /// Whether this kind of signal is *positive* (raises the score) or
    /// *negative* (lowers it).
    pub fn polarity(self) -> i8 {
        match self {
            SignalKind::UserDirective => 2,
            SignalKind::Confirmed => 1,
            SignalKind::InferredApplied => 1,
            SignalKind::Refuted => -2,
            SignalKind::InferredIgnored => -1,
        }
    }
}

/// One distilled piece of guidance the council should remember.
///
/// Entries are *not* findings — they are higher-level lessons:
/// `"In src/auth/**, password-in-log findings have been consistently
///   confirmed by reviewers"` is one entry; the dozen actual findings
/// that fed it are not.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LearnedEntry {
    /// Stable opaque id. Derived from `(file_glob, category, normalised
    /// title)` so dedup is content-addressable and survives reorderings.
    pub id: String,

    /// Plain-text guidance the council should keep in mind. Will be
    /// rendered verbatim into the proposer/judge system prompts.
    pub guidance: String,

    /// Optional repo-relative path glob this guidance applies to. `None`
    /// means it applies everywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_glob: Option<String>,

    /// Optional category this guidance applies to. `None` means it applies
    /// to every persona.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<FindingCategory>,

    /// Strongest signal that's ever applied to this entry. Used for the
    /// score function — explicit user directives outweigh inferred
    /// signals even if the inferred count is higher.
    pub strongest_kind: SignalKind,

    /// PR + comment where the entry was first observed. Lets reviewers
    /// trace a piece of guidance back to its source.
    pub source_pr: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_comment_id: Option<u64>,

    /// Running counters. `confirmations` includes every positive-polarity
    /// signal; `refutations` every negative one.
    #[serde(default)]
    pub confirmations: u32,
    #[serde(default)]
    pub refutations: u32,

    /// ISO-8601 UTC timestamps. `first_seen` is immutable after creation;
    /// `last_seen` is updated whenever the entry is reinforced.
    pub first_seen: String,
    pub last_seen: String,

    /// Recomputed by [`crate::score::score_entry`] at compaction time so
    /// downstream tools can read the score without rerunning the
    /// algorithm. Persisted purely for observability — the loader
    /// recomputes on demand.
    #[serde(default)]
    pub score: f64,
}

/// Top-level corpus shape. One per repo; one artifact uploaded per
/// workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearningCorpus {
    pub schema_version: u32,
    /// `owner/name`. Empty when the corpus is bootstrapping.
    #[serde(default)]
    pub repo: String,
    /// ISO-8601 UTC timestamp the corpus was last written.
    #[serde(default)]
    pub updated_at: String,
    /// The body of the corpus — bounded by [`crate::compact::CompactOptions::max_entries`].
    #[serde(default)]
    pub entries: Vec<LearnedEntry>,
    /// One-line counters surfaced by the last self-healing load. Not
    /// load-bearing; surfaced in the rendered corpus summary so a
    /// reviewer can spot decay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_load_summary: Option<crate::load::LoadSummary>,
}

impl LearningCorpus {
    /// Fresh empty corpus for the given repo. The "self-healing" base case.
    pub fn empty(repo: impl Into<String>) -> Self {
        Self {
            schema_version: CORPUS_SCHEMA_VERSION,
            repo: repo.into(),
            updated_at: now_iso8601(),
            entries: Vec::new(),
            last_load_summary: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Stable id for an entry — deterministic across runs so dedup works.
///
/// Uses a small displaceable hash over the normalised inputs. We don't
/// need cryptographic strength; we need stability + collision resistance
/// at corpus-size scale (≤ a few hundred entries).
pub fn entry_id(
    file_glob: Option<&str>,
    category: Option<FindingCategory>,
    guidance: &str,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    file_glob.unwrap_or("").hash(&mut h);
    category.map(|c| c.label()).unwrap_or("").hash(&mut h);
    // Normalise the guidance: collapse whitespace, lowercase. Same wording
    // with different spacing should land on the same id.
    let normalised: String = guidance
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    normalised.hash(&mut h);
    format!("e{:016x}", h.finish())
}

pub(crate) fn now_iso8601() -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_id_stable_under_whitespace_and_case_changes() {
        let a = entry_id(
            Some("src/**"),
            Some(FindingCategory::Security),
            "Log secrets are bad",
        );
        let b = entry_id(
            Some("src/**"),
            Some(FindingCategory::Security),
            "log    secrets are bad",
        );
        let c = entry_id(
            Some("src/**"),
            Some(FindingCategory::Security),
            "LOG SECRETS ARE BAD",
        );
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn entry_id_differs_when_category_differs() {
        let a = entry_id(None, Some(FindingCategory::Security), "x");
        let b = entry_id(None, Some(FindingCategory::Correctness), "x");
        assert_ne!(a, b);
    }

    #[test]
    fn entry_id_differs_when_glob_differs() {
        let a = entry_id(Some("src/**"), None, "x");
        let b = entry_id(Some("tests/**"), None, "x");
        assert_ne!(a, b);
    }

    #[test]
    fn signal_polarity_ordered() {
        assert!(SignalKind::UserDirective.polarity() > SignalKind::Confirmed.polarity());
        assert!(SignalKind::Confirmed.polarity() > 0);
        assert!(SignalKind::InferredApplied.polarity() > 0);
        assert!(SignalKind::Refuted.polarity() < SignalKind::InferredIgnored.polarity());
    }

    #[test]
    fn corpus_empty_is_clean() {
        let c = LearningCorpus::empty("x/y");
        assert!(c.is_empty());
        assert_eq!(c.schema_version, CORPUS_SCHEMA_VERSION);
        assert_eq!(c.repo, "x/y");
        assert!(!c.updated_at.is_empty());
    }

    #[test]
    fn clamp_guidance_passes_short_strings_unchanged() {
        let s = "short guidance";
        assert_eq!(clamp_guidance(s), s);
    }

    #[test]
    fn clamp_guidance_truncates_oversized_with_ellipsis() {
        let long = "a".repeat(MAX_GUIDANCE_LEN + 100);
        let out = clamp_guidance(&long);
        assert_eq!(out.chars().count(), MAX_GUIDANCE_LEN);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn clamp_guidance_handles_multibyte_boundaries() {
        // 4-byte chars; ensure truncation lands on a char boundary.
        let long: String = "🦀".repeat(MAX_GUIDANCE_LEN + 10);
        let out = clamp_guidance(&long);
        assert_eq!(out.chars().count(), MAX_GUIDANCE_LEN);
        assert!(out.ends_with('…'));
    }
}
