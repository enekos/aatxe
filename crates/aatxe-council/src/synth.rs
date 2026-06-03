//! Deterministic synthesis: dedup similar findings across proposers,
//! normalise severity, rank.
//!
//! Two findings are considered duplicates when (a) they refer to the same
//! file, (b) their line numbers are within a small window (default 3), and
//! (c) their titles share a Jaccard similarity above a threshold over the
//! lowercase token set. On a duplicate, the survivor takes the *higher*
//! severity, and its `raised_by` field is overwritten with a `+`-joined
//! list — overlap across personas is signal, not noise.

use crate::types::Finding;

#[derive(Debug, Clone, Copy)]
pub struct SynthOptions {
    /// Jaccard similarity over the lowercase token set of the title at or
    /// above which two findings are considered duplicates. `[0, 1]`.
    /// Default 0.55 — empirically chosen so trivially-similar phrasings
    /// merge but distinct issues stay split.
    pub title_similarity: f64,
    /// Two findings on the same file within `±line_window` lines are
    /// merge candidates (if titles also match). `0` means exact line.
    pub line_window: u32,
}

impl Default for SynthOptions {
    fn default() -> Self {
        Self {
            title_similarity: 0.55,
            line_window: 3,
        }
    }
}

/// Dedup-and-rank pipeline applied to the raw union of proposer findings.
///
/// 1. Pairwise dedup: two findings on the same file within `line_window`
///    lines whose titles cross the `title_similarity` threshold are
///    merged into one (severity = max, raised_by = both).
/// 2. Sort: severity desc, then file asc, then line asc, then title asc —
///    deterministic for tests and for human review.
pub fn dedup_and_rank(findings: Vec<Finding>, opts: SynthOptions) -> Vec<Finding> {
    let mut merged: Vec<Finding> = Vec::with_capacity(findings.len());
    for f in findings {
        if let Some(target) = merged.iter_mut().find(|m| is_duplicate(m, &f, opts)) {
            if f.severity > target.severity {
                target.severity = f.severity;
            }
            target.raised_by = match (&target.raised_by, &f.raised_by) {
                (Some(a), Some(b)) if !a.split('+').any(|t| t == b) => Some(format!("{a}+{b}")),
                (Some(a), _) => Some(a.clone()),
                (None, Some(b)) => Some(b.clone()),
                (None, None) => None,
            };
            // Prefer the longer rationale (more information).
            if f.rationale.len() > target.rationale.len() {
                target.rationale = f.rationale;
            }
            if target.suggestion.is_none() && f.suggestion.is_some() {
                target.suggestion = f.suggestion;
            }
            continue;
        }
        merged.push(f);
    }

    merged.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(a.file.cmp(&b.file))
            .then(a.line.unwrap_or(0).cmp(&b.line.unwrap_or(0)))
            .then(a.title.cmp(&b.title))
    });
    merged
}

fn is_duplicate(a: &Finding, b: &Finding, opts: SynthOptions) -> bool {
    if a.file != b.file {
        return false;
    }
    let line_ok = match (a.line, b.line) {
        (Some(la), Some(lb)) => la.abs_diff(lb) <= opts.line_window,
        // If either side is missing a line, fall back to title similarity
        // alone (whole-file findings).
        _ => true,
    };
    if !line_ok {
        return false;
    }
    title_jaccard(&a.title, &b.title) >= opts.title_similarity
}

/// Tokenise a title into a reusable buffer.  Returns the number of tokens.
fn tokenise<'a>(s: &'a str, buf: &mut Vec<&'a str>) -> usize {
    buf.clear();
    buf.extend(
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty()),
    );
    buf.sort_unstable();
    buf.dedup();
    buf.len()
}

fn title_jaccard(a: &str, b: &str) -> f64 {
    let mut buf_a: Vec<&str> = Vec::with_capacity(16);
    let mut buf_b: Vec<&str> = Vec::with_capacity(16);
    let n_a = tokenise(a, &mut buf_a);
    let n_b = tokenise(b, &mut buf_b);
    if n_a == 0 && n_b == 0 {
        return 1.0;
    }
    let mut i = 0;
    let mut j = 0;
    let mut inter = 0usize;
    while i < n_a && j < n_b {
        match buf_a[i].cmp(buf_b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
        }
    }
    let union = n_a + n_b - inter;
    if union == 0 {
        return 0.0;
    }
    inter as f64 / union as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FindingCategory, Severity};

    fn mk(
        file: &str,
        line: Option<u32>,
        sev: Severity,
        cat: FindingCategory,
        title: &str,
        by: &str,
    ) -> Finding {
        Finding {
            file: file.into(),
            line,
            severity: sev,
            category: cat,
            title: title.into(),
            rationale: "r".into(),
            suggestion: None,
            raised_by: Some(by.into()),
        }
    }

    #[test]
    fn merges_near_duplicate_findings_taking_max_severity() {
        let a = mk(
            "src/a.rs",
            Some(10),
            Severity::Minor,
            FindingCategory::Correctness,
            "unwrap on None will panic",
            "correctness",
        );
        let b = mk(
            "src/a.rs",
            Some(12),
            Severity::Major,
            FindingCategory::Security,
            "unwrap on None panic",
            "security",
        );
        let merged = dedup_and_rank(vec![a, b], SynthOptions::default());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].severity, Severity::Major);
        let by = merged[0].raised_by.as_deref().unwrap();
        assert!(by.contains("correctness"));
        assert!(by.contains("security"));
    }

    #[test]
    fn distinct_files_never_merge() {
        let a = mk(
            "src/a.rs",
            Some(10),
            Severity::Major,
            FindingCategory::Correctness,
            "foo",
            "x",
        );
        let b = mk(
            "src/b.rs",
            Some(10),
            Severity::Major,
            FindingCategory::Correctness,
            "foo",
            "y",
        );
        let merged = dedup_and_rank(vec![a, b], SynthOptions::default());
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn distant_lines_never_merge_even_with_same_title() {
        let a = mk(
            "src/a.rs",
            Some(10),
            Severity::Major,
            FindingCategory::Correctness,
            "panic on unwrap",
            "x",
        );
        let b = mk(
            "src/a.rs",
            Some(200),
            Severity::Major,
            FindingCategory::Correctness,
            "panic on unwrap",
            "y",
        );
        let merged = dedup_and_rank(vec![a, b], SynthOptions::default());
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn sort_order_is_severity_desc_then_path() {
        let findings = vec![
            mk(
                "z.rs",
                None,
                Severity::Nit,
                FindingCategory::Maintainability,
                "x",
                "m",
            ),
            mk(
                "a.rs",
                None,
                Severity::Critical,
                FindingCategory::Security,
                "y",
                "s",
            ),
            mk(
                "m.rs",
                None,
                Severity::Major,
                FindingCategory::Correctness,
                "z",
                "c",
            ),
        ];
        let ranked = dedup_and_rank(findings, SynthOptions::default());
        assert_eq!(ranked[0].file, "a.rs");
        assert_eq!(ranked[1].file, "m.rs");
        assert_eq!(ranked[2].file, "z.rs");
    }

    #[test]
    fn whole_file_findings_merge_on_title_alone() {
        // Identical-meaning titles with enough token overlap to cross the
        // default 0.55 Jaccard threshold. No line numbers → file-level
        // findings merge purely on title similarity.
        let a = mk(
            "src/a.rs",
            None,
            Severity::Major,
            FindingCategory::Maintainability,
            "missing tests for new module",
            "m",
        );
        let b = mk(
            "src/a.rs",
            None,
            Severity::Minor,
            FindingCategory::Correctness,
            "missing tests for the new module",
            "c",
        );
        let merged = dedup_and_rank(vec![a, b], SynthOptions::default());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].severity, Severity::Major);
    }
}
