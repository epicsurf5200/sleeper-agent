//! Stamps build provenance into the binary so the GUI's Settings tab can show
//! exactly which build is running. The crate version alone is not enough —
//! it rarely changes between builds, and "am I running a stale binary?" is
//! the question this is actually here to answer.

use std::process::Command;

fn main() {
    // Re-run when HEAD moves, so the stamp does not go stale mid-branch.
    // Missing (e.g. building from a source tarball) is fine; the values just
    // fall back to "unknown" below.
    for p in [".git/HEAD", ".git/refs/heads"] {
        println!("cargo:rerun-if-changed={p}");
    }

    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };

    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    // A dirty tree means the binary does not correspond to any commit, which
    // is worth seeing in the UI rather than being told a misleading hash.
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let commit = if dirty { format!("{commit}-dirty") } else { commit };

    // Commit date rather than build date: reproducible, and it answers "how
    // old is this code?" which is the thing you actually want to know.
    let date = git(&["log", "-1", "--format=%cd", "--date=format:%Y-%m-%d"])
        .unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=SA_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=SA_GIT_DATE={date}");
}
