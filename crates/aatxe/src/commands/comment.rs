//! `aatxe comment` — post / update the sticky PR comment.

use crate::cli::CommentArgs;
use crate::github_http::UreqClient;
use aatxe_core::github::{detect_context, validate_sticky, GithubContext};
use aatxe_core::secret::Secret;
use anyhow::{anyhow, Context, Result};
use std::fs;

pub fn execute(args: CommentArgs) -> Result<()> {
    let body = fs::read_to_string(&args.report)
        .with_context(|| format!("reading body from {}", args.report.display()))?;
    validate_sticky(&body).map_err(|e| anyhow!(e.to_string()))?;

    let detected = detect_context(|k| std::env::var(k).ok());
    let repo = args
        .repo
        .or(detected.repo)
        .ok_or_else(|| anyhow!("missing repo: pass --repo or set GITHUB_REPOSITORY"))?;
    let pr = args
        .pr
        .or(detected.pr)
        .ok_or_else(|| anyhow!("missing pr: pass --pr or run inside a pull_request event"))?;
    let token = args
        .token
        .map(Secret::new)
        .or(detected.token)
        .ok_or_else(|| anyhow!("missing token: pass --token or set GITHUB_TOKEN/GH_TOKEN"))?;

    let ctx = GithubContext {
        repo,
        pr,
        token,
        api_base: args.api_base,
    };
    let client = UreqClient;
    let result = client.upsert_sticky_comment(&ctx, &body)?;
    if result.created {
        println!("created sticky comment id={}", result.id);
    } else {
        println!("updated sticky comment id={}", result.id);
    }
    Ok(())
}
