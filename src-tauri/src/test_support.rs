//! Test-only helpers shared by the Rust test suite.

use std::path::Path;
use std::process::Command;

/// Create a real (but disposable) Git repository with one commit, so Git
/// behaviour is exercised against the actual CLI instead of a mock.
pub fn fixture_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path();
    run(path, &["init", "-q", "-b", "main"]);
    run(path, &["config", "user.email", "test@example.invalid"]);
    run(path, &["config", "user.name", "LJ Hub Test"]);
    run(path, &["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("tracked.txt"), "original\n").unwrap();
    run(path, &["add", "tracked.txt"]);
    run(path, &["commit", "-q", "-m", "initial"]);
    dir
}

fn run(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .expect("git available");
    assert!(status.success(), "git {args:?} failed");
}
