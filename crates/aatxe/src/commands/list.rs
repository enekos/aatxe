//! `aatxe list` — print bench files that would run, without running them.

use crate::adapter::real_fs::RealFs;
use crate::cli::ListArgs;
use aatxe_core::affected::{DirEntry, EntryKind, Fs, GlobMatcher};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn execute(args: ListArgs) -> Result<()> {
    let cwd = args.cwd.unwrap_or_else(|| std::env::current_dir().unwrap());
    let fs = RealFs;
    let lang = args.lang.to_core();
    let mut globs: Vec<&str> = args.patterns.iter().map(|s| s.as_str()).collect();
    if globs.is_empty() {
        globs.extend(lang.default_globs().iter().copied());
    }
    let matchers: Vec<GlobMatcher> = globs.iter().map(|g| GlobMatcher::new(g)).collect();
    let excludes = ["node_modules", "dist", "build", ".git", "target", "vendor"];
    let mut out: Vec<PathBuf> = Vec::new();
    walk(&cwd, &fs, &matchers, &excludes, &mut out);
    out.sort();
    out.dedup();
    for p in &out {
        let rel = p
            .strip_prefix(&cwd)
            .map(|r| r.to_path_buf())
            .unwrap_or(p.clone());
        println!("{}", rel.display());
    }
    eprintln!(
        "aatxe list: {} bench file(s) (lang={})",
        out.len(),
        lang.label()
    );
    Ok(())
}

fn walk(
    root: &Path,
    fs: &dyn Fs,
    matchers: &[GlobMatcher],
    excludes: &[&str],
    out: &mut Vec<PathBuf>,
) {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs.read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries {
            let DirEntry { path, kind } = e;
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if excludes.iter().any(|x| x == &name) {
                continue;
            }
            match kind {
                EntryKind::Dir => stack.push(path),
                EntryKind::File => {
                    if matchers.iter().any(|m| m.matches(&path)) {
                        out.push(path);
                    }
                }
                EntryKind::Other => {}
            }
        }
    }
}
