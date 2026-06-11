//! Tolerant JSON response parsing.
//!
//! Even with `response_format: json_object`, LLMs sometimes wrap their
//! answer in a ```json``` fence, prepend a sentence, or trail a comment.
//! The parsing in this module strips all of that before handing to
//! `serde_json`, then falls back to a more lenient extractor that scans
//! for the *first* balanced `{...}` block.

use crate::persona::Persona;
use crate::types::{Finding, FindingCategory, JudgeVerdict, Severity};
use serde::Deserialize;

/// Confidence assigned to a candidate the judge failed to score — because
/// it returned malformed JSON, an empty `verdicts` list, or simply omitted
/// the candidate's index. Deliberately set just ABOVE the default
/// confidence floor (0.55) so the documented "over-include rather than
/// silently drop on judge failure" policy actually holds: a value below
/// the floor (the old 0.5) hid these findings instead of shipping them,
/// which is the opposite of over-including. A keep here is a soft keep —
/// a working judge that genuinely doubts a finding will say so explicitly
/// with a lower number.
pub const JUDGE_FALLBACK_CONFIDENCE: f64 = 0.6;

/// Parse a proposer response into a finding list. The persona is used as a
/// safe default category (the model may omit it).
pub fn parse_findings_json(raw: &str, persona: Persona) -> Vec<Finding> {
    let Some(json) = extract_json_object(raw) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<RawFindingsEnvelope>(&json) else {
        return Vec::new();
    };
    parsed
        .findings
        .into_iter()
        .filter_map(|rf| rf.into_finding(persona))
        .collect()
}

/// Parse the judge's response into per-candidate verdicts. Returns a vector
/// matched against the input candidate list — for any candidate the judge
/// failed to address, we synthesise a `keep` at [`JUDGE_FALLBACK_CONFIDENCE`]
/// so we never silently drop a finding because the judge forgot it.
pub fn parse_judge_verdicts(
    raw: &str,
    candidate_count: usize,
) -> Vec<(JudgeVerdict, f64, Option<String>)> {
    let mut out: Vec<(JudgeVerdict, f64, Option<String>)> =
        vec![(JudgeVerdict::Keep, JUDGE_FALLBACK_CONFIDENCE, None); candidate_count];
    let Some(json) = extract_json_object(raw) else {
        return out;
    };
    let Ok(parsed) = serde_json::from_str::<RawVerdictsEnvelope>(&json) else {
        return out;
    };
    for v in parsed.verdicts {
        let Some(idx) = v.index else { continue };
        if (idx as usize) >= candidate_count {
            continue;
        }
        let verdict = match v
            .verdict
            .as_deref()
            .unwrap_or("keep")
            .to_ascii_lowercase()
            .as_str()
        {
            "drop" | "reject" | "discard" => JudgeVerdict::Drop,
            "downgrade" | "lower" | "soften" => JudgeVerdict::Downgrade,
            _ => JudgeVerdict::Keep,
        };
        let conf = v
            .confidence
            .unwrap_or(JUDGE_FALLBACK_CONFIDENCE)
            .clamp(0.0, 1.0);
        out[idx as usize] = (verdict, conf, v.note);
    }
    out
}

/// Extract a balanced JSON object from a free-form string. Returns the
/// inner JSON or `None` if no balanced object exists. Handles strings and
/// escapes correctly.
fn extract_json_object(raw: &str) -> Option<String> {
    let s = strip_code_fence(raw);
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_code_fence(raw: &str) -> &str {
    let r = raw.trim();
    if let Some(rest) = r.strip_prefix("```json") {
        rest.trim_start_matches('\n').trim_end_matches("```").trim()
    } else if let Some(rest) = r.strip_prefix("```") {
        rest.trim_start_matches('\n').trim_end_matches("```").trim()
    } else {
        r
    }
}

#[derive(Debug, Deserialize)]
struct RawFindingsEnvelope {
    #[serde(default)]
    findings: Vec<RawFinding>,
}

#[derive(Debug, Deserialize)]
struct RawFinding {
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    rationale: Option<String>,
    #[serde(default)]
    suggestion: Option<String>,
}

impl RawFinding {
    fn into_finding(self, persona: Persona) -> Option<Finding> {
        let title = self.title?.trim().to_string();
        if title.is_empty() {
            return None;
        }
        let rationale = self.rationale.unwrap_or_default().trim().to_string();
        let severity = self
            .severity
            .as_deref()
            .map(Severity::parse_lenient)
            .unwrap_or(Severity::Minor);
        let category = self
            .category
            .as_deref()
            .and_then(parse_category)
            .unwrap_or_else(|| persona.category());
        Some(Finding {
            file: self.file.unwrap_or_default(),
            line: self.line,
            severity,
            category,
            title,
            rationale,
            suggestion: self.suggestion.filter(|s| !s.trim().is_empty()),
            raised_by: Some(persona.label().to_string()),
        })
    }
}

fn parse_category(s: &str) -> Option<FindingCategory> {
    match s.trim().to_ascii_lowercase().as_str() {
        "correctness" | "logic" | "bug" => Some(FindingCategory::Correctness),
        "security" | "sec" => Some(FindingCategory::Security),
        "performance" | "perf" | "speed" => Some(FindingCategory::Performance),
        "maintainability" | "style" | "quality" | "readability" => {
            Some(FindingCategory::Maintainability)
        }
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct RawVerdictsEnvelope {
    #[serde(default)]
    verdicts: Vec<RawVerdict>,
}

#[derive(Debug, Deserialize)]
struct RawVerdict {
    #[serde(default)]
    index: Option<u32>,
    #[serde(default)]
    verdict: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let raw = r#"{"findings": [
            {"file":"src/x.rs","line":10,"severity":"major","category":"correctness","title":"panic","rationale":"unwrap on None","suggestion":"use match"}
        ]}"#;
        let f = parse_findings_json(raw, Persona::Correctness);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].file, "src/x.rs");
        assert_eq!(f[0].line, Some(10));
        assert_eq!(f[0].severity, Severity::Major);
        assert_eq!(f[0].suggestion.as_deref(), Some("use match"));
        assert_eq!(f[0].raised_by.as_deref(), Some("correctness"));
    }

    #[test]
    fn strips_code_fence_wrapper() {
        let raw = "```json\n{\"findings\": [{\"title\":\"t\",\"severity\":\"nit\",\"rationale\":\"r\"}]}\n```";
        let f = parse_findings_json(raw, Persona::Maintainability);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Nit);
        // No category supplied → falls back to persona
        assert_eq!(f[0].category, FindingCategory::Maintainability);
    }

    #[test]
    fn tolerates_prose_prefix() {
        let raw = "Sure, here are my findings:\n{\"findings\": []}\nLet me know if you need more.";
        let f = parse_findings_json(raw, Persona::Security);
        assert!(f.is_empty());
    }

    #[test]
    fn drops_unparseable_input() {
        assert!(parse_findings_json("not json at all", Persona::Correctness).is_empty());
        assert!(parse_findings_json("{this is broken", Persona::Correctness).is_empty());
    }

    #[test]
    fn drops_findings_without_title() {
        let raw = r#"{"findings": [{"severity":"minor","rationale":"r"}]}"#;
        let f = parse_findings_json(raw, Persona::Correctness);
        assert!(f.is_empty());
    }

    #[test]
    fn unknown_severity_falls_back_to_minor() {
        let raw = r#"{"findings": [{"title":"t","severity":"OMEGA","rationale":"r"}]}"#;
        let f = parse_findings_json(raw, Persona::Correctness);
        assert_eq!(f[0].severity, Severity::Minor);
    }

    #[test]
    fn judge_verdicts_align_by_index_and_default_keep() {
        let raw = r#"{"verdicts": [
            {"index": 0, "verdict": "drop", "confidence": 0.9},
            {"index": 2, "verdict": "downgrade", "confidence": 0.7, "note": "speculative"}
        ]}"#;
        let v = parse_judge_verdicts(raw, 3);
        assert_eq!(v[0].0, JudgeVerdict::Drop);
        assert!((v[0].1 - 0.9).abs() < 1e-9);
        assert_eq!(v[1].0, JudgeVerdict::Keep);
        assert_eq!(v[1].1, JUDGE_FALLBACK_CONFIDENCE);
        assert_eq!(v[2].0, JudgeVerdict::Downgrade);
        assert_eq!(v[2].2.as_deref(), Some("speculative"));
    }

    #[test]
    fn judge_verdicts_ignore_out_of_range_index() {
        let raw = r#"{"verdicts": [{"index": 99, "verdict": "drop", "confidence": 1.0}]}"#;
        let v = parse_judge_verdicts(raw, 2);
        // All slots stayed at default
        for slot in v {
            assert_eq!(slot.0, JudgeVerdict::Keep);
            assert_eq!(slot.1, JUDGE_FALLBACK_CONFIDENCE);
        }
    }
}
