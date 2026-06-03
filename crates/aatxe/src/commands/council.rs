//! `aatxe council` — fetch a PR diff, run the pi-proxy agent council,
//! render the sticky markdown body, and optionally post it.
//!
//! Wires together:
//! * [`crate::gh_diff::fetch_pr_diff`] — pulls the unified diff over
//!   `Accept: application/vnd.github.v3.diff`.
//! * [`crate::pi_proxy::PiAgentClient`] — spawns the local `pi` coding
//!   agent per LLM call so proposers can `read`/`grep`/`find`/`ls` the
//!   repo under review. Uses `KIMI_API_KEY` (forwarded to the child).
//! * [`aatxe_council::pipeline::run_council`] — the proposer→judge
//!   pipeline lives in the pure crate.
//! * [`crate::github_http::UreqClient`] — same sticky-comment client the
//!   perf gate uses, with the council's own marker.

use crate::claude_code::{ClaudeCodeClient, ClaudeCodeConfig};
use crate::cli::{BackendArg, CouncilArgs};
use crate::commands::Outcome;
use crate::gh_diff::fetch_pr_diff;
use crate::github_http::UreqClient;
use crate::pi_proxy::{PiAgentClient, PiConfig};
use crate::stub_client::{stub_enabled, StubKimi};
use aatxe_core::github::{detect_context, validate_sticky, GithubContext};
use aatxe_council::diff::parse_unified_diff;
use aatxe_council::events::{CouncilEvent, EventSink, NullSink};
use aatxe_council::llm::LlmClient;
use aatxe_council::pipeline::{run_council, CouncilOptions};
use aatxe_council::report::render_markdown;
use aatxe_learn::{build_guidance, load_self_healing, InjectionContext};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

pub fn execute(args: CouncilArgs) -> Result<Outcome> {
    let use_stub = stub_enabled();
    let model = if use_stub {
        args.model.clone().unwrap_or_else(|| "stub".to_string())
    } else {
        // Both real backends resolve credentials per-provider through the
        // child process; we surface a display string for the report
        // header only.
        match args.backend {
            BackendArg::PiProxy => {
                let pi_cfg_peek = PiConfig::from_env();
                args.model
                    .clone()
                    .unwrap_or_else(|| format!("pi+{}", pi_cfg_peek.model))
            }
            BackendArg::ClaudeCode => {
                let cc_peek = ClaudeCodeConfig::from_env();
                args.model.clone().unwrap_or_else(|| {
                    cc_peek
                        .model
                        .clone()
                        .map(|m| format!("claude-code+{m}"))
                        .unwrap_or_else(|| "claude-code".to_string())
                })
            }
        }
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

    // Run the council. Pick the LLM client: deterministic stub for the
    // offline smoke-test path (`AATXE_COUNCIL_STUB=1`), or the pi-proxy
    // agent runner otherwise.
    let client: Box<dyn LlmClient> = if use_stub {
        eprintln!("aatxe council: AATXE_COUNCIL_STUB=1 — using deterministic stub LLM (no Moonshot/Anthropic calls)");
        Box::new(StubKimi)
    } else {
        match args.backend {
            BackendArg::PiProxy => {
                let mut pi_cfg = PiConfig::from_env();
                if let Some(p) = args.pi_binary.clone() {
                    pi_cfg.binary = p;
                }
                eprintln!(
                    "aatxe council: pi-proxy ({}/{}), tools=read+grep+find+ls",
                    pi_cfg.provider, pi_cfg.model,
                );
                Box::new(PiAgentClient::new(pi_cfg))
            }
            BackendArg::ClaudeCode => {
                let mut cc_cfg = ClaudeCodeConfig::from_env();
                if let Some(p) = args.claude_binary.clone() {
                    cc_cfg.binary = p;
                }
                eprintln!(
                    "aatxe council: claude-code ({}{}), tools=Read+Grep+Glob",
                    cc_cfg.model.as_deref().unwrap_or("subscription-default"),
                    cc_cfg
                        .max_budget_usd
                        .map(|b| format!(", budget=${b}"))
                        .unwrap_or_default(),
                );
                Box::new(ClaudeCodeClient::new(cc_cfg))
            }
        }
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

    let event_sink: Arc<dyn EventSink> = match &args.json_events {
        Some(spec) => Arc::new(JsonLinesSink::open(spec.as_str())?),
        None => Arc::new(NullSink),
    };

    let opts = CouncilOptions {
        model: model.clone(),
        repo: repo.clone().unwrap_or_default(),
        pr: pr.unwrap_or(0),
        confidence_floor: args.confidence_floor,
        extra_ignored: args.extra_ignored.clone(),
        learned_guidance,
        event_sink,
        ..CouncilOptions::default()
    };
    let mut report =
        run_council(&diff_text, &opts, client.as_ref()).context("council pipeline failed")?;

    // Interactive curation — default-on when stdin is a TTY *and*
    // `--post` is set (no point curating if nothing is going to be
    // posted). `--interactive=true`/`false` forces either direction.
    let want_interactive = match args.interactive {
        Some(v) => v,
        None => args.post && crate::curator::stdin_is_tty(),
    };
    if want_interactive {
        let stdin = std::io::stdin();
        let reader = stdin.lock();
        let stderr = std::io::stderr();
        let mut writer = stderr.lock();
        let summary = crate::curator::curate_report(
            &mut report,
            std::io::BufReader::new(reader),
            &mut writer,
        )?;
        if summary.dropped > 0 {
            eprintln!(
                "aatxe council: curator dropped {} finding(s) at indices {:?}",
                summary.dropped, summary.dropped_indices,
            );
        }
    }

    // Render — runs AFTER curation so dropped findings are filtered out
    // by `shippable()`'s verdict==Drop check.
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

/// `EventSink` that writes one JSON object per line to a configurable
/// destination. Matches the `jq`-friendly `kind`-tagged shape defined in
/// [`aatxe_council::events::CouncilEvent`]. Use `--json-events -` for
/// stdout; anything else is treated as a path.
///
/// Writes are serialised behind a mutex so the parallel proposer
/// threads can't interleave a half-written line on the consumer.
/// `emit` swallows IO errors (a broken pipe shouldn't kill a 60-minute
/// council run); fatal-on-open errors do surface so the CLI can fail
/// fast when the user mistypes a path.
enum JsonLinesTarget {
    Stdout,
    File(std::fs::File),
}

#[derive(Debug)]
pub(crate) struct JsonLinesSink {
    inner: Mutex<JsonLinesTarget>,
}

impl std::fmt::Debug for JsonLinesTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonLinesTarget::Stdout => write!(f, "Stdout"),
            JsonLinesTarget::File(_) => write!(f, "File"),
        }
    }
}

impl JsonLinesSink {
    pub(crate) fn open(spec: &str) -> Result<Self> {
        let target = if spec == "-" || spec.is_empty() {
            JsonLinesTarget::Stdout
        } else {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(spec)
                .with_context(|| format!("opening --json-events sink {spec}"))?;
            JsonLinesTarget::File(f)
        };
        Ok(Self {
            inner: Mutex::new(target),
        })
    }

    fn write_line(&self, line: &str) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return, // poisoned — silently drop, council keeps running
        };
        let res: std::io::Result<()> = match &mut *guard {
            JsonLinesTarget::Stdout => {
                let mut out = std::io::stdout().lock();
                out.write_all(line.as_bytes())
                    .and_then(|()| out.write_all(b"\n"))
            }
            JsonLinesTarget::File(f) => f
                .write_all(line.as_bytes())
                .and_then(|()| f.write_all(b"\n")),
        };
        let _ = res; // best-effort
    }
}

impl EventSink for JsonLinesSink {
    fn emit(&self, event: &CouncilEvent) {
        if let Ok(line) = serde_json::to_string(event) {
            self.write_line(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_lines_sink_writes_one_line_per_event_to_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let sink = JsonLinesSink::open(path.to_str().unwrap()).unwrap();
        sink.emit(&CouncilEvent::Start {
            repo: "x/y".into(),
            pr: 1,
            model: "stub".into(),
            files_total: 1,
            files_reviewed: 1,
            n_chunks: 1,
        });
        sink.emit(&CouncilEvent::Done {
            total_duration_ms: 42,
            shippable_count: 0,
            critical_count: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
        });
        // Drop the sink so its mutex/file are flushed/closed.
        drop(sink);
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "one JSON object per emit: {body}");
        assert!(lines[0].contains("\"kind\":\"start\""));
        assert!(lines[1].contains("\"kind\":\"done\""));
        // Each line must parse as a CouncilEvent again — guards against
        // accidental schema breakage between writer + reader.
        for line in &lines {
            let _: CouncilEvent =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("bad line {line}: {e}"));
        }
    }

    #[test]
    fn json_lines_sink_open_with_dash_does_not_touch_filesystem() {
        let sink = JsonLinesSink::open("-").unwrap();
        // Emitting to stdout in a unit test would pollute the test
        // runner's output but should not panic or error.
        sink.emit(&CouncilEvent::SynthesizeDone {
            n_raw: 0,
            n_deduped: 0,
        });
    }
}
