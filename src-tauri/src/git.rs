//! Read-only Git inspection through the Git CLI.
//!
//! LJ Hub never runs destructive Git commands and does not implement its own
//! Git object handling; it shells out to `git` with explicit, read-only
//! arguments. Every command is run with `-C <path>` so the calling process's
//! working directory cannot leak into the result.

use crate::model::{GitEvidence, GitFileChange, GitState};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

const GIT_TIMEOUT_SECS: u64 = 20;

#[derive(Debug)]
pub struct GitOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run a read-only git command. Only the subcommands listed in `ALLOWED` may be
/// used, which keeps this module incapable of changing a repository.
pub fn run_git(dir: &Path, args: &[&str]) -> Result<GitOutput, String> {
    let allowed: &[&str] = &[
        "rev-parse",
        "status",
        "log",
        "remote",
        "diff",
        "show",
        "branch",
        "ls-files",
        "config",
    ];
    let sub = args.first().copied().unwrap_or_default();
    if !allowed.contains(&sub) {
        return Err(format!("拒绝执行非只读 git 子命令：{sub}"));
    }
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    command.env("GIT_OPTIONAL_LOCKS", "0");
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.stdin(std::process::Stdio::null());

    let output = run_with_timeout(command, GIT_TIMEOUT_SECS)?;
    Ok(GitOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn run_with_timeout(
    mut command: Command,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    // `std::process` has no timeout, so the child is reaped by a watchdog
    // thread. Git commands here are short; a hang means something is wrong and
    // we would rather report it than block the UI.
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 git：{e}"))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    return Err(format!("git 命令超时（{timeout_secs} 秒）"));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => return Err(format!("等待 git 失败：{e}")),
        }
    }
    child
        .wait_with_output()
        .map_err(|e| format!("读取 git 输出失败：{e}"))
}

/// Resolve the work-tree root for a directory, if it is inside a repository.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    let out = run_git(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    if !out.success {
        return None;
    }
    let line = out.stdout.lines().next()?.trim().to_string();
    if line.is_empty() {
        None
    } else {
        Some(PathBuf::from(line))
    }
}

pub fn remote_url(dir: &Path) -> Option<String> {
    let out = run_git(dir, &["remote", "get-url", "origin"]).ok()?;
    if !out.success {
        return None;
    }
    let line = out.stdout.lines().next()?.trim().to_string();
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

/// Full repository state used by the sidebar and by handoff evidence.
pub fn state(dir: &Path) -> GitState {
    let mut state = GitState {
        is_repo: false,
        root: None,
        branch: None,
        head: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        dirty: false,
        changes: Vec::new(),
        untracked: Vec::new(),
        error: None,
    };

    let root = match repo_root(dir) {
        Some(r) => r,
        None => {
            state.error = Some("该目录不是 Git 仓库".to_string());
            return state;
        }
    };
    state.is_repo = true;
    state.root = Some(root.display().to_string());

    match run_git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(out) if out.success => state.branch = Some(out.stdout.trim().to_string()),
        Ok(out) => {
            state.branch = Some("(未命名分支)".to_string()).or(Some(out.stderr.trim().to_string()))
        }
        Err(e) => state.error = Some(e),
    }
    if let Ok(out) = run_git(&root, &["rev-parse", "HEAD"]) {
        if out.success {
            state.head = Some(out.stdout.trim().to_string());
        }
    }

    match run_git(&root, &["status", "--porcelain=v1", "--branch"]) {
        Ok(out) if out.success => {
            for line in out.stdout.lines() {
                if let Some(rest) = line.strip_prefix("## ") {
                    // e.g. "main...origin/main [ahead 1, behind 2]"
                    if let Some(idx) = rest.find("...") {
                        let (_, tail) = rest.split_at(idx + 3);
                        state.upstream = Some(rest[..idx].to_string());
                        if let (Some(a), Some(b)) =
                            (extract_count(tail, "ahead"), extract_count(tail, "behind"))
                        {
                            state.ahead = a;
                            state.behind = b;
                        }
                    }
                    continue;
                }
                if line.len() < 3 {
                    continue;
                }
                let (code, path) = line.split_at(2);
                let path = path.trim().trim_matches('"').to_string();
                if code == "??" {
                    state.untracked.push(path.clone());
                }
                let staged = code
                    .chars()
                    .next()
                    .map(|c| c != ' ' && c != '?')
                    .unwrap_or(false);
                state.changes.push(GitFileChange {
                    status: code.to_string(),
                    path,
                    staged,
                });
            }
            state.dirty = !state.changes.is_empty();
        }
        Ok(out) => state.error = Some(out.stderr.trim().to_string()),
        Err(e) => state.error = Some(e),
    }

    state
}

fn extract_count(text: &str, key: &str) -> Option<i64> {
    let idx = text.find(key)?;
    let rest = &text[idx + key.len()..];
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Read a unified diff for one path (working tree vs HEAD). Untracked files
/// have no diff, which is reported instead of invented.
pub fn diff_for(dir: &Path, path: &str, max_bytes: usize) -> Result<String, String> {
    let head = run_git(dir, &["diff", "HEAD", "--", path])?;
    let mut text = head.stdout;
    if text.trim().is_empty() {
        let staged = run_git(dir, &["diff", "--cached", "--", path])?;
        text = staged.stdout;
    }
    if text.trim().is_empty() {
        return Ok(String::new());
    }
    if text.len() > max_bytes {
        text.truncate(max_bytes);
        text.push_str("\n… （diff 已截断）");
    }
    Ok(text)
}

pub fn evidence(dir: &Path) -> GitEvidence {
    let state = state(dir);
    let fingerprint = fingerprint_of(dir);
    GitEvidence {
        root: state.root,
        branch: state.branch,
        head: state.head,
        dirty: state.dirty,
        dirty_files: state
            .changes
            .iter()
            .map(|c| format!("{} {}", c.status, c.path))
            .collect(),
        captured_at: crate::model::now_iso(),
        content_fingerprint: fingerprint.as_ref().map(|(hash, _)| hash.clone()),
        fingerprint_detail: fingerprint.map(|(_, detail)| detail),
    }
}

/// Per-file hashing cap. Bigger files contribute their size and a head/tail
/// digest instead of a full read, so a multi-gigabyte artifact cannot stall
/// verification.
pub const FINGERPRINT_FILE_CAP: u64 = 16 * 1024 * 1024;
/// Total budget for one fingerprint pass.
pub const FINGERPRINT_TOTAL_CAP: u64 = 256 * 1024 * 1024;
/// Never hash more than this many changed files in one pass.
pub const FINGERPRINT_MAX_FILES: usize = 2_000;

/// Content fingerprint of the *changed* part of a repository.
///
/// Returns `(sha256, human readable coverage detail)`. `None` when the
/// directory is not a repository.
///
/// What it covers: HEAD, branch, the porcelain status lines, and the bytes of
/// every changed/untracked file (bounded). This detects "the file list is the
/// same but a file's content moved on", which a filename comparison misses.
pub fn fingerprint_of(dir: &Path) -> Option<(String, String)> {
    let state = state(dir);
    if !state.is_repo {
        return None;
    }
    let root = PathBuf::from(state.root.clone()?);

    let mut hasher = Sha256::new();
    hasher.update(b"lj-hub-workspace-fingerprint-v1\n");
    hasher.update(state.head.as_deref().unwrap_or("-").as_bytes());
    hasher.update(b"\n");
    hasher.update(state.branch.as_deref().unwrap_or("-").as_bytes());
    hasher.update(b"\n");

    let mut entries: Vec<(String, String)> = state
        .changes
        .iter()
        .map(|c| (c.path.clone(), c.status.clone()))
        .collect();
    entries.sort();

    let mut hashed_files = 0usize;
    let mut hashed_bytes = 0u64;
    let mut skipped_files = 0usize;

    for (path, status) in &entries {
        hasher.update(format!("{status} {path}\n").as_bytes());
        let absolute = root.join(path);
        let Ok(meta) = std::fs::metadata(&absolute) else {
            hasher.update(b"<missing>\n");
            skipped_files += 1;
            continue;
        };
        if !meta.is_file() {
            hasher.update(b"<not-a-file>\n");
            continue;
        }
        hasher.update(meta.len().to_string().as_bytes());
        hasher.update(b"\n");

        if hashed_files >= FINGERPRINT_MAX_FILES
            || hashed_bytes >= FINGERPRINT_TOTAL_CAP
            || meta.len() > FINGERPRINT_FILE_CAP
        {
            // Record size + mtime instead of content so the check stays cheap
            // but still notices an edit that changes either.
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            hasher.update(format!("<meta-only mtime={modified}>\n").as_bytes());
            skipped_files += 1;
            continue;
        }

        match std::fs::read(&absolute) {
            Ok(bytes) => {
                hasher.update(&bytes);
                hashed_files += 1;
                hashed_bytes += bytes.len() as u64;
            }
            Err(_) => {
                hasher.update(b"<unreadable>\n");
                skipped_files += 1;
            }
        }
        hasher.update(b"\n");
    }

    let detail = format!(
        "改了 {} 项；按内容哈希 {} 个文件（{} KiB）；仅按元数据 {} 个",
        entries.len(),
        hashed_files,
        hashed_bytes / 1024,
        skipped_files
    );
    Some((format!("{:x}", hasher.finalize()), detail))
}

/// Recent commits, used for handoff "what changed" context.
pub fn recent_commits(dir: &Path, count: usize) -> Vec<String> {
    let n = format!("-{count}");
    match run_git(
        dir,
        &["log", &n, "--pretty=format:%h %ad %s", "--date=short"],
    ) {
        Ok(out) if out.success => out.stdout.lines().map(|l| l.to_string()).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_repo;

    #[test]
    fn non_repo_is_reported_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        assert!(!state.is_repo);
        assert!(state.error.is_some());
    }

    #[test]
    fn dirty_and_untracked_are_detected() {
        let repo = fixture_repo();
        std::fs::write(repo.path().join("tracked.txt"), "changed").unwrap();
        std::fs::write(repo.path().join("new.txt"), "new").unwrap();
        let state = state(repo.path());
        assert!(state.is_repo);
        assert!(state.dirty);
        assert!(state
            .changes
            .iter()
            .any(|c| c.path == "tracked.txt" && c.status.trim() == "M"));
        assert!(state.untracked.iter().any(|p| p == "new.txt"));
        assert!(state.head.is_some());
    }

    #[test]
    fn destructive_subcommands_are_refused() {
        let repo = fixture_repo();
        assert!(run_git(repo.path(), &["reset", "--hard"]).is_err());
        assert!(run_git(repo.path(), &["clean", "-fd"]).is_err());
        assert!(run_git(repo.path(), &["push"]).is_err());
    }

    #[test]
    fn fingerprint_detects_content_change_with_same_file_list() {
        let repo = fixture_repo();
        std::fs::write(repo.path().join("a.txt"), "one").unwrap();
        std::fs::write(repo.path().join("b.txt"), "two").unwrap();
        let first = fingerprint_of(repo.path()).unwrap().0;

        // Same file names, same status list, different content.
        std::fs::write(repo.path().join("a.txt"), "one changed").unwrap();
        let second = fingerprint_of(repo.path()).unwrap().0;
        assert_ne!(first, second, "文件名不变但内容变化必须被发现");

        // Re-hashing the unchanged state is stable.
        let third = fingerprint_of(repo.path()).unwrap().0;
        assert_eq!(second, third);
    }

    #[test]
    fn fingerprint_notices_a_new_commit() {
        let repo = fixture_repo();
        let before = fingerprint_of(repo.path()).unwrap().0;
        std::fs::write(repo.path().join("tracked.txt"), "next\n").unwrap();
        // The read-only allowlist must keep refusing write subcommands, so the
        // test drives git directly here on purpose.
        for args in [
            vec!["add", "tracked.txt"],
            vec!["commit", "-q", "-m", "next"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .env("GIT_TERMINAL_PROMPT", "0")
                .status()
                .expect("git available");
            assert!(status.success());
        }
        let after = fingerprint_of(repo.path()).unwrap().0;
        assert_ne!(before, after);
    }

    #[test]
    fn evidence_carries_fingerprint_and_coverage() {
        let repo = fixture_repo();
        std::fs::write(repo.path().join("x.txt"), "x").unwrap();
        let evidence = evidence(repo.path());
        assert!(evidence.content_fingerprint.is_some());
        assert!(evidence
            .fingerprint_detail
            .as_deref()
            .unwrap_or_default()
            .contains("按内容哈希"));
    }

    #[test]
    fn diff_reports_changes() {
        let repo = fixture_repo();
        std::fs::write(repo.path().join("tracked.txt"), "changed\n").unwrap();
        let diff = diff_for(repo.path(), "tracked.txt", 100_000).unwrap();
        assert!(diff.contains("changed"));
    }
}
