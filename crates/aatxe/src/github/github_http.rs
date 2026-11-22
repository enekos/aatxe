//! Thin wrapper around [`ureq`] that implements the sticky-comment protocol
//! using the URL/header helpers from [`aatxe_core::github`].
//!
//! Why hand-roll instead of using `octocrab`? Aatxe needs exactly three
//! endpoints. Pulling in a full GitHub SDK + tokio runtime to call them is
//! gratuitous; `ureq` is blocking, dependency-light, and rustls by default.

use aatxe_core::github::{
    create_comment_url, default_headers, list_comments_url, patch_comment_url, GithubContext,
};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Comment {
    id: u64,
    #[serde(default)]
    body: Option<String>,
}

pub struct UpsertResult {
    pub id: u64,
    pub created: bool,
}

#[derive(Default)]
pub struct UreqClient;

impl UreqClient {
    pub fn upsert_sticky_comment(&self, ctx: &GithubContext, body: &str) -> Result<UpsertResult> {
        if let Some(existing) = self.find_sticky_comment(ctx)? {
            self.patch_comment(ctx, existing, body)?;
            return Ok(UpsertResult {
                id: existing,
                created: false,
            });
        }
        let id = self.create_comment(ctx, body)?;
        Ok(UpsertResult { id, created: true })
    }

    fn find_sticky_comment(&self, ctx: &GithubContext) -> Result<Option<u64>> {
        let marker = aatxe_core::report::STICKY_MARKER;
        for page in 1..=50u32 {
            let url = list_comments_url(ctx, page);
            let mut req = ureq::get(&url);
            for (k, v) in default_headers(&ctx.token) {
                req = req.set(k, &v);
            }
            let res = req.call().with_context(|| format!("GET {}", url))?;
            let comments: Vec<Comment> = res.into_json().context("decoding comments list")?;
            for c in &comments {
                if c.body.as_deref().is_some_and(|b| b.contains(marker)) {
                    return Ok(Some(c.id));
                }
            }
            if comments.len() < 100 {
                return Ok(None);
            }
        }
        Ok(None)
    }

    fn patch_comment(&self, ctx: &GithubContext, id: u64, body: &str) -> Result<()> {
        let url = patch_comment_url(ctx, id);
        let mut req = ureq::patch(&url);
        for (k, v) in default_headers(&ctx.token) {
            req = req.set(k, &v);
        }
        let res = req
            .send_json(serde_json::json!({ "body": body }))
            .with_context(|| format!("PATCH {}", url))?;
        if (200..300).contains(&res.status()) {
            Ok(())
        } else {
            Err(anyhow!("GH PATCH comment returned status {}", res.status()))
        }
    }

    fn create_comment(&self, ctx: &GithubContext, body: &str) -> Result<u64> {
        let url = create_comment_url(ctx);
        let mut req = ureq::post(&url);
        for (k, v) in default_headers(&ctx.token) {
            req = req.set(k, &v);
        }
        let res = req
            .send_json(serde_json::json!({ "body": body }))
            .with_context(|| format!("POST {}", url))?;
        let parsed: Comment = res.into_json().context("decoding created comment")?;
        Ok(parsed.id)
    }
}
