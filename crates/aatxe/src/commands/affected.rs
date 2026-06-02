//! `aatxe affected` — print the affected bench files for a given diff base.

use crate::adapter::real_fs::{RealFs, RealGit};
use crate::cli::AffectedArgs;
use aatxe_core::affected::{resolve_affected, AffectedOptions};
use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn execute(args: AffectedArgs) -> Result<()> {
    let cwd = args.cwd.unwrap_or_else(|| std::env::current_dir().unwrap());
    let fs = RealFs;
    let git = RealGit;
    let set = resolve_affected(&AffectedOptions {
        cwd: cwd.clone(),
        base: args.base.clone(),
        language: args.lang.to_core(),
        patterns: args.patterns,
        extra_changed_files: vec![],
        git: &git,
        fs: &fs,
    })?;
    let affected_set: HashSet<PathBuf> = set.bench_files.iter().cloned().collect();
    if args.show_all {
        for p in &set.all_bench_files {
            let marker = if affected_set.contains(p) { "*" } else { " " };
            println!("{marker} {}", rel(&cwd, p));
        }
    } else {
        for p in &set.bench_files {
            println!("{}", rel(&cwd, p));
        }
    }
    eprintln!(
        "aatxe affected: {}/{} bench file(s) affected; {} file(s) changed since {}",
        set.bench_files.len(),
        set.all_bench_files.len(),
        set.changed_files.len(),
        set.base,
    );
    Ok(())
}

fn rel(cwd: &Path, p: &Path) -> String {
    p.strip_prefix(cwd)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}
