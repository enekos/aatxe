//! Fetch a pull request's unified diff from GitHub.
//!
//! We use the same `ureq` agent as the existing sticky-comment poster
//! and the same `default_headers` helper from [`aatxe_core::github`] —
//! the only difference is the `Accept` header (`application/vnd.github.v3.diff`
//! returns the raw unified-diff payload directly rather than a JSON
//! envelope wrapping it).

use aatxe_core::github::{default_headers, GithubContext};
use anyhow::{anyhow, Context, Result};

/// GitHub returns a hard 406 above this size (per their docs). The actual
/// PR endpoint is more lenient, but if someone is reviewing a 10MB diff
/// the council is the wrong tool — bail with a friendly error.
const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

/// Fetch the unified diff body for a PR. Returns the raw diff text.
pub fn fetch_pr_diff(ctx: &GithubContext) -> Result<String> {
    let url = format!("{}/repos/{}/pulls/{}", ctx.api_base(), ctx.repo, ctx.pr,);
    let mut req = ureq::get(&url);
    for (k, v) in default_headers(&ctx.token) {
        req = req.set(k, &v);
    }
    // Override Accept: the GH `pulls/{n}` endpoint returns the unified diff
    // payload directly when asked for the diff media type.
    req = req.set("Accept", "application/vnd.github.v3.diff");

    let response = req.call().with_context(|| format!("GET {}", url))?;
    if response.status() < 200 || response.status() >= 300 {
        return Err(anyhow!(
            "GitHub returned status {} for {}",
            response.status(),
            url
        ));
    }

    let mut reader = response.into_reader();
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    use std::io::Read;
    reader
        .by_ref()
        .take((MAX_DIFF_BYTES + 1) as u64)
        .read_to_end(&mut buf)
        .with_context(|| format!("reading diff body from {}", url))?;
    if buf.len() > MAX_DIFF_BYTES {
        return Err(anyhow!(
            "PR diff exceeds {} bytes; refusing to review",
            MAX_DIFF_BYTES
        ));
    }
    String::from_utf8(buf).with_context(|| "PR diff was not valid UTF-8")
}
