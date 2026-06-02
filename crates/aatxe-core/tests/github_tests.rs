//! Sticky-comment helpers: no HTTP, just URL + header shape + sticky guard.

use aatxe_core::github::{
    create_comment_url, default_headers, detect_context, list_comments_url, patch_comment_url,
    validate_sticky, GithubContext,
};
use aatxe_core::report::STICKY_MARKER;
use aatxe_core::secret::Secret;
use std::collections::HashMap;

fn ctx() -> GithubContext {
    GithubContext {
        repo: "owner/name".into(),
        pr: 42,
        token: Secret::new("ghp_xxx"),
        api_base: None,
    }
}

#[test]
fn url_helpers_are_correct() {
    let c = ctx();
    assert_eq!(
        list_comments_url(&c, 1),
        "https://api.github.com/repos/owner/name/issues/42/comments?per_page=100&page=1"
    );
    assert_eq!(
        patch_comment_url(&c, 999),
        "https://api.github.com/repos/owner/name/issues/comments/999"
    );
    assert_eq!(
        create_comment_url(&c),
        "https://api.github.com/repos/owner/name/issues/42/comments"
    );
}

#[test]
fn custom_api_base_is_honoured() {
    let mut c = ctx();
    c.api_base = Some("https://github.example.com/api/v3".into());
    assert!(create_comment_url(&c).starts_with("https://github.example.com/api/v3"));
}

#[test]
fn default_headers_set_bearer_and_user_agent() {
    let h = default_headers(&Secret::new("abc"));
    let map: HashMap<&str, String> = h.into_iter().collect();
    assert_eq!(map["Authorization"], "Bearer abc");
    assert_eq!(map["Accept"], "application/vnd.github+json");
    assert!(map["User-Agent"].starts_with("aatxe/"));
}

#[test]
fn github_context_debug_redacts_token() {
    let printed = format!("{:?}", ctx());
    assert!(
        !printed.contains("ghp_xxx"),
        "token must not appear in Debug output: {printed}"
    );
    assert!(printed.contains("Secret(***)"));
}

#[test]
fn validate_sticky_requires_marker() {
    assert!(validate_sticky(&format!("{STICKY_MARKER}\nbody")).is_ok());
    assert!(validate_sticky("no marker here").is_err());
}

#[test]
fn detect_context_reads_actions_env() {
    let env: HashMap<&str, &str> = [
        ("GITHUB_TOKEN", "secret"),
        ("GITHUB_REPOSITORY", "o/r"),
        ("GITHUB_REF", "refs/pull/123/merge"),
    ]
    .into_iter()
    .collect();
    let detected = detect_context(|k| env.get(k).map(|v| v.to_string()));
    assert_eq!(detected.repo.as_deref(), Some("o/r"));
    assert_eq!(detected.pr, Some(123));
    assert_eq!(detected.token.as_ref().map(Secret::reveal), Some("secret"));
}

#[test]
fn detect_context_falls_back_to_ref_name() {
    let env: HashMap<&str, &str> = [("GITHUB_REF_NAME", "55/merge"), ("GH_TOKEN", "tok")]
        .into_iter()
        .collect();
    let d = detect_context(|k| env.get(k).map(|v| v.to_string()));
    assert_eq!(d.pr, Some(55));
    assert_eq!(d.token.as_ref().map(Secret::reveal), Some("tok"));
}

#[test]
fn detect_context_empty_env_returns_nothing() {
    let d = detect_context(|_| None);
    assert_eq!(d.repo, None);
    assert_eq!(d.pr, None);
    assert!(d.token.is_none());
}

#[test]
fn explicit_aatxe_pr_overrides_other_sources() {
    // Even with a GITHUB_REF pointing at #123, AATXE_PR wins.
    let env: HashMap<&str, &str> = [("AATXE_PR", "999"), ("GITHUB_REF", "refs/pull/123/merge")]
        .into_iter()
        .collect();
    let d = detect_context(|k| env.get(k).map(|v| v.to_string()));
    assert_eq!(d.pr, Some(999), "AATXE_PR must take precedence");
}

#[test]
fn detect_context_ignores_non_pr_refs() {
    // Push to master / tag refs / commit refs shouldn't yield a PR number.
    for value in ["refs/heads/master", "refs/tags/v1.0.0", "deadbeef"] {
        let env: HashMap<&str, &str> = [("GITHUB_REF", value)].into_iter().collect();
        let d = detect_context(|k| env.get(k).map(|v| v.to_string()));
        assert_eq!(d.pr, None, "GITHUB_REF={value} should not yield a PR");
    }
}

#[test]
fn detect_context_rejects_invalid_aatxe_pr() {
    // Non-numeric / zero / negative values must be ignored, not crashed on.
    for value in ["abc", "0", "-1", ""] {
        let env: HashMap<&str, &str> = [("AATXE_PR", value)].into_iter().collect();
        let d = detect_context(|k| env.get(k).map(|v| v.to_string()));
        assert_eq!(d.pr, None, "AATXE_PR={value:?} should be ignored");
    }
}

#[test]
fn token_falls_back_to_gh_token_when_github_token_absent() {
    let env: HashMap<&str, &str> = [("GH_TOKEN", "fallback-token")].into_iter().collect();
    let d = detect_context(|k| env.get(k).map(|v| v.to_string()));
    assert_eq!(d.token.as_ref().map(Secret::reveal), Some("fallback-token"));
}

#[test]
fn list_comments_url_increments_page() {
    let c = ctx();
    let p1 = list_comments_url(&c, 1);
    let p2 = list_comments_url(&c, 2);
    assert!(p1.ends_with("page=1"));
    assert!(p2.ends_with("page=2"));
}
