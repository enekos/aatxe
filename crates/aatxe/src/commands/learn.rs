//! `aatxe learn` — harvest / compact / show the self-healing learning
//! corpus.
//!
//! This module wires the pure [`aatxe_learn`] crate to the filesystem and
//! the GitHub REST API. The corpus on disk is a single JSON file (default
//! `aatxe-learning-corpus.json`) round-tripped between workflow runs by
//! the `actions/{up,down}load-artifact` actions — the CLI itself never
//! touches the GH artifact API.

use crate::cli::{LearnArgs, LearnCommand, LearnCompactArgs, LearnHarvestArgs, LearnShowArgs};
use crate::commands::Outcome;
use crate::github::github_http::UreqClient;
use aatxe_core::github::{detect_context, GithubContext};
use aatxe_council::types::{CouncilReport, Severity};
use aatxe_learn::harvest::{ShippedFindingRef, DEFAULT_TRUSTED_ASSOCIATIONS};
use aatxe_learn::{
    compact, harvest_pr_feedback, load_self_healing, merge_entries, CompactOptions, HarvestInput,
    LearningCorpus, PrComment,
};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub fn execute(args: LearnArgs) -> Result<Outcome> {
    match args.command {
        LearnCommand::Harvest(a) => harvest(a).map(|_| Outcome::Ok),
        LearnCommand::Compact(a) => do_compact(a).map(|_| Outcome::Ok),
        LearnCommand::Show(a) => show(a).map(|_| Outcome::Ok),
    }
}

fn load_corpus_from_disk(path: &Path, repo: &str) -> LearningCorpus {
    match fs::read_to_string(path) {
        Ok(s) => load_self_healing(&s, repo),
        Err(_) => LearningCorpus::empty(repo),
    }
}

fn write_corpus_to_disk(path: &Path, corpus: &LearningCorpus) -> Result<()> {
    let json = serde_json::to_string_pretty(corpus).context("serialising corpus")?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating parent directory {}", parent.display()))?;
        }
    }
    fs::write(path, json).with_context(|| format!("writing corpus to {}", path.display()))?;
    Ok(())
}

fn harvest(args: LearnHarvestArgs) -> Result<()> {
    let detected = detect_context(|k| std::env::var(k).ok());
    let repo = args
        .repo
        .clone()
        .or(detected.repo.clone())
        .ok_or_else(|| anyhow!("repo: pass --repo or set GITHUB_REPOSITORY"))?;
    let pr = args
        .pr
        .or(detected.pr)
        .ok_or_else(|| anyhow!("pr: pass --pr or set GITHUB_REF / AATXE_PR"))?;

    let mut corpus = load_corpus_from_disk(&args.corpus, &repo);
    corpus.repo = repo.clone();
    if let Some(summary) = &corpus.last_load_summary {
        eprintln!(
            "aatxe learn: loaded corpus from {} ({} entries, {} dropped malformed{})",
            args.corpus.display(),
            summary.entries_loaded,
            summary.entries_dropped_unparseable,
            if summary.corpus_was_invalid {
                " — top-level JSON was invalid, starting fresh"
            } else if summary.corpus_from_future_version.is_some() {
                " — corpus was from a newer schema version, starting fresh"
            } else if summary.schema_upgraded_from.is_some() {
                " — schema upgraded"
            } else {
                ""
            }
        );
    }

    // Load comments — either from --comments-file (test/offline) or GH API.
    let comments: Vec<PrComment> = if let Some(path) = &args.comments_file {
        let s = fs::read_to_string(path)
            .with_context(|| format!("reading comments file {}", path.display()))?;
        serde_json::from_str(&s).context("parsing comments file as Vec<PrComment>")?
    } else {
        let token = args
            .token
            .clone()
            .map(aatxe_core::secret::Secret::new)
            .or(detected.token.clone())
            .ok_or_else(|| anyhow!("token: pass --token or set GITHUB_TOKEN (or use --comments-file for offline runs)"))?;
        let ctx = GithubContext {
            repo: repo.clone(),
            pr,
            token,
            api_base: args.api_base.clone(),
        };
        UreqClient
            .list_pr_comments_with_reactions(&ctx)
            .with_context(|| format!("fetching PR comments for {}#{}", repo, pr))?
    };

    // Identify the council's own sticky comment (if any).
    let marker = aatxe_council::report::STICKY_MARKER;
    let council_comment = comments.iter().find(|c| c.body.contains(marker));
    let other_comments: Vec<PrComment> = comments
        .iter()
        .filter(|c| !c.body.contains(marker))
        .cloned()
        .collect();

    // Load the latest council report if provided, so `good catch on N` can
    // resolve indices. Otherwise harvest with an empty findings list.
    let council_report: Option<CouncilReport> = if let Some(path) = &args.council_report {
        let s = fs::read_to_string(path)
            .with_context(|| format!("reading council report {}", path.display()))?;
        let r: CouncilReport = serde_json::from_str(&s).context("parsing council report JSON")?;
        Some(r)
    } else {
        None
    };
    let shipped_owned: Vec<(String, String, aatxe_council::types::FindingCategory)> =
        council_report
            .as_ref()
            .map(|r| {
                // Replicate the same severity-desc ordering the report
                // renderer uses, so the indices reviewers reference in
                // their comments line up with what they actually see.
                let mut judged = r
                    .judged
                    .iter()
                    .filter(|jf| jf.survives(r.confidence_floor))
                    .collect::<Vec<_>>();
                judged.sort_by(|a, b| {
                    b.finding
                        .severity
                        .cmp(&a.finding.severity)
                        .then(a.finding.category.label().cmp(b.finding.category.label()))
                        .then(a.finding.title.cmp(&b.finding.title))
                });
                judged
                    .into_iter()
                    .map(|jf| {
                        (
                            jf.finding.title.clone(),
                            jf.finding.file.clone(),
                            jf.finding.category,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
    let shipped_refs: Vec<ShippedFindingRef> = shipped_owned
        .iter()
        .map(|(t, f, c)| ShippedFindingRef {
            title: t.as_str(),
            file: f.as_str(),
            category: *c,
        })
        .collect();

    let now = now_iso();
    // Resolve the trust allowlist. CLI override wins; otherwise use the
    // safe default ("OWNER", "MEMBER", "COLLABORATOR" — i.e. write access).
    let trusted_owned: Vec<String> = if args.trusted_associations.is_empty() {
        DEFAULT_TRUSTED_ASSOCIATIONS
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        args.trusted_associations.clone()
    };
    let trusted_refs: Vec<&str> = trusted_owned.iter().map(String::as_str).collect();
    let input = HarvestInput {
        repo: repo.as_str(),
        pr,
        council_comment,
        other_comments: &other_comments,
        shipped_findings: &shipped_refs,
        bot_login: &args.bot_login,
        trusted_associations: &trusted_refs,
        now_iso: &now,
    };
    let new_entries = harvest_pr_feedback(&input);

    let n_new = new_entries.len();
    let stats = merge_entries(&mut corpus, new_entries);

    let opts = CompactOptions {
        max_entries: args.max_entries,
        min_score: args.min_score,
        ..CompactOptions::default()
    };
    let evicted = compact(&mut corpus, &opts, OffsetDateTime::now_utc());
    let _ = Severity::Minor; // touch a council type so the import stays meaningful

    corpus.updated_at = now;
    write_corpus_to_disk(&args.corpus, &corpus)?;

    eprintln!(
        "aatxe learn: harvested {} signal(s) from PR #{} → {} new entries, {} updated, {} evicted, {} total",
        n_new,
        pr,
        stats.added,
        stats.updated,
        evicted,
        corpus.entries.len(),
    );
    Ok(())
}

fn do_compact(args: LearnCompactArgs) -> Result<()> {
    let mut corpus = load_corpus_from_disk(&args.corpus, "");
    let before = corpus.entries.len();
    let opts = CompactOptions {
        max_entries: args.max_entries,
        min_score: args.min_score,
        ..CompactOptions::default()
    };
    let evicted = compact(&mut corpus, &opts, OffsetDateTime::now_utc());
    corpus.updated_at = now_iso();
    write_corpus_to_disk(&args.corpus, &corpus)?;
    eprintln!(
        "aatxe learn compact: {} → {} entries ({} evicted)",
        before,
        corpus.entries.len(),
        evicted,
    );
    Ok(())
}

fn show(args: LearnShowArgs) -> Result<()> {
    let corpus = load_corpus_from_disk(&args.corpus, "");
    if args.json {
        let json = serde_json::to_string_pretty(&corpus).context("serialising corpus")?;
        println!("{}", json);
        return Ok(());
    }
    println!(
        "Corpus: repo={} entries={} schema_version={} updated_at={}",
        if corpus.repo.is_empty() {
            "(unset)"
        } else {
            &corpus.repo
        },
        corpus.entries.len(),
        corpus.schema_version,
        if corpus.updated_at.is_empty() {
            "(never)"
        } else {
            &corpus.updated_at
        },
    );
    if let Some(s) = &corpus.last_load_summary {
        if s.entries_dropped_unparseable > 0
            || s.corpus_was_invalid
            || s.schema_upgraded_from.is_some()
            || s.corpus_from_future_version.is_some()
        {
            println!(
                "  load summary: dropped={} invalid={} upgraded_from={:?} from_future={:?}",
                s.entries_dropped_unparseable,
                s.corpus_was_invalid,
                s.schema_upgraded_from,
                s.corpus_from_future_version,
            );
        }
    }
    if corpus.entries.is_empty() {
        println!("(empty)");
        return Ok(());
    }
    for (i, e) in corpus.entries.iter().enumerate() {
        println!(
            "  #{:>3}  score={:>6.2}  kind={:<14} +{}/-{}  pr#{}  {}{}",
            i,
            e.score,
            e.strongest_kind.label(),
            e.confirmations,
            e.refutations,
            e.source_pr,
            scope_str(e),
            truncated(&e.guidance, 80),
        );
    }
    Ok(())
}

fn scope_str(e: &aatxe_learn::LearnedEntry) -> String {
    match (&e.file_glob, e.category) {
        (Some(g), Some(c)) => format!("[{}·{}] ", g, c.label()),
        (Some(g), None) => format!("[{}] ", g),
        (None, Some(c)) => format!("[{}] ", c.label()),
        (None, None) => String::new(),
    }
}

fn truncated(s: &str, max: usize) -> String {
    let normalised: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.len() <= max {
        normalised
    } else {
        format!("{}…", &normalised[..max])
    }
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
