//! Persistence for collaborative runs.
//!
//! One JSON document per run under `<data>/runs/<run_id>.json`, written
//! atomically. Every state transition goes through the store, so a crash can
//! never leave the in-memory state ahead of what is on disk.
//!
//! Format compatibility: unknown fields are ignored and missing optional fields
//! fall back to defaults, so a run written by an older build still loads. A run
//! file that cannot be parsed is *reported*, never silently discarded.

use crate::model::now_iso;
use crate::scheduler::{Run, RunStatus, StepStatus};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RunStore {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LoadProblem {
    pub run_id: String,
    pub detail: String,
}

impl RunStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        let root = data_dir.as_ref().join("runs");
        let _ = fs::create_dir_all(&root);
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_of(&self, run_id: &str) -> PathBuf {
        self.root.join(format!("{run_id}.json"))
    }

    pub fn save(&self, run: &Run) -> Result<(), String> {
        let path = self.path_of(&run.id);
        let bytes = serde_json::to_vec_pretty(run).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn load(&self, run_id: &str) -> Result<Run, String> {
        let bytes =
            fs::read(self.path_of(run_id)).map_err(|e| format!("运行记录不存在或不可读：{e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("运行记录解析失败：{e}"))
    }

    /// All runs, newest first, plus any files that failed to parse.
    pub fn list(&self) -> (Vec<Run>, Vec<LoadProblem>) {
        let mut runs = Vec::new();
        let mut problems = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return (runs, problems);
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(run_id) = name.strip_suffix(".json") else {
                continue;
            };
            match self.load(run_id) {
                Ok(run) => runs.push(run),
                Err(detail) => problems.push(LoadProblem {
                    run_id: run_id.to_string(),
                    detail,
                }),
            }
        }
        runs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        (runs, problems)
    }

    pub fn delete(&self, run_id: &str) -> Result<(), String> {
        let path = self.path_of(run_id);
        if path.exists() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// After a restart, a step that was mid-flight has no live session. It must
    /// not be assumed to have succeeded, and must not be blindly re-run: both
    /// are recorded as a human decision with the evidence we do have.
    ///
    /// Steps that never started (Pending) stay Pending and resume normally.
    pub fn reconcile_after_restart(run: &mut Run) -> Vec<String> {
        let mut notes = Vec::new();
        let live_ids: Vec<String> = Vec::new();
        let _ = &live_ids;
        let mut touched = false;
        for state in run.steps.iter_mut() {
            if matches!(
                state.status,
                StepStatus::Running | StepStatus::Verifying | StepStatus::AwaitingHandoff
            ) {
                let detail = match (&state.git, state.status) {
                    (Some(git), _) => format!(
                        "进程在步骤执行期间结束。开始时记录的 Git 事实：HEAD {}，改动 {} 项（指纹 {}）。\
                         无法确认这一步是否真正完成，已停止自动推进，请人工确认后选择重试或标记完成。",
                        git.head.as_deref().unwrap_or("(无)"),
                        git.dirty_files.len(),
                        git.content_fingerprint
                            .as_deref()
                            .map(|f| &f[..f.len().min(12)])
                            .unwrap_or("(未记录)")
                    ),
                    (None, _) => "进程在步骤执行期间结束，且没有留下可核对的 Git 事实；\
                                  无法确认是否完成，已停止自动推进。"
                        .to_string(),
                };
                state.needs_human_reason = Some(detail.clone());
                notes.push(format!("步骤 {}：{}", state.id, detail));
                state.status = StepStatus::NeedsHuman;
                touched = true;
            }
        }
        if touched {
            run.stop_reason = Some("上次运行被中断，存在需要人工确认的步骤".to_string());
            run.refresh_status();
            run.updated_at = now_iso();
        }
        notes
    }

    /// Runs that need a human decision, so the UI can surface them on start-up.
    pub fn needs_human(&self) -> Vec<(String, String, String)> {
        let (runs, _) = self.list();
        runs.into_iter()
            .filter_map(|run| {
                let blocked: Vec<&str> = run
                    .steps
                    .iter()
                    .filter(|s| s.status == StepStatus::NeedsHuman)
                    .map(|s| s.id.as_str())
                    .collect();
                if blocked.is_empty() {
                    return None;
                }
                Some((
                    run.id.clone(),
                    run.title.clone(),
                    format!("需要人工处理：{}", blocked.join(", ")),
                ))
            })
            .collect()
    }
}

/// Runs that are still worth driving (pending/running work and not blocked).
pub fn is_driveable(run: &Run) -> bool {
    matches!(run.status, RunStatus::Draft | RunStatus::Running)
        && run
            .steps
            .iter()
            .any(|s| matches!(s.status, StepStatus::Pending | StepStatus::Running))
}

impl Run {
    /// Helper used by tests and the UI projection layer.
    pub fn step_state_status(&self, step_id: &str) -> Option<StepStatus> {
        self.state(step_id).map(|s| s.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{
        GateKind, Phase, RunPlan, StepMode, StepSpec, WorkspaceRef, DEFAULT_MAX_LOOPS,
    };
    use tempfile::tempdir;

    fn simple_run() -> Run {
        let plan = RunPlan {
            phases: vec![Phase {
                id: "p1".into(),
                title: "阶段一".into(),
                parallelism: 1,
                gate: GateKind::AllDone,
            }],
            steps: vec![StepSpec {
                id: "s1".into(),
                phase: "p1".into(),
                title: "写代码".into(),
                prompt: "实现".into(),
                agent: "opencode".into(),
                mode: StepMode::Write,
                depends_on: vec![],
                verify_by: None,
                max_loops: DEFAULT_MAX_LOOPS,
                max_attempts: 2,
                timeout_secs: 60,
                consumes: vec![],
            }],
        };
        Run::new(
            "run-1".into(),
            "proj-1".into(),
            "接力".into(),
            "目标".into(),
            "planner".into(),
            plan,
            WorkspaceRef::main("/tmp/ws"),
        )
    }

    #[test]
    fn run_roundtrip_survives_restart() {
        let dir = tempdir().unwrap();
        let store = RunStore::new(dir.path());
        let mut run = simple_run();
        run.status = RunStatus::Running;
        run.transition("s1", StepStatus::Running, "started");
        store.save(&run).unwrap();

        let reopened = RunStore::new(dir.path());
        let loaded = reopened.load("run-1").unwrap();
        assert_eq!(loaded.status, RunStatus::Running);
        assert_eq!(loaded.history.len(), 1);
        assert_eq!(loaded.step_state_status("s1"), Some(StepStatus::Running));
    }

    #[test]
    fn interrupted_steps_are_not_assumed_done_and_not_blindly_rerun() {
        let dir = tempdir().unwrap();
        let store = RunStore::new(dir.path());
        let mut run = simple_run();
        run.status = RunStatus::Running;
        run.transition("s1", StepStatus::Running, "started");
        run.state_mut("s1").unwrap().git = Some(crate::model::GitEvidence {
            root: Some("/tmp/ws".into()),
            branch: Some("main".into()),
            head: Some("abc123".into()),
            dirty: true,
            dirty_files: vec![" M a.txt".into()],
            captured_at: "2026-01-01T00:00:00Z".into(),
            content_fingerprint: Some("deadbeefdeadbeef".into()),
            fingerprint_detail: Some("按内容哈希 1 个文件".into()),
        });
        store.save(&run).unwrap();

        let mut loaded = store.load("run-1").unwrap();
        let notes = RunStore::reconcile_after_restart(&mut loaded);
        assert_eq!(notes.len(), 1);
        assert_eq!(
            loaded.step_state_status("s1"),
            Some(StepStatus::NeedsHuman),
            "中断的步骤必须交给人工确认"
        );
        assert_eq!(loaded.status, RunStatus::NeedsHuman);
        assert!(loaded
            .state("s1")
            .unwrap()
            .needs_human_reason
            .as_deref()
            .unwrap_or_default()
            .contains("abc123"));

        // A pending step (never started) is still pending after reconciliation.
        let mut fresh = simple_run();
        RunStore::reconcile_after_restart(&mut fresh);
        assert_eq!(fresh.step_state_status("s1"), Some(StepStatus::Pending));
    }

    #[test]
    fn corrupt_run_file_is_reported_not_ignored() {
        let dir = tempdir().unwrap();
        let store = RunStore::new(dir.path());
        store.save(&simple_run()).unwrap();
        std::fs::write(dir.path().join("runs").join("broken.json"), b"{ not json").unwrap();
        let (runs, problems) = store.list();
        assert_eq!(runs.len(), 1);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].detail.contains("解析失败"));
    }

    #[test]
    fn needs_human_listing_surfaces_blocked_runs() {
        let dir = tempdir().unwrap();
        let store = RunStore::new(dir.path());
        let mut run = simple_run();
        run.status = RunStatus::Running;
        run.transition("s1", StepStatus::NeedsHuman, "agent 不可用");
        store.save(&run).unwrap();
        let blocked = store.needs_human();
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].2.contains("s1"));
    }
}
