//! Self-healing corpus loader.
//!
//! The single load entry point is [`load_self_healing`]. It accepts any
//! string (including empty) and *always* returns a usable corpus. The
//! contract:
//!
//! 1. Empty / whitespace input → fresh empty corpus.
//! 2. Malformed top-level JSON → fresh empty corpus + `corpus_was_invalid:
//!    true` in the summary.
//! 3. Valid top-level JSON, malformed individual entries → those entries
//!    are dropped, the rest survives; `entries_dropped_unparseable` counts.
//! 4. Older but still recognised `schema_version` → upgrade in place;
//!    `schema_upgraded_from` records the previous version.
//! 5. Newer-than-known `schema_version` → refuse the corpus *body* (return
//!    fresh empty) but record the future version in the summary so the
//!    workflow can surface a warning instead of silently downgrading.
//!
//! Every failure mode produces a usable corpus + a non-fatal counter in
//! [`LoadSummary`] — never an `Err`.

use crate::types::{
    clamp_guidance, entry_id, LearnedEntry, LearningCorpus, SignalKind, CORPUS_SCHEMA_VERSION,
};
use aatxe_council::types::FindingCategory;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Surfaced after every load. Always populated, even on the happy path
/// (counters are zero).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoadSummary {
    /// Number of entries that loaded cleanly.
    pub entries_loaded: u32,
    /// Number of entries that were present but unparseable and got
    /// dropped. A non-zero count surfaces a warning in the rendered
    /// corpus summary.
    pub entries_dropped_unparseable: u32,
    /// `Some(v)` when the on-disk schema version was older than
    /// [`CORPUS_SCHEMA_VERSION`] and the loader upgraded entries in place.
    pub schema_upgraded_from: Option<u32>,
    /// True when the top-level JSON failed to parse — the returned corpus
    /// is empty, and the workflow should treat this as "bootstrap from
    /// scratch."
    pub corpus_was_invalid: bool,
    /// `Some(v)` when the on-disk version was newer than this binary
    /// understands. Returned corpus is empty (we refuse to mis-interpret
    /// future data); the workflow should fail-soft and surface the
    /// counter so the binary gets upgraded.
    pub corpus_from_future_version: Option<u32>,
}

/// Load a corpus from a JSON string with full self-healing semantics.
/// `repo` is used to bootstrap a fresh corpus when the input is missing or
/// invalid — it's *not* consulted otherwise.
pub fn load_self_healing(json: &str, repo: &str) -> LearningCorpus {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return LearningCorpus::empty(repo);
    }

    let root: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => {
            return with_summary(
                LearningCorpus::empty(repo),
                LoadSummary {
                    corpus_was_invalid: true,
                    ..Default::default()
                },
            );
        }
    };

    // Distinguish three cases for `schemaVersion`:
    //   - missing entirely → treat as v0 (genuine legacy bootstrap).
    //   - present and a clean u64 within u32::MAX → trust the value.
    //   - present and any other shape (string, float, negative, oversized) →
    //     refuse to interpret; surface as `corpus_was_invalid`. Otherwise
    //     a crafted `"schemaVersion": "9".repeat(big)` would silently fall
    //     back to 0 and bypass the future-version guard.
    let on_disk_version: u32 = match root.get("schemaVersion") {
        // Key absent → genuine legacy bootstrap, upgrade from v0.
        None => 0,
        // Key present and a clean u64 within u32 — trust it.
        Some(v) if v.as_u64().is_some_and(|n| n <= u32::MAX as u64) => v.as_u64().unwrap() as u32,
        // Key present and a clean u64 but oversized — definitely a future
        // / crafted version we cannot upgrade. Surface as future-version
        // rather than invalid so the workflow nudges a binary upgrade
        // instead of silently bootstrapping from scratch.
        Some(v) if v.as_u64().is_some() => {
            return with_summary(
                LearningCorpus::empty(repo),
                LoadSummary {
                    corpus_from_future_version: Some(u32::MAX),
                    ..Default::default()
                },
            );
        }
        // Key present but not a u64 (string, float, negative, object…).
        // Refuse to interpret — otherwise `"schemaVersion": "9999..."`
        // would silently fall back to 0 and bypass the future-version
        // guard, letting an attacker smuggle in a poisoned shape.
        Some(_) => {
            return with_summary(
                LearningCorpus::empty(repo),
                LoadSummary {
                    corpus_was_invalid: true,
                    ..Default::default()
                },
            );
        }
    };

    if on_disk_version > CORPUS_SCHEMA_VERSION {
        return with_summary(
            LearningCorpus::empty(repo),
            LoadSummary {
                corpus_from_future_version: Some(on_disk_version),
                ..Default::default()
            },
        );
    }

    let mut summary = LoadSummary::default();
    if on_disk_version < CORPUS_SCHEMA_VERSION {
        summary.schema_upgraded_from = Some(on_disk_version);
    }

    // Pull header fields with sensible defaults.
    let stored_repo = root
        .get("repo")
        .and_then(|v| v.as_str())
        .unwrap_or(repo)
        .to_string();
    let updated_at = root
        .get("updatedAt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut entries: Vec<LearnedEntry> = Vec::new();
    if let Some(arr) = root.get("entries").and_then(|v| v.as_array()) {
        for raw in arr {
            match parse_one_entry(raw) {
                Some(e) => entries.push(e),
                None => summary.entries_dropped_unparseable += 1,
            }
        }
    }
    summary.entries_loaded = entries.len() as u32;

    LearningCorpus {
        schema_version: CORPUS_SCHEMA_VERSION,
        repo: stored_repo,
        updated_at,
        entries,
        last_load_summary: Some(summary),
    }
}

fn with_summary(mut c: LearningCorpus, s: LoadSummary) -> LearningCorpus {
    c.last_load_summary = Some(s);
    c
}

/// Lossy single-entry parse. Required fields with missing/bad values cause
/// the whole entry to be dropped; optional fields fall back to defaults.
fn parse_one_entry(raw: &Value) -> Option<LearnedEntry> {
    // Use a permissive intermediate. We deserialise the well-formed shape
    // first, and only fall back to manual extraction when that fails so we
    // tolerate stale schemas.
    if let Ok(mut parsed) = serde_json::from_value::<EntryV1>(raw.clone()) {
        // The strict parse accepts any string for guidance; we additionally
        // reject empty / whitespace-only guidance because such entries have
        // no actionable content for the council to inject. We also clamp
        // length so a hand-edited or replayed corpus cannot smuggle in
        // oversized guidance past the harvest-time cap.
        if parsed.guidance.trim().is_empty() {
            return None;
        }
        parsed.guidance = clamp_guidance(&parsed.guidance);
        return Some(parsed.into_canonical());
    }

    // Fallback: manual best-effort. Drop the entry if guidance is missing
    // or empty (no recoverable content) or `firstSeen` is missing.
    let raw_guidance = raw.get("guidance").and_then(|v| v.as_str())?.to_string();
    if raw_guidance.trim().is_empty() {
        return None;
    }
    let guidance = clamp_guidance(&raw_guidance);
    let file_glob = raw
        .get("fileGlob")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let category = raw
        .get("category")
        .and_then(|v| v.as_str())
        .and_then(parse_category_lenient);
    let strongest_kind = raw
        .get("strongestKind")
        .and_then(|v| v.as_str())
        .and_then(parse_signal_lenient)
        .unwrap_or(SignalKind::Confirmed);
    let source_pr = raw.get("sourcePr").and_then(|v| v.as_u64()).unwrap_or(0);
    let source_comment_id = raw.get("sourceCommentId").and_then(|v| v.as_u64());
    let confirmations = raw
        .get("confirmations")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let refutations = raw.get("refutations").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let first_seen = raw
        .get("firstSeen")
        .and_then(|v| v.as_str())
        .map(str::to_string)?;
    let last_seen = raw
        .get("lastSeen")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| first_seen.clone());
    let id = raw
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| entry_id(file_glob.as_deref(), category, &guidance));
    let score = raw.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);

    Some(LearnedEntry {
        id,
        guidance,
        file_glob,
        category,
        strongest_kind,
        source_pr,
        source_comment_id,
        confirmations,
        refutations,
        first_seen,
        last_seen,
        score,
    })
}

/// Strict v1 entry shape. When `serde_json` accepts this, we use the
/// happy-path parse. Anything that fails falls through to the lossy
/// extractor above.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryV1 {
    id: String,
    guidance: String,
    #[serde(default)]
    file_glob: Option<String>,
    #[serde(default)]
    category: Option<FindingCategory>,
    strongest_kind: SignalKind,
    source_pr: u64,
    #[serde(default)]
    source_comment_id: Option<u64>,
    #[serde(default)]
    confirmations: u32,
    #[serde(default)]
    refutations: u32,
    first_seen: String,
    last_seen: String,
    #[serde(default)]
    score: f64,
}

impl EntryV1 {
    fn into_canonical(self) -> LearnedEntry {
        LearnedEntry {
            id: self.id,
            guidance: self.guidance,
            file_glob: self.file_glob,
            category: self.category,
            strongest_kind: self.strongest_kind,
            source_pr: self.source_pr,
            source_comment_id: self.source_comment_id,
            confirmations: self.confirmations,
            refutations: self.refutations,
            first_seen: self.first_seen,
            last_seen: self.last_seen,
            score: self.score,
        }
    }
}

fn parse_category_lenient(s: &str) -> Option<FindingCategory> {
    match s.trim().to_ascii_lowercase().as_str() {
        "correctness" => Some(FindingCategory::Correctness),
        "security" => Some(FindingCategory::Security),
        "performance" => Some(FindingCategory::Performance),
        "maintainability" => Some(FindingCategory::Maintainability),
        "judge" => Some(FindingCategory::Judge),
        _ => None,
    }
}

fn parse_signal_lenient(s: &str) -> Option<SignalKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "user-directive" | "directive" => Some(SignalKind::UserDirective),
        "confirmed" => Some(SignalKind::Confirmed),
        "refuted" => Some(SignalKind::Refuted),
        "inferred-applied" => Some(SignalKind::InferredApplied),
        "inferred-ignored" => Some(SignalKind::InferredIgnored),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_empty_corpus_no_warnings() {
        let c = load_self_healing("", "x/y");
        assert!(c.is_empty());
        assert_eq!(c.repo, "x/y");
        assert!(c.last_load_summary.is_none());
    }

    #[test]
    fn malformed_json_yields_empty_corpus_with_warning() {
        let c = load_self_healing("{not json", "x/y");
        assert!(c.is_empty());
        let s = c.last_load_summary.unwrap();
        assert!(s.corpus_was_invalid);
        assert_eq!(s.entries_loaded, 0);
    }

    #[test]
    fn future_schema_version_yields_empty_corpus_with_warning() {
        let c = load_self_healing(
            r#"{"schemaVersion": 9999, "repo": "x/y", "entries": []}"#,
            "x/y",
        );
        assert!(c.is_empty());
        assert_eq!(
            c.last_load_summary.unwrap().corpus_from_future_version,
            Some(9999)
        );
    }

    #[test]
    fn valid_entry_round_trips_cleanly() {
        let one = serde_json::json!({
            "schemaVersion": 1,
            "repo": "x/y",
            "updatedAt": "2026-06-02T00:00:00Z",
            "entries": [{
                "id": "abc",
                "guidance": "Avoid panic in handlers",
                "fileGlob": "src/**/*.rs",
                "category": "correctness",
                "strongestKind": "user-directive",
                "sourcePr": 7,
                "sourceCommentId": 12345,
                "confirmations": 3,
                "refutations": 0,
                "firstSeen": "2026-05-30T00:00:00Z",
                "lastSeen": "2026-06-01T00:00:00Z",
                "score": 1.7
            }]
        });
        let c = load_self_healing(&one.to_string(), "x/y");
        assert_eq!(c.entries.len(), 1);
        let e = &c.entries[0];
        assert_eq!(e.id, "abc");
        assert_eq!(e.guidance, "Avoid panic in handlers");
        assert_eq!(e.file_glob.as_deref(), Some("src/**/*.rs"));
        assert_eq!(e.category, Some(FindingCategory::Correctness));
        assert_eq!(e.strongest_kind, SignalKind::UserDirective);
        assert_eq!(e.source_pr, 7);
        assert_eq!(e.source_comment_id, Some(12345));
        assert_eq!(e.confirmations, 3);
        let s = c.last_load_summary.unwrap();
        assert_eq!(s.entries_loaded, 1);
        assert_eq!(s.entries_dropped_unparseable, 0);
    }

    #[test]
    fn malformed_entries_get_dropped_others_survive() {
        // Entry 1 is fine. Entry 2 has a missing `firstSeen`. Entry 3 has
        // an empty `guidance`. Entry 4 is a totally unrelated object.
        let one = serde_json::json!({
            "schemaVersion": 1,
            "repo": "x/y",
            "entries": [
                {
                    "id": "a",
                    "guidance": "good entry",
                    "strongestKind": "confirmed",
                    "sourcePr": 1,
                    "firstSeen": "2026-05-30T00:00:00Z",
                    "lastSeen":  "2026-05-30T00:00:00Z"
                },
                {
                    "id": "b",
                    "guidance": "missing firstSeen",
                    "strongestKind": "confirmed",
                    "sourcePr": 1,
                    "lastSeen":  "2026-05-30T00:00:00Z"
                },
                {
                    "id": "c",
                    "guidance": "   ",
                    "strongestKind": "confirmed",
                    "sourcePr": 1,
                    "firstSeen": "2026-05-30T00:00:00Z",
                    "lastSeen":  "2026-05-30T00:00:00Z"
                },
                42
            ]
        });
        let c = load_self_healing(&one.to_string(), "x/y");
        assert_eq!(c.entries.len(), 1);
        assert_eq!(c.entries[0].guidance, "good entry");
        let s = c.last_load_summary.unwrap();
        assert_eq!(s.entries_loaded, 1);
        assert_eq!(s.entries_dropped_unparseable, 3);
    }

    #[test]
    fn malformed_schema_version_string_yields_invalid_not_upgrade() {
        // Without the explicit guard, `"schemaVersion": "9999..."` falls
        // back to 0 (because `as_u64()` returns None on a string) and
        // takes the "upgrade from v0" path, bypassing the future-version
        // check. The hardened loader must surface this as invalid.
        let c = load_self_healing(
            r#"{"schemaVersion": "9999999999999999999", "repo": "x/y", "entries": []}"#,
            "x/y",
        );
        assert!(c.is_empty());
        let s = c.last_load_summary.unwrap();
        assert!(
            s.corpus_was_invalid,
            "non-integer schemaVersion must be rejected"
        );
        assert!(s.schema_upgraded_from.is_none());
        assert!(s.corpus_from_future_version.is_none());
    }

    #[test]
    fn schema_version_above_u32_max_treated_as_future() {
        // u64 value too big for u32. Must be surfaced as future-version so
        // the workflow nudges a binary upgrade instead of bootstrapping.
        let c = load_self_healing(
            r#"{"schemaVersion": 5000000000, "repo": "x/y", "entries": []}"#,
            "x/y",
        );
        assert!(c.is_empty());
        let s = c.last_load_summary.unwrap();
        assert_eq!(s.corpus_from_future_version, Some(u32::MAX));
    }

    #[test]
    fn schema_version_negative_yields_invalid() {
        let c = load_self_healing(
            r#"{"schemaVersion": -1, "repo": "x/y", "entries": []}"#,
            "x/y",
        );
        assert!(c.is_empty());
        assert!(c.last_load_summary.unwrap().corpus_was_invalid);
    }

    #[test]
    fn schema_version_object_yields_invalid() {
        let c = load_self_healing(
            r#"{"schemaVersion": {"v": 1}, "repo": "x/y", "entries": []}"#,
            "x/y",
        );
        assert!(c.is_empty());
        assert!(c.last_load_summary.unwrap().corpus_was_invalid);
    }

    #[test]
    fn oversized_guidance_is_clamped_at_load() {
        use crate::types::MAX_GUIDANCE_LEN;
        let huge_guidance = "z".repeat(MAX_GUIDANCE_LEN * 3);
        let one = serde_json::json!({
            "schemaVersion": 1,
            "repo": "x/y",
            "entries": [{
                "id": "abc",
                "guidance": huge_guidance,
                "strongestKind": "user-directive",
                "sourcePr": 7,
                "firstSeen": "2026-05-30T00:00:00Z",
                "lastSeen":  "2026-06-01T00:00:00Z"
            }]
        });
        let c = load_self_healing(&one.to_string(), "x/y");
        assert_eq!(c.entries.len(), 1);
        let g = &c.entries[0].guidance;
        assert_eq!(g.chars().count(), MAX_GUIDANCE_LEN);
        assert!(g.ends_with('…'));
    }

    #[test]
    fn missing_schema_version_treated_as_zero_then_upgraded() {
        let one = serde_json::json!({
            "repo": "x/y",
            "entries": [{
                "id": "a",
                "guidance": "g",
                "strongestKind": "confirmed",
                "sourcePr": 1,
                "firstSeen": "2026-05-30T00:00:00Z",
                "lastSeen":  "2026-05-30T00:00:00Z"
            }]
        });
        let c = load_self_healing(&one.to_string(), "x/y");
        assert_eq!(c.schema_version, CORPUS_SCHEMA_VERSION);
        let s = c.last_load_summary.unwrap();
        assert_eq!(s.schema_upgraded_from, Some(0));
        assert_eq!(s.entries_loaded, 1);
    }
}
