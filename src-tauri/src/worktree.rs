//! Isolated Git worktrees for parallel runs.
//!
//! Design notes from auditing FreeBuddy (`electron/cli/taskWorkspace.ts`, MIT):
//!  - worktrees live under the application data directory, never inside the
//!    repository, and the path is containment-checked;
//!  - the task key is validated before it is used in a path;
//!  - creation fails loudly instead of reusing an existing directory.
//!
//! LJ Hub deliberately goes further in three places, because "two agents must
//! not silently overwrite each other" is an explicit product requirement:
//!  - a **named branch** is created per run (`--detach` would make the work
//!    unrecoverable and un-mergeable later);
//!  - the mapping project → repository → worktree → branch → run is recorded on
//!    disk, so the index survives restarts;
//!  - integration is an explicit, reversible step that refuses to run over a
//!    dirty target and reports conflicts instead of overwriting anything.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeRecord {
    pub id: String,
    /// Logical project this worktree belongs to.
    pub project_id: Option<String>,
    pub run_id: Option<String>,
    /// Repository the worktree was created from.
    pub repo_root: String,
    pub path: String,
    pub branch: String,
    /// Base commit the branch was created at, so drift is provable later.
    pub base_head: String,
    pub created_at: String,
    pub removed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedWorktree {
    pub path: PathBuf,
    pub branch: String,
    pub base_head: String,
}

fn run_git_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|e| format!("无法启动 git：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} 失败：{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Turn a title into a safe slug for a branch and directory name.
pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_alphanumeric() {
            // Keep CJK and other letters: they are valid in branch names.
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.chars().count() >= 40 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "run".to_string()
    } else {
        trimmed
    }
}

/// Create an isolated worktree for a run.
///
/// Refuses to touch anything on failure and never runs a destructive command.
pub fn create_for_run(
    repo_root: &Path,
    data_dir: &Path,
    title: &str,
) -> Result<CreatedWorktree, String> {
    if !repo_root.join(".git").exists() && crate::git::repo_root(repo_root).is_none() {
        return Err(format!(
            "{} 不是 Git 仓库，无法创建隔离工作区",
            repo_root.display()
        ));
    }
    // A dirty repository would make the worktree's starting point ambiguous.
    let status = crate::git::state(repo_root);
    if status.dirty {
        // Uncommitted work in the main tree belongs to the user (or another
        // agent). Newer git can still branch from HEAD, so only warn when the
        // changes overlap - we cannot know, so we refuse by default.
        return Err(format!(
            "主工作区有 {} 项未提交改动，为避免把别人的成果带进隔离区，已拒绝创建 worktree。\
             请先提交或暂存，再重试。",
            status.changes.len()
        ));
    }
    let base_head = crate::git::run_git(repo_root, &["rev-parse", "HEAD"])?
        .stdout
        .trim()
        .to_string();
    if base_head.is_empty() {
        return Err("仓库还没有提交，无法创建 worktree".to_string());
    }

    let slug = slugify(title);
    let id = crate::model::new_id("wt");
    let short = id.replace("wt-", "");
    let short = &short[..short.len().min(8)];
    let branch = format!("lj-hub/{slug}-{short}");

    let root_dir = data_dir.join("worktrees");
    std::fs::create_dir_all(&root_dir).map_err(|e| e.to_string())?;
    let path = root_dir.join(format!("{slug}-{short}"));
    if path.exists() {
        return Err(format!("隔离目录已存在，拒绝复用：{}", path.display()));
    }
    // Containment check: the resolved path must stay under our data directory.
    let canonical_root = root_dir.canonicalize().map_err(|e| e.to_string())?;
    let canonical_parent = path
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| "无法解析隔离目录父路径".to_string())?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(format!(
            "隔离目录不在应用数据目录内，已拒绝：{}",
            path.display()
        ));
    }

    run_git_in(
        repo_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().ok_or("路径包含非法字符")?,
            "HEAD",
        ],
    )
    .map_err(|e| format!("创建 worktree 失败：{e}"))?;

    Ok(CreatedWorktree {
        path,
        branch,
        base_head,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeStatus {
    pub record: WorktreeRecord,
    pub exists: bool,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty: bool,
    pub commits_ahead: i64,
    pub error: Option<String>,
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn inspect(record: &WorktreeRecord) -> WorktreeStatus {
    let path = PathBuf::from(&record.path);
    if !path.is_dir() {
        return WorktreeStatus {
            record: record.clone(),
            exists: false,
            branch: None,
            head: None,
            dirty: false,
            commits_ahead: 0,
            error: Some("隔离目录不存在（可能已被手动删除）".to_string()),
        };
    }
    let branch = git_out(&path, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let head = git_out(&path, &["rev-parse", "HEAD"]);
    let state = crate::git::state(&path);
    let ahead = git_out(
        &path,
        &[
            "rev-list",
            "--count",
            &format!("{}..HEAD", record.base_head),
        ],
    )
    .and_then(|s| s.parse::<i64>().ok())
    .unwrap_or(0);
    WorktreeStatus {
        record: record.clone(),
        exists: true,
        branch,
        head,
        dirty: state.dirty,
        commits_ahead: ahead,
        error: None,
    }
}

/// Persisted index of worktrees, so "which agent is working where" survives a
/// restart and a moved project can be re-pointed deliberately.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorktreeIndex {
    #[serde(default)]
    pub records: Vec<WorktreeRecord>,
}

impl WorktreeIndex {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("worktrees.json")
    }

    pub fn load(data_dir: &Path) -> Self {
        std::fs::read(Self::path(data_dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let path = Self::path(data_dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub fn insert(&mut self, record: WorktreeRecord) {
        self.records.retain(|r| r.id != record.id);
        self.records.push(record);
    }

    pub fn for_run(&self, run_id: &str) -> Vec<WorktreeRecord> {
        self.records
            .iter()
            .filter(|r| r.run_id.as_deref() == Some(run_id) && r.removed_at.is_none())
            .cloned()
            .collect()
    }
}

/// Result of trying to integrate a worktree branch back.
#[derive(Debug, Clone, Serialize)]
pub struct IntegrationReport {
    pub ok: bool,
    pub target_branch: String,
    pub source_branch: String,
    pub commits: Vec<String>,
    pub dirty_files: Vec<String>,
    pub message: String,
    /// True when git reported conflicts and the merge was aborted, leaving the
    /// target repository exactly as it was.
    pub conflicts: Vec<String>,
    pub aborted: bool,
}

/// Integrate a worktree branch into a target branch *without* ever leaving the
/// repository half-merged: a conflict is detected, reported, and aborted.
pub fn integrate(
    repo_root: &Path,
    source_branch: &str,
    target_branch: &str,
    dry_run: bool,
) -> Result<IntegrationReport, String> {
    let target_state = crate::git::state(repo_root);
    let current_branch = target_state
        .branch
        .clone()
        .ok_or_else(|| "无法确定当前分支（可能是 detached HEAD）".to_string())?;
    if current_branch != target_branch {
        return Err(format!(
            "当前分支是 {current_branch}，不是目标分支 {target_branch}；\
             为避免误操作，LJ Hub 不会自动切换分支。"
        ));
    }
    if target_state.dirty {
        return Err(format!(
            "目标工作区有 {} 项未提交改动，拒绝合并以免覆盖。",
            target_state.changes.len()
        ));
    }

    let commits = git_out(
        repo_root,
        &[
            "log",
            "--oneline",
            &format!("{target_branch}..{source_branch}"),
        ],
    )
    .unwrap_or_default();
    let commits: Vec<String> = commits
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if commits.is_empty() {
        return Ok(IntegrationReport {
            ok: true,
            target_branch: target_branch.to_string(),
            source_branch: source_branch.to_string(),
            commits: vec![],
            dirty_files: vec![],
            message: "没有需要集成的新提交".to_string(),
            conflicts: vec![],
            aborted: false,
        });
    }

    if dry_run {
        // `merge-tree --write-tree` answers "would this conflict" without
        // touching the index, refs or working tree: exit 0 means clean, exit 1
        // means conflicts, and the conflicted paths are listed in the output.
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["merge-tree", "--write-tree", target_branch, source_branch])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| format!("无法启动 git merge-tree：{e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let conflicts: Vec<String> = if output.status.success() {
            Vec::new()
        } else {
            stdout
                .lines()
                .filter_map(|line| line.split_once('\t').map(|(_, path)| path.to_string()))
                .collect()
        };
        return Ok(IntegrationReport {
            ok: conflicts.is_empty(),
            target_branch: target_branch.to_string(),
            source_branch: source_branch.to_string(),
            commits: commits.clone(),
            dirty_files: vec![],
            message: if conflicts.is_empty() {
                format!("可以合并：{} 个提交，未发现冲突", commits.len())
            } else {
                format!("发现 {} 处冲突，未做任何修改", conflicts.len())
            },
            conflicts,
            aborted: false,
        });
    }

    // Real merge. Any failure is followed by an abort, so the repository is
    // restored rather than left conflicted.
    let merge = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "merge",
            "--no-ff",
            "--no-commit",
            "-m",
            &format!("Merge {source_branch} into {target_branch} (LJ Hub run integration)"),
            source_branch,
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("无法启动 git merge：{e}"))?;

    if !merge.status.success() {
        let stdout = String::from_utf8_lossy(&merge.stdout).to_string();
        let stderr = String::from_utf8_lossy(&merge.stderr).to_string();
        let status = crate::git::state(repo_root);
        let conflicted: Vec<String> = status
            .changes
            .iter()
            .filter(|c| {
                c.status.contains('U') || c.status.contains("AA") || c.status.contains("DD")
            })
            .map(|c| c.path.clone())
            .collect();
        // Undo: never leave a half-merged tree behind.
        let aborted = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["merge", "--abort"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let after = crate::git::state(repo_root);
        return Ok(IntegrationReport {
            ok: false,
            target_branch: target_branch.to_string(),
            source_branch: source_branch.to_string(),
            commits,
            dirty_files: status.changes.iter().map(|c| c.path.clone()).collect(),
            message: format!(
                "合并失败{}：{}\n{}\n回滚后工作区改动 {} 项",
                if aborted {
                    "（已回滚到合并前状态）"
                } else {
                    "（未能自动回滚，请手动检查）"
                },
                stdout.trim(),
                stderr.trim(),
                after.changes.len()
            ),
            conflicts: conflicted,
            aborted,
        });
    }

    let dirty_files = crate::git::state(repo_root)
        .changes
        .iter()
        .map(|c| c.path.clone())
        .collect::<Vec<_>>();
    Ok(IntegrationReport {
        ok: true,
        target_branch: target_branch.to_string(),
        source_branch: source_branch.to_string(),
        commits,
        dirty_files,
        message: "合并成功，未自动提交：请审阅后自行提交（LJ Hub 不会替你提交）".to_string(),
        conflicts: vec![],
        aborted: false,
    })
}

/// Remove a worktree. Refuses when it still has uncommitted work unless
/// `discard_uncommitted` is explicitly set by the user.
pub fn remove(
    repo_root: &Path,
    record: &WorktreeRecord,
    discard_uncommitted: bool,
) -> Result<(), String> {
    let path = PathBuf::from(&record.path);
    if path.is_dir() {
        let status = inspect(record);
        if status.dirty && !discard_uncommitted {
            return Err(format!(
                "隔离工作区仍有未提交改动（{} 个文件），拒绝删除。请先处理，或明确选择丢弃。",
                crate::git::state(&path).changes.len()
            ));
        }
        if discard_uncommitted {
            run_git_in(repo_root, &["worktree", "remove", "--force", &record.path])
                .map_err(|e| format!("删除 worktree 失败：{e}"))?;
        } else {
            run_git_in(repo_root, &["worktree", "remove", &record.path])
                .map_err(|e| format!("删除 worktree 失败：{e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_repo;
    use tempfile::tempdir;

    #[test]
    fn slugify_is_safe_and_keeps_readability() {
        assert_eq!(slugify("Fix login bug"), "fix-login-bug");
        assert_eq!(slugify("修复 登录"), "修复-登录");
        assert_eq!(slugify("///"), "run");
    }

    #[test]
    fn worktree_is_created_with_a_branch_and_recorded() {
        let repo = fixture_repo();
        let data = tempdir().unwrap();
        let created = create_for_run(repo.path(), data.path(), "实现 登录").expect("create");
        assert!(created.path.is_dir());
        assert!(created.branch.starts_with("lj-hub/"));
        assert!(!created.base_head.is_empty());

        // It is a real worktree, on its own branch.
        let branch = git_out(&created.path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(branch, created.branch);

        // Writing in the worktree must not touch the main tree.
        std::fs::write(created.path.join("only-here.txt"), "x").unwrap();
        assert!(!repo.path().join("only-here.txt").exists());
    }

    #[test]
    fn dirty_main_tree_refuses_worktree_creation() {
        let repo = fixture_repo();
        std::fs::write(repo.path().join("tracked.txt"), "changed").unwrap();
        let data = tempdir().unwrap();
        let error = create_for_run(repo.path(), data.path(), "x").unwrap_err();
        assert!(error.contains("未提交改动"), "{error}");
    }

    #[test]
    fn inspection_reports_branch_head_and_drift() {
        let repo = fixture_repo();
        let data = tempdir().unwrap();
        let created = create_for_run(repo.path(), data.path(), "任务").unwrap();
        std::fs::write(created.path.join("new.txt"), "content").unwrap();
        for args in [vec!["add", "new.txt"], vec!["commit", "-q", "-m", "work"]] {
            let status = Command::new("git")
                .arg("-C")
                .arg(&created.path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
        let record = WorktreeRecord {
            id: "wt-1".into(),
            project_id: None,
            run_id: Some("run-1".into()),
            repo_root: repo.path().display().to_string(),
            path: created.path.display().to_string(),
            branch: created.branch.clone(),
            base_head: created.base_head.clone(),
            created_at: "now".into(),
            removed_at: None,
        };
        let status = inspect(&record);
        assert!(status.exists);
        assert_eq!(status.commits_ahead, 1);
        assert_eq!(status.branch.as_deref(), Some(created.branch.as_str()));
    }

    #[test]
    fn integration_refuses_a_dirty_target_and_reports_conflicts() {
        let repo = fixture_repo();
        let data = tempdir().unwrap();
        let created = create_for_run(repo.path(), data.path(), "改动").unwrap();

        // A conflicting change in the worktree...
        std::fs::write(created.path.join("tracked.txt"), "from worktree\n").unwrap();
        for args in [vec!["add", "tracked.txt"], vec!["commit", "-q", "-m", "wt"]] {
            assert!(Command::new("git")
                .arg("-C")
                .arg(&created.path)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        // ...and a different change on the main branch.
        std::fs::write(repo.path().join("tracked.txt"), "from main\n").unwrap();
        for args in [
            vec!["add", "tracked.txt"],
            vec!["commit", "-q", "-m", "main"],
        ] {
            assert!(Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .status()
                .unwrap()
                .success());
        }

        // Dry run must not modify anything.
        let before = git_out(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        let report = integrate(repo.path(), &created.branch, "main", true).unwrap();
        assert!(!report.ok, "应发现冲突：{}", report.message);
        assert_eq!(
            git_out(repo.path(), &["rev-parse", "HEAD"]).unwrap(),
            before
        );

        // A dirty target refuses outright.
        std::fs::write(repo.path().join("tracked.txt"), "dirty\n").unwrap();
        assert!(integrate(repo.path(), &created.branch, "main", false).is_err());
    }

    #[test]
    fn removal_refuses_to_discard_uncommitted_work() {
        let repo = fixture_repo();
        let data = tempdir().unwrap();
        let created = create_for_run(repo.path(), data.path(), "保留").unwrap();
        std::fs::write(created.path.join("wip.txt"), "unfinished").unwrap();
        let record = WorktreeRecord {
            id: "wt-1".into(),
            project_id: None,
            run_id: None,
            repo_root: repo.path().display().to_string(),
            path: created.path.display().to_string(),
            branch: created.branch.clone(),
            base_head: created.base_head.clone(),
            created_at: "now".into(),
            removed_at: None,
        };
        let error = remove(repo.path(), &record, false).unwrap_err();
        assert!(error.contains("未提交改动"), "{error}");
        assert!(created.path.is_dir(), "拒绝删除后目录必须还在");

        // Explicit discard works.
        remove(repo.path(), &record, true).unwrap();
        assert!(!created.path.exists());
    }

    #[test]
    fn index_roundtrip_keeps_the_mapping() {
        let data = tempdir().unwrap();
        let mut index = WorktreeIndex::load(data.path());
        index.insert(WorktreeRecord {
            id: "wt-1".into(),
            project_id: Some("proj-1".into()),
            run_id: Some("run-1".into()),
            repo_root: "/repo".into(),
            path: "/data/worktrees/x".into(),
            branch: "lj-hub/x".into(),
            base_head: "abc".into(),
            created_at: "now".into(),
            removed_at: None,
        });
        index.save(data.path()).unwrap();
        let reloaded = WorktreeIndex::load(data.path());
        assert_eq!(reloaded.for_run("run-1").len(), 1);
        assert!(reloaded.for_run("other").is_empty());
    }
}
