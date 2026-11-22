//! GitHub REST helpers used by the CLI (the pure URL/header logic lives in
//! `aatxe_core::github`).
//!
//! * [`github_http`] — `ureq`-backed client that posts/updates the sticky PR
//!   comment.

pub mod github_http;
