//! Production [`Fs`] and [`GitRunner`] implementations.
//!
//! Tests use in-memory impls in `crates/aatxe-core/tests/affected_tests.rs`.

use aatxe_core::affected::{AffectedError, DirEntry, EntryKind, Fs, GitRunner};
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct RealFs;

impl Fs for RealFs {
    fn read_to_string(&self, path: &Path) -> Result<String, AffectedError> {
        fs::read_to_string(path).map_err(|e| AffectedError::Io(e.to_string()))
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>, AffectedError> {
        let it = fs::read_dir(path).map_err(|e| AffectedError::Io(e.to_string()))?;
        let mut out: Vec<DirEntry> = Vec::new();
        for e in it {
            let entry = e.map_err(|err| AffectedError::Io(err.to_string()))?;
            let ft = entry
                .file_type()
                .map_err(|err| AffectedError::Io(err.to_string()))?;
            let kind = if ft.is_dir() {
                EntryKind::Dir
            } else if ft.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            out.push(DirEntry {
                path: entry.path(),
                kind,
            });
        }
        Ok(out)
    }

    fn metadata(&self, path: &Path) -> Result<EntryKind, AffectedError> {
        match fs::metadata(path) {
            Ok(m) if m.is_dir() => Ok(EntryKind::Dir),
            Ok(m) if m.is_file() => Ok(EntryKind::File),
            Ok(_) => Ok(EntryKind::Other),
            Err(e) => Err(AffectedError::Io(e.to_string())),
        }
    }
}

pub struct RealGit;

impl GitRunner for RealGit {
    fn run(&self, args: &[&str], cwd: &Path) -> Result<String, AffectedError> {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|e| AffectedError::GitFailed(e.to_string()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            return Err(AffectedError::GitFailed(format!(
                "git {} exited {}: {}",
                args.join(" "),
                out.status.code().unwrap_or(-1),
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}
