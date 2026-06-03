//! Unified-diff parsing + path filtering + chunking.
//!
//! The council never sees the raw GitHub diff body unfiltered. We strip
//! noise upstream of any LLM call for three reasons:
//!
//! 1. **Cost.** Lockfiles can be megabytes. Sending them to the model
//!    is pure burn.
//! 2. **False positives.** Generated files are the single largest source
//!    of LLM-PR-reviewer nit spam in published audits (see e.g. the dev.to
//!    "4 reviewers, 146 PRs" comparison — 15% useless + 21% nitpicking on
//!    the worst-tuned tool, almost entirely from generated/lock files).
//! 3. **Hallucination surface.** Vendored / generated code uses idioms the
//!    model has never been trained to review *as authored*, so it flags
//!    things that are intentional artefacts of the generator.

use serde::{Deserialize, Serialize};

/// Default filename-and-glob blocklist applied before the council runs.
/// Matching is on the full POSIX-style repo-relative path, OR on the path
/// basename, OR on a substring (for `node_modules/` etc.).
pub const DEFAULT_IGNORED_PATTERNS: &[&str] = &[
    // Lockfiles
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "go.sum",
    "Pipfile.lock",
    "composer.lock",
    "Gemfile.lock",
    "uv.lock",
    // Generated code (substring match)
    ".pb.go",
    ".pb.h",
    ".pb.cc",
    "_pb2.py",
    "_pb2_grpc.py",
    ".generated.go",
    ".gen.go",
    ".generated.ts",
    ".gen.ts",
    ".g.dart",
    "generated.dart",
    // Vendored / build artefacts
    "node_modules/",
    "vendor/",
    "target/",
    "build/",
    "dist/",
    "out/",
    ".next/",
    ".nuxt/",
    "__pycache__/",
    ".min.js",
    ".min.css",
    // Snapshots / fixtures we don't review
    "__snapshots__/",
    ".snap",
];

/// A single file's slice of a unified diff plus its parsed metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ParsedFile {
    /// Repo-relative POSIX path of the *new* (post-PR) revision.
    /// For deletions, falls back to the old path.
    pub path: String,
    /// True when the entire file is new in this PR.
    pub is_new: bool,
    /// True when the file is deleted by this PR.
    pub is_deleted: bool,
    /// True when only the path changed (rename without content edits).
    pub is_pure_rename: bool,
    /// Lines added (`+`) in this file's hunks.
    pub additions: u32,
    /// Lines removed (`-`) in this file's hunks.
    pub deletions: u32,
    /// The unified-diff text for *this file only*, with the original
    /// `diff --git` header through the end of the last hunk. Preserved
    /// verbatim so the LLM sees what the human reviewer would see.
    pub body: String,
    /// Full post-PR content of this file when available. Populated by
    /// callers that have repo-checkout access (eval harness, future
    /// `aatxe council --repo-path`); left `None` when only the diff is
    /// available. The prompt builder uses this to give proposers
    /// surrounding-code context for hunks they would otherwise see in
    /// isolation. Truncated if oversized — see
    /// [`ChunkPolicy::max_file_context_bytes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// What [`chunk_for_review`] produced.
#[derive(Debug, Clone)]
pub struct DiffChunk {
    /// Files in this chunk. May be a single huge file, or a batch of small
    /// ones whose total body fits the budget.
    pub files: Vec<ParsedFile>,
    /// Pre-rendered diff body the prompt builder can splice in directly.
    pub body: String,
    /// Total bytes in `body`.
    pub bytes: usize,
    /// Repository files NOT in the diff that the caller flagged as
    /// relevant context for this chunk — typically helpers / patterns /
    /// types the diff references but doesn't modify. The prompt builder
    /// renders these in a dedicated "Related repository context" block,
    /// distinct from the per-file `context` on each [`ParsedFile`].
    ///
    /// Filled by [`chunk_for_review`] from its `related` argument, applying
    /// [`ChunkPolicy::max_related_context_bytes`] for per-file truncation
    /// and [`ChunkPolicy::max_chunk_related_bytes`] as a chunk-level cap.
    /// Files that don't fit are dropped silently (the diff and per-file
    /// context always survive).
    pub related: Vec<RelatedFile>,
}

/// A repository file presented as cross-reference context — NOT a file
/// the PR modified. Distinct from [`ParsedFile`] because we want zero
/// ambiguity at every layer (parser, chunker, prompt, eval scorer) about
/// whether a piece of content is being reviewed or just referenced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedFile {
    /// Repo-relative POSIX path.
    pub path: String,
    /// File body. Truncation, if any, has already been applied by the
    /// chunker — what's here is what ships to the model.
    pub content: String,
}

/// Tunables for chunking the parsed file list into LLM-sized batches.
#[derive(Debug, Clone, Copy)]
pub struct ChunkPolicy {
    /// Soft limit on the body bytes per chunk. We pack files greedily up to
    /// this. Default ~120 KB, comfortable inside Kimi K2.6's 256K-token
    /// context while leaving room for the system prompt + JSON schema +
    /// response. Public so the CLI / benches can override.
    pub max_chunk_bytes: usize,
    /// Hard ceiling on the body of any single file. If a file's body exceeds
    /// this we truncate the middle and emit a `... [truncated] ...` marker
    /// so the model knows it didn't see everything.
    pub max_file_bytes: usize,
    /// Hard ceiling on the full-file context block attached to a single
    /// file (see [`ParsedFile::context`]). Larger context windows let the
    /// model reason about surrounding code but cost tokens linearly.
    /// Default 64 KB — about a 1500-line source file at typical density,
    /// big enough that ~95% of real-world reviewed files fit unchanged.
    pub max_file_context_bytes: usize,
    /// Hard ceiling on the *sum* of all per-file context blocks in a
    /// single chunk. Prevents a chunk full of small files from emitting
    /// hundreds of KB of context all at once. Default 256 KB. Files
    /// beyond the budget get their context dropped (diff still shipped),
    /// in declaration order — the most-changed files in a PR are usually
    /// listed first by the GitHub API.
    pub max_chunk_context_bytes: usize,
    /// Hard ceiling on the body of any single *related* file (see
    /// [`DiffChunk::related`]). Same shape as `max_file_context_bytes`
    /// but applied to cross-reference files. Default 32 KB — related
    /// files are summary references, not the primary review surface, so
    /// we trim them more aggressively than the files being reviewed.
    pub max_related_context_bytes: usize,
    /// Hard ceiling on the *sum* of all related-file blocks in a single
    /// chunk. Default 128 KB. Distinct from
    /// `max_chunk_context_bytes` so that loading a fat helper module as
    /// related context doesn't starve the per-file context of the files
    /// the reviewer is actually scoring.
    pub max_chunk_related_bytes: usize,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self {
            max_chunk_bytes: 120 * 1024,
            max_file_bytes: 80 * 1024,
            max_file_context_bytes: 64 * 1024,
            max_chunk_context_bytes: 256 * 1024,
            max_related_context_bytes: 32 * 1024,
            max_chunk_related_bytes: 128 * 1024,
        }
    }
}

/// Split a unified-diff blob (as returned by GitHub with
/// `Accept: application/vnd.github.v3.diff`) into per-file slices.
///
/// Tolerant of:
/// * trailing newlines / `\r\n` mixed line endings
/// * binary file markers (`Binary files ... differ`) — these emit a
///   [`ParsedFile`] with `additions = deletions = 0` so the filter can
///   decide whether to ignore.
/// * rename-only entries with no `@@` hunks.
///
/// Intentionally NOT tolerant of:
/// * combined-mode merge diffs (`diff --cc`) — those should not appear on
///   normal PR review surfaces; if they do, we drop the entry.
pub fn parse_unified_diff(text: &str) -> Vec<ParsedFile> {
    // Avoid a full-string scan + allocation when the input is already LF-only.
    // GitHub diffs have consistent line endings; checking the first 4 KB is
    // enough to detect CRLF with near-certainty, saving a 100 MB scan for the
    // overwhelmingly-common LF-only case.
    let owned: String;
    let normalised: &str = if text[..text.len().min(4096)].contains("\r\n") {
        owned = text.replace("\r\n", "\n");
        &owned
    } else {
        text
    };

    // Collect all file slices so we can parse them in parallel.
    // Pre-size the vector using a cheap lower-bound heuristic: assume at
    // least 4 KB per file (conservative for real diffs). Over-allocation is
    // harmless (just a few hundred KB) and eliminates all reallocations.
    let est_files = normalised.len() / 4096;
    let mut pieces: Vec<&str> = Vec::with_capacity(est_files.max(4));
    let mut iter = normalised.split("\ndiff --git ");
    if let Some(first) = iter.next() {
        if let Some(stripped) = first.strip_prefix("diff --git ") {
            pieces.push(stripped);
        } else if !first.trim().is_empty() && first.contains("diff --git ") {
            // Some upstreams elide the leading newline; recover.
            pieces.extend(first.split("diff --git ").filter(|s| !s.trim().is_empty()));
        }
    }
    for piece in iter {
        if !piece.trim().is_empty() {
            pieces.push(piece);
        }
    }

    // Parse slices in parallel using available CPU cores.
    // Each slice is independent and only borrows from `normalised`,
    // so scoped threads are safe.
    // Cap at 8 threads: on Apple Silicon (and many x86 laptops) more threads
    // just increase scheduler overhead without adding real parallelism, and
    // may land work on efficiency cores which are slower for CPU-bound tasks.
    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(1)
        .min(pieces.len())
        .max(1);
    if n_threads == 1 {
        pieces.into_iter().filter_map(parse_one_file).collect()
    } else {
        let chunk_size = pieces.len().div_ceil(n_threads);
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(n_threads);
            for chunk in pieces.chunks(chunk_size) {
                handles.push(s.spawn(|| {
                    chunk
                        .iter()
                        .copied()
                        .filter_map(parse_one_file)
                        .collect::<Vec<_>>()
                }));
            }
            let mut out = Vec::with_capacity(pieces.len());
            for handle in handles {
                out.extend(handle.join().unwrap());
            }
            out
        })
    }
}

#[inline]
fn parse_one_file(body_without_header_prefix: &str) -> Option<ParsedFile> {
    // body_without_header_prefix starts with: "a/<old> b/<new>\nindex ...\n..."
    let mut path: Option<&str> = None;
    let mut old_path: Option<&str> = None;
    let mut is_new = false;
    let mut is_deleted = false;
    let mut is_pure_rename = false;
    let mut additions: u32 = 0;
    let mut deletions: u32 = 0;
    let mut saw_hunk = false;
    let mut saw_combined = false;

    for line in body_without_header_prefix.split('\n') {
        if saw_hunk {
            if line.starts_with('+') && !line.starts_with("+++") {
                additions += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                deletions += 1;
            }
            continue;
        }
        if line.starts_with("diff --cc ") || line.starts_with("diff --combined ") {
            saw_combined = true;
            break;
        }
        if let Some(p) = line.strip_prefix("--- ") {
            if p != "/dev/null" {
                old_path = Some(strip_git_prefix(p));
            }
        } else if let Some(p) = line.strip_prefix("+++ ") {
            if p == "/dev/null" {
                is_deleted = true;
            } else {
                path = Some(strip_git_prefix(p));
            }
        } else if line.starts_with("new file mode ") {
            is_new = true;
        } else if line.starts_with("deleted file mode ") {
            is_deleted = true;
        } else if line.starts_with("rename from ") || line.starts_with("rename to ") {
            is_pure_rename = true;
        } else if line.starts_with("@@") {
            saw_hunk = true;
        }
    }
    if saw_combined {
        return None;
    }
    if !saw_hunk && !is_pure_rename {
        // No hunks and not a rename: try harder — could be binary or
        // mode-only. We still emit it (path-filtering may drop it).
        // Fall through with path set from header.
    }
    if saw_hunk {
        is_pure_rename = false;
    }
    let path = path
        .or(old_path)
        .or_else(|| paths_from_header(body_without_header_prefix).map(|(_, b)| b))?
        .to_string();

    let mut body = String::with_capacity(13 + body_without_header_prefix.len());
    body.push_str("diff --git ");
    body.push_str(body_without_header_prefix);

    Some(ParsedFile {
        path,
        is_new,
        is_deleted,
        is_pure_rename,
        additions,
        deletions,
        body,
        context: None,
    })
}

#[inline]
fn strip_git_prefix(s: &str) -> &str {
    s.strip_prefix("a/")
        .or_else(|| s.strip_prefix("b/"))
        .unwrap_or(s)
        .trim_end_matches(['\t', ' '])
}

/// Best-effort fallback path extraction from the `diff --git a/X b/Y` line
/// itself, used when `---`/`+++` are missing (binary, mode-only).
/// `body` is the file slice *without* the leading `diff --git ` prefix.
fn paths_from_header(body: &str) -> Option<(&str, &str)> {
    let first = body.split('\n').next()?;
    // first is like "a/X b/Y" (no "diff --git " prefix)
    let sep = first.find(" b/")?;
    let a = first[..sep].strip_prefix("a/").unwrap_or(&first[..sep]);
    let b = &first[sep + 3..];
    Some((a, b))
}

/// Return true when `path` matches any of the ignored patterns.
///
/// `patterns` are interpreted as substrings *or* exact basename matches
/// (no glob syntax — that's enough for the real ignore list and avoids
/// pulling a glob crate). Strings ending in `/` match directory prefixes;
/// strings starting with `.` are also tried as basename suffixes (e.g.
/// `.min.js` matches `dist/app.min.js`).
#[inline]
pub fn is_ignored(path: &str, patterns: &[&str]) -> bool {
    let basename = path.rsplit('/').next().unwrap_or(path);
    for &pat in patterns {
        if let Some(dir) = pat.strip_suffix('/') {
            if path == dir {
                return true;
            }
            if path.starts_with(dir) {
                if let Some(rest) = path.get(dir.len()..) {
                    if rest.starts_with('/') {
                        return true;
                    }
                }
            }
            if path.contains(&format!("/{dir}/")) {
                return true;
            }
        } else if pat.starts_with('.') {
            if basename.ends_with(pat) {
                return true;
            }
        } else if basename == pat || path.contains(pat) {
            return true;
        }
    }
    false
}

/// Attach post-PR file contents to the parsed file list. `lookup` is
/// called once per file, given its repo-relative path; returning
/// `Some(content)` populates [`ParsedFile::context`], `None` leaves it
/// unset. Skips deleted files (no head-side content exists). Mutates in
/// place and returns the same vector so calls can be chained.
///
/// Typical caller pattern (eval harness):
/// ```ignore
/// let files = parse_unified_diff(&diff);
/// let files = attach_file_contexts(files, |path| files_map.get(path).cloned());
/// ```
pub fn attach_file_contexts<F>(mut files: Vec<ParsedFile>, mut lookup: F) -> Vec<ParsedFile>
where
    F: FnMut(&str) -> Option<String>,
{
    for f in &mut files {
        if f.is_deleted {
            continue;
        }
        if let Some(content) = lookup(&f.path) {
            f.context = Some(content);
        }
    }
    files
}

/// Filter the parsed file list, dropping anything matching the ignore
/// patterns. Returns `(kept, dropped_paths)` so the CLI can report what was
/// elided.
pub fn filter_ignored(files: Vec<ParsedFile>, patterns: &[&str]) -> (Vec<ParsedFile>, Vec<String>) {
    let mut kept = Vec::with_capacity(files.len());
    let mut dropped = Vec::new();
    for f in files {
        if is_ignored(&f.path, patterns) {
            dropped.push(f.path);
        } else {
            kept.push(f);
        }
    }
    (kept, dropped)
}

/// Pack the parsed files into chunks for the LLM.
///
/// Greedy strategy:
/// * Truncate individual files exceeding `policy.max_file_bytes` (middle
///   elision with a marker).
/// * Append files to the current chunk while
///   `current_bytes + file.body.len() <= policy.max_chunk_bytes`.
/// * A single file larger than the chunk budget is emitted on its own
///   (post-truncation it fits by construction).
///
/// The `related` argument supplies helper files the diff *references*
/// but doesn't modify; the same list is shared across chunks, with the
/// per-chunk budget re-applied for each.
///
/// Related-file truncation:
/// * Each file is truncated to `policy.max_related_context_bytes`
///   (middle elision with `[truncated]` marker), then
/// * Files are appended to the chunk's related set in declaration order
///   while the cumulative byte total stays under
///   `policy.max_chunk_related_bytes`. Files past the budget are
///   dropped — silently from the model's view, but the harness still
///   knows what it asked for and logs are still in tact.
///
/// For callers that own the file list (typically after [`filter_ignored`]),
/// see [`chunk_for_review_owned`] — it moves bodies instead of cloning
/// them.
pub fn chunk_for_review(
    files: &[ParsedFile],
    related: &[RelatedFile],
    policy: ChunkPolicy,
) -> Vec<DiffChunk> {
    let mut chunks: Vec<DiffChunk> = Vec::new();
    let mut cur_files: Vec<ParsedFile> = Vec::new();
    let mut cur_body = String::new();
    let mut cur_context_bytes: usize = 0;

    for f in files {
        let body = truncate_body(&f.body, policy.max_file_bytes);
        // Context budgeting: truncate per-file first, then drop entirely
        // if the chunk-level sum would blow the budget. Diff body always
        // ships — context is the only thing the chunker may discard.
        let context = match &f.context {
            None => None,
            Some(ctx) => {
                let truncated = truncate_body(ctx, policy.max_file_context_bytes);
                if cur_context_bytes + truncated.len() > policy.max_chunk_context_bytes {
                    None
                } else {
                    cur_context_bytes += truncated.len();
                    Some(truncated)
                }
            }
        };
        let f_trunc = ParsedFile {
            path: f.path.clone(),
            is_new: f.is_new,
            is_deleted: f.is_deleted,
            is_pure_rename: f.is_pure_rename,
            additions: f.additions,
            deletions: f.deletions,
            body,
            context,
        };
        let projected = cur_body.len() + f_trunc.body.len() + 1;
        if !cur_files.is_empty() && projected > policy.max_chunk_bytes {
            chunks.push(DiffChunk {
                files: std::mem::take(&mut cur_files),
                bytes: cur_body.len(),
                body: std::mem::take(&mut cur_body),
                related: pack_related(related, policy),
            });
            cur_context_bytes = 0;
        }
        if !cur_body.is_empty() {
            cur_body.push('\n');
        }
        cur_body.push_str(&f_trunc.body);
        cur_files.push(f_trunc);
    }
    if !cur_files.is_empty() {
        chunks.push(DiffChunk {
            files: cur_files,
            bytes: cur_body.len(),
            body: cur_body,
            related: pack_related(related, policy),
        });
    }
    chunks
}

/// Owned variant of [`chunk_for_review`] that takes `Vec<ParsedFile>`
/// by value and **moves** bodies instead of cloning them. For callers
/// that already own the file list (e.g. after [`filter_ignored`]) this
/// avoids a full copy of every file body when no truncation is needed.
pub fn chunk_for_review_owned(
    files: Vec<ParsedFile>,
    related: &[RelatedFile],
    policy: ChunkPolicy,
) -> Vec<DiffChunk> {
    let mut chunks: Vec<DiffChunk> = Vec::new();
    let mut cur_files: Vec<ParsedFile> = Vec::new();
    let mut cur_body = String::new();
    let mut cur_context_bytes: usize = 0;

    for mut f in files {
        let body = if f.body.len() <= policy.max_file_bytes {
            std::mem::take(&mut f.body)
        } else {
            truncate_body(&f.body, policy.max_file_bytes)
        };
        let context = match f.context {
            None => None,
            Some(ctx) => {
                let truncated = if ctx.len() <= policy.max_file_context_bytes {
                    ctx
                } else {
                    truncate_body(&ctx, policy.max_file_context_bytes)
                };
                if cur_context_bytes + truncated.len() > policy.max_chunk_context_bytes {
                    None
                } else {
                    cur_context_bytes += truncated.len();
                    Some(truncated)
                }
            }
        };
        let f_trunc = ParsedFile {
            path: f.path,
            is_new: f.is_new,
            is_deleted: f.is_deleted,
            is_pure_rename: f.is_pure_rename,
            additions: f.additions,
            deletions: f.deletions,
            body,
            context,
        };
        let projected = cur_body.len() + f_trunc.body.len() + 1;
        if !cur_files.is_empty() && projected > policy.max_chunk_bytes {
            chunks.push(DiffChunk {
                files: std::mem::take(&mut cur_files),
                bytes: cur_body.len(),
                body: std::mem::take(&mut cur_body),
                related: pack_related(related, policy),
            });
            cur_context_bytes = 0;
        }
        if !cur_body.is_empty() {
            cur_body.push('\n');
        }
        cur_body.push_str(&f_trunc.body);
        cur_files.push(f_trunc);
    }
    if !cur_files.is_empty() {
        chunks.push(DiffChunk {
            files: cur_files,
            bytes: cur_body.len(),
            body: cur_body,
            related: pack_related(related, policy),
        });
    }
    chunks
}

fn pack_related(related: &[RelatedFile], policy: ChunkPolicy) -> Vec<RelatedFile> {
    if related.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(related.len());
    let mut bytes: usize = 0;
    for rf in related {
        let truncated = truncate_body(&rf.content, policy.max_related_context_bytes);
        if bytes + truncated.len() > policy.max_chunk_related_bytes {
            // Drop silently; the diff + per-file context still ship.
            continue;
        }
        bytes += truncated.len();
        out.push(RelatedFile {
            path: rf.path.clone(),
            content: truncated,
        });
    }
    out
}

#[inline]
fn truncate_body(body: &str, max_bytes: usize) -> String {
    if body.len() <= max_bytes {
        return body.to_string();
    }
    // Keep head + tail, drop the middle. Charset is ASCII-ish in diffs, so
    // splitting on bytes is acceptable; we still nudge to the next newline
    // to avoid landing mid-line.
    let half = max_bytes / 2;
    let head_end = body.get(..half).and_then(|s| s.rfind('\n')).unwrap_or(half);
    let tail_start_min = body.len().saturating_sub(half);
    let tail_start = body[tail_start_min..]
        .find('\n')
        .map(|p| tail_start_min + p + 1)
        .unwrap_or(tail_start_min);
    let head = &body[..head_end];
    let tail = &body[tail_start..];
    let elided = body.len() - head.len() - tail.len();
    format!("{head}\n... [truncated {elided} bytes — file too large] ...\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"diff --git a/src/auth.rs b/src/auth.rs
index 0000001..0000002 100644
--- a/src/auth.rs
+++ b/src/auth.rs
@@ -10,3 +10,4 @@ fn login() {
 let user = decode_jwt(token);
-let _ = user;
+let _ = user.unwrap();
+println!("logged in");
diff --git a/Cargo.lock b/Cargo.lock
index 0000003..0000004 100644
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1,2 +1,3 @@
 # auto generated
-old
+new
+new2
diff --git a/docs/readme.md b/docs/readme.md
deleted file mode 100644
index 0000005..0000000
--- a/docs/readme.md
+++ /dev/null
@@ -1,2 +0,0 @@
-line one
-line two
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
"#;

    #[test]
    fn parses_modify_delete_rename_and_lockfile() {
        let files = parse_unified_diff(SAMPLE);
        assert_eq!(files.len(), 4);
        let by_path: std::collections::HashMap<&str, &ParsedFile> =
            files.iter().map(|f| (f.path.as_str(), f)).collect();

        let auth = by_path["src/auth.rs"];
        assert_eq!(auth.additions, 2);
        assert_eq!(auth.deletions, 1);
        assert!(!auth.is_new);
        assert!(!auth.is_deleted);

        let lock = by_path["Cargo.lock"];
        assert_eq!(lock.additions, 2);
        assert_eq!(lock.deletions, 1);

        let readme = by_path["docs/readme.md"];
        assert!(readme.is_deleted);
        assert_eq!(readme.deletions, 2);

        let renamed = by_path["new.txt"];
        assert!(renamed.is_pure_rename);
        assert_eq!(renamed.additions, 0);
        assert_eq!(renamed.deletions, 0);
    }

    #[test]
    fn ignored_filter_drops_lockfile_and_keeps_source() {
        let files = parse_unified_diff(SAMPLE);
        let (kept, dropped) = filter_ignored(files, DEFAULT_IGNORED_PATTERNS);
        let kept_paths: Vec<&str> = kept.iter().map(|f| f.path.as_str()).collect();
        assert!(kept_paths.contains(&"src/auth.rs"));
        assert!(kept_paths.contains(&"docs/readme.md"));
        assert!(dropped.iter().any(|p| p == "Cargo.lock"));
    }

    #[test]
    fn ignored_recognises_dir_prefixes() {
        assert!(is_ignored(
            "node_modules/foo/bar.js",
            DEFAULT_IGNORED_PATTERNS
        ));
        assert!(is_ignored(
            "vendor/github.com/x/y.go",
            DEFAULT_IGNORED_PATTERNS
        ));
        assert!(is_ignored("target/release/foo", DEFAULT_IGNORED_PATTERNS));
        assert!(!is_ignored("src/main.rs", DEFAULT_IGNORED_PATTERNS));
    }

    #[test]
    fn ignored_recognises_dot_suffix() {
        assert!(is_ignored("services/auth.pb.go", DEFAULT_IGNORED_PATTERNS));
        assert!(is_ignored("dist/app.min.js", DEFAULT_IGNORED_PATTERNS));
        assert!(!is_ignored("services/auth.go", DEFAULT_IGNORED_PATTERNS));
    }

    #[test]
    fn chunking_packs_small_files_together_and_emits_large_alone() {
        let small = ParsedFile {
            path: "a.rs".into(),
            is_new: false,
            is_deleted: false,
            is_pure_rename: false,
            additions: 1,
            deletions: 0,
            body: "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-x\n+y\n".into(),
            context: None,
        };
        let mut big = small.clone();
        big.path = "b.rs".into();
        big.body = format!(
            "diff --git a/b.rs b/b.rs\n@@ -1 +1 @@\n{}",
            "+y\n".repeat(2000) // ~6 KB
        );
        let policy = ChunkPolicy {
            max_chunk_bytes: 1024,
            max_file_bytes: 1024,
            ..ChunkPolicy::default()
        };
        let chunks = chunk_for_review(&[small.clone(), big, small.clone()], &[], policy);
        // Small files: one fits with the (truncated) big file? No — big > budget so
        // it ends up alone in its own chunk. Then small + small fit together.
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(c.bytes <= policy.max_chunk_bytes + policy.max_file_bytes);
        }
    }

    #[test]
    fn truncate_body_marks_elision_for_oversize() {
        let body = "x\n".repeat(10_000); // 20 KB
        let out = truncate_body(&body, 2048);
        assert!(out.contains("[truncated"));
        assert!(out.len() < body.len());
    }

    #[test]
    fn parse_tolerates_crlf() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        let files = parse_unified_diff(&crlf);
        assert_eq!(files.len(), 4);
    }

    #[test]
    fn parse_rejects_combined_diff() {
        let combined = "diff --cc src/x.rs\n--- a/x\n+++ b/x\n@@@ -1,1 -1,1 +1,1 @@@\n";
        let files = parse_unified_diff(combined);
        assert!(files.is_empty());
    }

    #[test]
    fn attach_file_contexts_populates_only_present_paths_and_skips_deleted() {
        let files = parse_unified_diff(SAMPLE);
        let mut probed: Vec<String> = Vec::new();
        let attached = attach_file_contexts(files, |path| {
            probed.push(path.to_string());
            match path {
                "src/auth.rs" => Some("fn login() { /* full file */ }\n".to_string()),
                // No content for Cargo.lock or new.txt — should leave None.
                _ => None,
            }
        });
        let by_path: std::collections::HashMap<&str, &ParsedFile> =
            attached.iter().map(|f| (f.path.as_str(), f)).collect();
        assert_eq!(
            by_path["src/auth.rs"].context.as_deref(),
            Some("fn login() { /* full file */ }\n")
        );
        assert!(by_path["Cargo.lock"].context.is_none());
        assert!(by_path["new.txt"].context.is_none());
        // docs/readme.md is deleted — lookup must never be called for it.
        assert!(
            !probed.iter().any(|p| p == "docs/readme.md"),
            "deleted files must be skipped, got probed = {probed:?}"
        );
    }

    #[test]
    fn chunk_truncates_oversized_file_context() {
        let mut f = ParsedFile {
            path: "src/big.rs".into(),
            is_new: false,
            is_deleted: false,
            is_pure_rename: false,
            additions: 1,
            deletions: 0,
            body: "diff --git a/src/big.rs b/src/big.rs\n@@ -1 +1 @@\n-x\n+y\n".into(),
            context: None,
        };
        // 40 KB of file context — well above the 8 KB per-file ceiling
        // we configure on the policy below.
        f.context = Some("println!(\"hi\");\n".repeat(2500));
        let policy = ChunkPolicy {
            max_chunk_bytes: 200 * 1024,
            max_file_bytes: 200 * 1024,
            max_file_context_bytes: 8 * 1024,
            max_chunk_context_bytes: 64 * 1024,
            ..ChunkPolicy::default()
        };
        let chunks = chunk_for_review(&[f], &[], policy);
        assert_eq!(chunks.len(), 1);
        let ctx = chunks[0].files[0]
            .context
            .as_deref()
            .expect("context must survive");
        assert!(ctx.contains("[truncated"), "ctx should be truncated");
        assert!(ctx.len() <= 8 * 1024 + 128, "ctx length should respect cap");
    }

    #[test]
    fn chunk_for_review_with_related_attaches_to_every_chunk() {
        let mk_file = |path: &str| ParsedFile {
            path: path.into(),
            is_new: false,
            is_deleted: false,
            is_pure_rename: false,
            additions: 1,
            deletions: 0,
            body: format!(
                "diff --git a/{path} b/{path}\n@@ -1 +1 @@\n-x\n+y\n{filler}",
                filler = "+".repeat(50)
            ),
            context: None,
        };
        // Two files large enough that they end up in separate chunks.
        let files = vec![mk_file("a.rs"), mk_file("b.rs")];
        let related = vec![
            RelatedFile {
                path: "lib/helper.rs".into(),
                content: "pub fn helper() {}\n".into(),
            },
            RelatedFile {
                path: "lib/util.rs".into(),
                content: "pub fn util() {}\n".into(),
            },
        ];
        let policy = ChunkPolicy {
            max_chunk_bytes: 80,
            max_file_bytes: 4096,
            ..ChunkPolicy::default()
        };
        let chunks = chunk_for_review(&files, &related, policy);
        assert!(chunks.len() >= 2, "files should split into separate chunks");
        for c in &chunks {
            assert_eq!(c.related.len(), 2, "every chunk carries the related set");
            assert!(c.related.iter().any(|r| r.path == "lib/helper.rs"));
            assert!(c.related.iter().any(|r| r.path == "lib/util.rs"));
        }
    }

    #[test]
    fn related_files_get_truncated_past_per_file_cap() {
        let files = vec![ParsedFile {
            path: "a.rs".into(),
            is_new: false,
            is_deleted: false,
            is_pure_rename: false,
            additions: 1,
            deletions: 0,
            body: "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-x\n+y\n".into(),
            context: None,
        }];
        let big = RelatedFile {
            path: "huge.rs".into(),
            content: "x\n".repeat(10_000), // 20 KB
        };
        let policy = ChunkPolicy {
            max_related_context_bytes: 1024,
            max_chunk_related_bytes: 4096,
            ..ChunkPolicy::default()
        };
        let chunks = chunk_for_review(&files, &[big], policy);
        assert_eq!(chunks.len(), 1);
        let r = &chunks[0].related[0];
        assert!(r.content.contains("[truncated"));
        assert!(r.content.len() <= 1024 + 128);
    }

    #[test]
    fn related_files_past_chunk_budget_are_dropped_silently() {
        let files = vec![ParsedFile {
            path: "a.rs".into(),
            is_new: false,
            is_deleted: false,
            is_pure_rename: false,
            additions: 1,
            deletions: 0,
            body: "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-x\n+y\n".into(),
            context: None,
        }];
        // Three 5KB files; chunk budget caps at 12KB → third drops.
        let rel = |name: &str, bytes: usize| RelatedFile {
            path: name.into(),
            content: "x".repeat(bytes),
        };
        let related = vec![rel("a.rs", 5_000), rel("b.rs", 5_000), rel("c.rs", 5_000)];
        let policy = ChunkPolicy {
            max_related_context_bytes: 10_000,
            max_chunk_related_bytes: 12_000,
            ..ChunkPolicy::default()
        };
        let chunks = chunk_for_review(&files, &related, policy);
        let paths: Vec<&str> = chunks[0].related.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn chunk_drops_context_when_chunk_budget_exceeded() {
        let mk = |path: &str, ctx_bytes: usize| ParsedFile {
            path: path.into(),
            is_new: false,
            is_deleted: false,
            is_pure_rename: false,
            additions: 1,
            deletions: 0,
            body: format!("diff --git a/{path} b/{path}\n@@ -1 +1 @@\n-x\n+y\n"),
            context: Some("x".repeat(ctx_bytes)),
        };
        let files = vec![
            mk("a.rs", 5_000),
            mk("b.rs", 5_000),
            mk("c.rs", 5_000), // would tip us past the 12 KB chunk-context cap
        ];
        let policy = ChunkPolicy {
            max_chunk_bytes: 200 * 1024,
            max_file_bytes: 200 * 1024,
            max_file_context_bytes: 10 * 1024,
            max_chunk_context_bytes: 12 * 1024,
            ..ChunkPolicy::default()
        };
        let chunks = chunk_for_review(&files, &[], policy);
        assert_eq!(chunks.len(), 1, "all three diffs fit in one chunk");
        let ctxs: Vec<bool> = chunks[0]
            .files
            .iter()
            .map(|f| f.context.is_some())
            .collect();
        assert_eq!(
            ctxs,
            vec![true, true, false],
            "a.rs + b.rs keep context, c.rs drops it (diff still ships)"
        );
        // The diffs themselves must always survive — context dropping is
        // a soft degradation, not a hard one.
        for f in &chunks[0].files {
            assert!(f.body.contains("@@"), "diff body must remain intact");
        }
    }
}
