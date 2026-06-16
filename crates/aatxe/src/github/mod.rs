//! GitHub REST helpers used by the CLI (the pure URL/header logic lives in
//! `aatxe_core::github`).
//!
//! * [`github_http`] — `ureq`-backed client that posts/updates the sticky PR
//!   comment and lists comments + reactions for the learning corpus.
//! * [`gh_diff`] — fetches a PR's unified diff (`Accept: vnd.github.v3.diff`).

pub mod gh_diff;
pub mod github_http;
