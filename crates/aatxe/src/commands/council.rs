//! `aatxe council` — fetch a PR diff, run the Kimi-backed agent council,
//! render the sticky markdown body, and optionally post it.
//!
//! Wires together:
//! * [`crate::gh_diff::fetch_pr_diff`] — pulls the unified diff over
//!   `Accept: application/vnd.github.v3.diff`.
//! * [`crate::kimi_http::KimiClient`] — OpenAI-compatible Moonshot client.
//! * [`aatxe_council::pipeline::run_council`] — the proposer→judge
//!   pipeline lives in the pure crate.
//! * [`crate::github_http::UreqClient`] — same sticky-comment client the
//!   perf gate uses, with the council's own marker.

use crate::cli::CouncilArgs;
use crate::commands::Outcome;
use crate::gh_diff::fetch_pr_diff;
use crate::github_http::UreqClient;
use crate::kimi_http::{KimiClient, KimiConfig};
use crate::stub_client::{stub_enabled, StubKimi};
use aatxe_core::github::{detect_context, validate_sticky, GithubContext};
use aatxe_council::diff::parse_unified_diff;
use aatxe_council::llm::LlmClient;
use aatxe_council::pipeline::{run_council, CouncilOptions};
use aatxe_council::report::render_markdown;
use aatxe_learn::{build_guidance, load_self_healing, InjectionContext};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::Read;

pub fn execute(args: CouncilArgs) -> Result<Outcome> {
    let use_stub = stub_enabled();
    let model = if use_stub {
        args.model.clone().unwrap_or_else(|| "stub".to_string())
    } else {
        let kimi_cfg_peek = KimiConfig::from_env()
            .ok_or_else(|| anyhow!("KIMI_API_KEY is not set — required for `aatxe council` (or export AATXE_COUNCIL_STUB=1 for an offline smoke test)"))?;
        args.model
            .clone()
            .unwrap_or_else(|| kimi_cfg_peek.default_model.clone())
    };

    // Source diff + GH context.
    let detected = detect_context(|k| std::env::var(k).ok());
    let pr = args.pr.or(detected.pr);
    let repo = args.repo.clone().or(detected.repo.clone());
    let token = args
        .token
        .clone()
        .map(aatxe_core::secret::Secret::new)
        .or(detected.token.clone());

    let (diff_text, gh_ctx_opt) = if let Some(path) = &args.diff_file {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading diff from {}", path.display()))?;
        (text, None)
    } else if let (Some(repo), Some(pr), Some(token)) = (repo.clone(), pr, token.clone()) {
        let ctx = GithubContext {
            repo: repo.clone(),
            pr,
            token,
            api_base: args.api_base.clone(),
        };
        let text =
            fetch_pr_diff(&ctx).with_context(|| format!("fetching PR diff for {}#{}", repo, pr))?;
        (text, Some(ctx))
    } else {
        // stdin fallback — useful for local dry-runs.
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading diff from stdin")?;
        if s.trim().is_empty() {
            return Err(anyhow!(
                "no diff source: pass --diff-file <path>, --pr <num>, or pipe a diff on stdin"
            ));
        }
        (s, None)
    };

    // Run the council. Pick the LLM client: stub (offline smoke test) or
    // the real Kimi HTTP client.
    let client: Box<dyn LlmClient> = if use_stub {
        eprintln!("aatxe council: AATXE_COUNCIL_STUB=1 — using deterministic stub LLM (no Moonshot calls)");
        Box::new(StubKimi)
    } else {
        let kimi_cfg = KimiConfig::from_env()
            .ok_or_else(|| anyhow!("KIMI_API_KEY disappeared between checks"))?;
        Box::new(KimiClient::new(kimi_cfg))
    };
    // Load the learning corpus (if any) and render the guidance block.
    // The council pipeline is decoupled from aatxe-learn — we hand it a
    // plain `learned_guidance: String` so it stays unaware of the corpus.
    let learned_guidance = match &args.learning_corpus {
        Some(path) => {
            let json = fs::read_to_string(path).unwrap_or_default();
            let corpus = load_self_healing(&json, repo.as_deref().unwrap_or(""));
            if let Some(s) = &corpus.last_load_summary {
                if s.entries_dropped_unparseable > 0
                    || s.corpus_was_invalid
                    || s.corpus_from_future_version.is_some()
                {
                    eprintln!(
                        "aatxe council: learning corpus had load warnings — dropped {} malformed entr{} invalid={} from_future={:?}",
                        s.entries_dropped_unparseable,
                        if s.entries_dropped_unparseable == 1 { "y," } else { "ies," },
                        s.corpus_was_invalid,
                        s.corpus_from_future_version,
                    );
                }
            }
            // Use the diff's changed files to filter scoped guidance.
            let parsed = parse_unified_diff(&diff_text);
            let changed_files: Vec<String> = parsed.into_iter().map(|f| f.path).collect();
            let ctx = InjectionContext {
                changed_files: &changed_files,
                max_entries: args.learning_max_entries,
                max_chars: 1500,
                persona_filter: None,
            };
            let g = build_guidance(&corpus, &ctx);
            if g.is_empty() {
                eprintln!("aatxe council: learning corpus has no relevant entries for this PR");
            } else {
                eprintln!(
                    "aatxe council: injecting {} learning-corpus entr{} into prompts",
                    corpus.entries.len().min(args.learning_max_entries),
                    if corpus.entries.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    }
                );
            }
            g
        }
        None => String::new(),
    };

    let opts = CouncilOptions {
        model: model.clone(),
        repo: repo.clone().unwrap_or_default(),
        pr: pr.unwrap_or(0),
        confidence_floor: args.confidence_floor,
        extra_ignored: args.extra_ignored.clone(),
        learned_guidance,
        ..CouncilOptions::default()
    };
    let report =
        run_council(&diff_text, &opts, client.as_ref()).context("council pipeline failed")?;

    // Render.
    let body = render_markdown(&report);

    // Persist artefacts.
    if let Some(path) = &args.out {
        let json = serde_json::to_string_pretty(&report).context("serialising CouncilReport")?;
        fs::write(path, json)
            .with_context(|| format!("writing council report to {}", path.display()))?;
        eprintln!("aatxe council: wrote {}", path.display());
    }
    if let Some(path) = &args.markdown {
        fs::write(path, &body)
            .with_context(|| format!("writing council markdown to {}", path.display()))?;
        eprintln!("aatxe council: wrote {}", path.display());
    }
    if args.markdown.is_none() && !args.post {
        // No artefact requested → print body to stdout for the caller to capture.
        println!("{}", body);
    }

    // Post the sticky comment if requested.
    if args.post {
        let ctx = gh_ctx_opt
            .or_else(|| {
                match (repo, pr, token) {
                    (Some(r), Some(p), Some(t)) => Some(GithubContext {
                        repo: r,
                        pr: p,
                        token: t,
                        api_base: args.api_base.clone(),
                    }),
                    _ => None,
                }
            })
            .ok_or_else(|| {
                anyhow!(
                    "--post requires --repo + --pr + --token (or GITHUB_REPOSITORY / GITHUB_TOKEN env)"
                )
            })?;
        validate_sticky(&body).map_err(|e| anyhow!("council body missing sticky marker: {e}"))?;
        let client = UreqClient;
        let res = client.upsert_sticky_comment(&ctx, &body)?;
        if res.created {
            eprintln!("aatxe council: created sticky comment id={}", res.id);
        } else {
            eprintln!("aatxe council: updated sticky comment id={}", res.id);
        }
    }

    let shippable = report.shippable();
    eprintln!(
        "aatxe council: {} shippable findings ({} critical) across {} reviewed file(s) in {} ms",
        shippable.len(),
        shippable
            .iter()
            .filter(|jf| jf.finding.severity == aatxe_council::types::Severity::Critical)
            .count(),
        report.files_reviewed,
        report.total_duration_ms,
    );

    if args.fail_on_critical && report.has_critical() {
        Ok(Outcome::Regressions)
    } else {
        Ok(Outcome::Ok)
    }
}
