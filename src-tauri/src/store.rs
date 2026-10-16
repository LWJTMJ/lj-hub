//! Local, file-based persistence.
//!
//! LJ Hub deliberately avoids a database service and a heavy embedded engine:
//! state lives in JSON documents under the app data directory, written
//! atomically (temp file + rename). Each task owns one file so a large
//! conversation does not rewrite the whole index.
//!
//! Layout:
//! ```text
//! <data_dir>/state.json          projects, task index, settings
//! <data_dir>/tasks/<task>.json   one task incl. its messages and handoff
//! ```

use crate::model::{Project, StoreState, Task, TaskSummary};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "state".into())
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let _ = fs::create_dir_all(root.join("tasks"));
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn task_path(&self, task_id: &str) -> PathBuf {
        self.root.join("tasks").join(format!("{task_id}.json"))
    }

    /// Read the index. A missing or corrupt file yields an empty state rather
    /// than an error: LJ Hub must still start and let the user recover.
    pub fn load_state(&self) -> StoreState {
        match fs::read(self.state_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => StoreState::default(),
        }
    }

    pub fn save_state(&self, state: &StoreState) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(state)?;
        atomic_write(&self.state_path(), &bytes)
    }

    pub fn save_task(&self, task: &Task) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(task)?;
        atomic_write(&self.task_path(&task.id), &bytes)
    }

    pub fn load_task(&self, task_id: &str) -> Option<Task> {
        fs::read(self.task_path(task_id))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    pub fn delete_task(&self, task_id: &str) -> std::io::Result<()> {
        let p = self.task_path(task_id);
        if p.exists() {
            fs::remove_file(p)?;
        }
        Ok(())
    }

    pub fn list_task_files(&self) -> Vec<String> {
        let dir = self.root.join("tasks");
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(stem) = name.strip_suffix(".json") {
                    out.push(stem.to_string());
                }
            }
        }
        out.sort();
        out
    }

    pub fn summary_of(task: &Task, live: bool) -> TaskSummary {
        TaskSummary {
            id: task.id.clone(),
            project_id: task.project_id.clone(),
            agent_id: task.agent_id.clone(),
            title: task.title.clone(),
            status: task.status,
            updated_at: task.updated_at.clone(),
            message_count: task.messages.len(),
            has_handoff: task.handoff.is_some(),
            live,
        }
    }

    /// Rebuild the task index from the task files on disk. Used on startup so
    /// that the index can never disagree with the durable documents.
    pub fn rebuild_index(&self, state: &mut StoreState, live_ids: &[String]) {
        let mut summaries = Vec::new();
        for id in self.list_task_files() {
            if let Some(task) = self.load_task(&id) {
                summaries.push(Self::summary_of(&task, live_ids.contains(&task.id)));
            }
        }
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        state.tasks = summaries;
    }

    pub fn upsert_project(state: &mut StoreState, project: Project) {
        if let Some(existing) = state.projects.iter_mut().find(|p| p.id == project.id) {
            *existing = project;
        } else {
            state.projects.push(project);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, MessageBody, TaskStatus};
    use tempfile::tempdir;

    fn sample_task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            project_id: "p1".into(),
            agent_id: "opencode".into(),
            title: "t".into(),
            status: TaskStatus::Active,
            native_session_id: Some("native-1".into()),
            native_session_restored: false,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            messages: vec![Message {
                id: "m1".into(),
                role: "user".into(),
                body: MessageBody::User { text: "hi".into() },
                created_at: "2026-01-01T00:00:00Z".into(),
            }],
            handoff: None,
            last_git: None,
        }
    }

    #[test]
    fn task_roundtrip_survives_restart() {
        let dir = tempdir().unwrap();
        let store = Store::new(dir.path());
        let task = sample_task("t1");
        store.save_task(&task).unwrap();

        // A brand new Store instance simulates an application restart.
        let reopened = Store::new(dir.path());
        let loaded = reopened.load_task("t1").expect("task persisted");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.native_session_id.as_deref(), Some("native-1"));

        let mut state = reopened.load_state();
        reopened.rebuild_index(&mut state, &[]);
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.tasks[0].id, "t1");
        assert!(!state.tasks[0].live);
    }

    #[test]
    fn missing_state_yields_default() {
        let dir = tempdir().unwrap();
        let store = Store::new(dir.path());
        let state = store.load_state();
        assert!(state.projects.is_empty());
        assert!(state.tasks.is_empty());
    }
}
