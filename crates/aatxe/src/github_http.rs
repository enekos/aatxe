//! Thin wrapper around [`ureq`] that implements the sticky-comment protocol
//! using the URL/header helpers from [`aatxe_core::github`].
//!
//! Why hand-roll instead of using `octocrab`? Aatxe needs exactly three
//! endpoints. Pulling in a full GitHub SDK + tokio runtime to call them is
//! gratuitous; `ureq` is blocking, dependency-light, and rustls by default.

use aatxe_core::github::{
    create_comment_url, default_headers, list_comments_url, patch_comment_url, GithubContext,
};
use aatxe_learn::harvest::{PrComment, Reactions};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Comment {
    id: u64,
    #[serde(default)]
    body: Option<String>,
}

/// Slice of the issue-comment object we need for harvest: id, body,
/// author login, author association, reaction summary, timestamp. The GH
/// REST API populates the `reactions` field by default with
/// `Accept: application/vnd.github+json`; `author_association` is always
/// present on issue-comment responses.
#[derive(Debug, Deserialize)]
struct CommentWithReactions {
    id: u64,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    user: Option<UserRef>,
    #[serde(default)]
    author_association: Option<String>,
    #[serde(default)]
    reactions: Option<ReactionsRaw>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UserRef {
    login: String,
}

#[derive(Debug, Deserialize, Default)]
struct ReactionsRaw {
    #[serde(default, rename = "+1")]
    plus_one: u32,
    #[serde(default, rename = "-1")]
    minus_one: u32,
    #[serde(default)]
    heart: u32,
    #[serde(default)]
    hooray: u32,
    #[serde(default)]
    rocket: u32,
    #[serde(default)]
    confused: u32,
}

impl From<ReactionsRaw> for Reactions {
    fn from(r: ReactionsRaw) -> Self {
        Reactions {
            plus_one: r.plus_one,
            minus_one: r.minus_one,
            heart: r.heart,
            hooray: r.hooray,
            rocket: r.rocket,
            confused: r.confused,
        }
    }
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

    /// List every issue comment on a PR with reactions + author. Pages
    /// through the GH API up to 50 pages × 100 = 5000 comments
    /// (vastly more than any reasonable PR).
    pub fn list_pr_comments_with_reactions(&self, ctx: &GithubContext) -> Result<Vec<PrComment>> {
        let mut out: Vec<PrComment> = Vec::new();
        for page in 1..=50u32 {
            let url = list_comments_url(ctx, page);
            let mut req = ureq::get(&url);
            for (k, v) in default_headers(&ctx.token) {
                req = req.set(k, &v);
            }
            let res = req.call().with_context(|| format!("GET {}", url))?;
            let raw: Vec<CommentWithReactions> =
                res.into_json().context("decoding comments list")?;
            let was_full = raw.len() == 100;
            for c in raw {
                out.push(PrComment {
                    id: c.id,
                    body: c.body.unwrap_or_default(),
                    user_login: c.user.map(|u| u.login).unwrap_or_default(),
                    // Fail-closed when GH omits the field — `NONE`
                    // associations are not in the default trust allowlist.
                    author_association: c.author_association.unwrap_or_else(|| "NONE".into()),
                    reactions: c.reactions.unwrap_or_default().into(),
                    created_at: c.created_at.unwrap_or_default(),
                });
            }
            if !was_full {
                break;
            }
        }
        Ok(out)
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
