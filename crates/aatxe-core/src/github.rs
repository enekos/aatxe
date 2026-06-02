//! Pure helpers for the GitHub sticky-comment protocol.
//!
//! This module does *not* perform network IO — it produces the URLs, headers,
//! and method/body pairs the CLI feeds to its HTTP client. Keeping the
//! protocol pure makes it trivial to unit-test (no mocked HTTP) and easy to
//! swap clients later.
//!
//! The CLI binary wires this up to [`ureq`] (or anything else) in
//! `crates/aatxe/src/github_http.rs`.

use crate::report::STICKY_MARKER;
use crate::secret::Secret;

/// PR context required to find / create / update the sticky comment.
#[derive(Debug, Clone)]
pub struct GithubContext {
    /// Repo in `owner/name` form.
    pub repo: String,
    /// Pull request number.
    pub pr: u64,
    /// GitHub token (PAT or Actions `GITHUB_TOKEN`). Stored as a [`Secret`]
    /// so an accidental `Debug` print of the surrounding struct does not
    /// leak the token.
    pub token: Secret,
    /// Override the GH API base. Defaults to `https://api.github.com`.
    pub api_base: Option<String>,
}

impl GithubContext {
    pub fn api_base(&self) -> &str {
        self.api_base.as_deref().unwrap_or("https://api.github.com")
    }
}

/// Header pairs to send on every GH REST call. The token is revealed from
/// its [`Secret`] wrapper only when the header value is constructed; the
/// returned `Authorization` string is the only place the cleartext lives,
/// and it is consumed by the HTTP client immediately.
pub fn default_headers(token: &Secret) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", format!("Bearer {}", token.reveal())),
        ("Accept", "application/vnd.github+json".to_string()),
        ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ("User-Agent", format!("aatxe/{}", env!("CARGO_PKG_VERSION"))),
    ]
}

/// URL for listing comments on a PR (paged, 100 per page).
pub fn list_comments_url(ctx: &GithubContext, page: u32) -> String {
    format!(
        "{}/repos/{}/issues/{}/comments?per_page=100&page={}",
        ctx.api_base(),
        ctx.repo,
        ctx.pr,
        page,
    )
}

/// URL for updating an issue comment by id.
pub fn patch_comment_url(ctx: &GithubContext, comment_id: u64) -> String {
    format!(
        "{}/repos/{}/issues/comments/{}",
        ctx.api_base(),
        ctx.repo,
        comment_id,
    )
}

/// URL for creating a new issue comment on a PR.
pub fn create_comment_url(ctx: &GithubContext) -> String {
    format!(
        "{}/repos/{}/issues/{}/comments",
        ctx.api_base(),
        ctx.repo,
        ctx.pr,
    )
}

/// Validate that a rendered comment body carries the sticky marker. Returns
/// an [`Err`] without the marker so a caller can refuse to post a body that
/// would create a new comment instead of updating the existing one.
pub fn validate_sticky(body: &str) -> Result<(), &'static str> {
    if body.contains(STICKY_MARKER) {
        Ok(())
    } else {
        Err("comment body is missing the sticky marker; render with crate::render_markdown")
    }
}

/// Resolve PR number and repo slug from common GH Actions / generic CI env
/// vars. Returns `None` for fields not present so the caller can decide what
/// to do (e.g. fall back to CLI flags).
#[derive(Debug, Clone, Default)]
pub struct DetectedContext {
    pub repo: Option<String>,
    pub pr: Option<u64>,
    pub token: Option<Secret>,
}

/// Read GH Actions / generic CI env to populate as much of [`GithubContext`]
/// as possible. The caller mixes in CLI overrides.
pub fn detect_context<F: Fn(&str) -> Option<String>>(get: F) -> DetectedContext {
    let token = get("GITHUB_TOKEN")
        .or_else(|| get("GH_TOKEN"))
        .map(Secret::new);
    let repo = get("GITHUB_REPOSITORY");
    let pr = detect_pr_number(&get);
    DetectedContext { repo, pr, token }
}

fn detect_pr_number<F: Fn(&str) -> Option<String>>(get: &F) -> Option<u64> {
    if let Some(v) = get("AATXE_PR") {
        if let Ok(n) = v.parse::<u64>() {
            if n > 0 {
                return Some(n);
            }
        }
    }
    // GitHub Actions: GITHUB_REF=refs/pull/<num>/merge
    if let Some(reff) = get("GITHUB_REF") {
        if let Some(rest) = reff.strip_prefix("refs/pull/") {
            if let Some(slash) = rest.find('/') {
                if let Ok(n) = rest[..slash].parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    // `pull_request` event payload: GITHUB_REF_NAME=<num>/merge
    if let Some(name) = get("GITHUB_REF_NAME") {
        if let Some((num, suffix)) = name.split_once('/') {
            if matches!(suffix, "merge" | "head") {
                if let Ok(n) = num.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}
