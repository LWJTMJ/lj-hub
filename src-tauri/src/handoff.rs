//! Task handoff: PREPARED → VERIFIED → ACCEPTED.
//!
//! The point of this module is that the *receiving* agent must prove state
//! before it takes over. Reading a summary is not verification: the receiver
//! re-reads the repository, compares Git state against what the sender
//! recorded, checks the registered artifacts, and only then can accept.

use crate::artifacts;
use crate::model::{
    now_iso, Handoff, HandoffStage, HandoffVerificationCheck, RequirementRef, Task,
};
use crate::store::Store;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct HandoffDraft {
    pub to_agent: Option<String>,
    pub objective: String,
    pub requirements: Vec<RequirementRef>,
    pub forbidden: Vec<String>,
    pub completed: Vec<String>,
    pub decisions: Vec<String>,
    pub failed_attempts: Vec<String>,
    pub remaining: Vec<String>,
    pub next_steps: Vec<String>,
}

/// Build a PREPARED handoff from the live state of the task.
pub fn prepare(
    task: &Task,
    project_root: &Path,
    artifacts: Vec<crate::model::ArtifactRef>,
    draft: HandoffDraft,
) -> Handoff {
    Handoff {
        id: crate::model::new_id("handoff"),
        task_id: task.id.clone(),
        from_agent: task.agent_id.clone(),
        to_agent: draft.to_agent,
        stage: HandoffStage::Prepared,
        objective: draft.objective,
        requirements: draft.requirements,
        forbidden: draft.forbidden,
        completed: draft.completed,
        decisions: draft.decisions,
        failed_attempts: draft.failed_attempts,
        git_evidence: crate::git::evidence(project_root),
        artifacts,
        remaining: draft.remaining,
        next_steps: draft.next_steps,
        created_at: now_iso(),
        verified_at: None,
        accepted_at: None,
        checks: Vec::new(),
        receiver_note: None,
        native_session_restored: false,
    }
}

/// Compare the sender's recorded repository state with the state on disk right
/// now. Any mismatch is reported as a failing check, never smoothed over.
pub fn verify(handoff: &Handoff, project_root: &Path) -> Vec<HandoffVerificationCheck> {
    let mut checks = Vec::new();

    // 1. The project is where the sender said it was.
    checks.push(HandoffVerificationCheck {
        name: "工程位置".to_string(),
        passed: project_root.is_dir(),
        detail: format!("{}", project_root.display()),
    });

    let current = crate::git::state(project_root);

    // 2. Git HEAD.
    let recorded_head = handoff.git_evidence.head.clone();
    let head_ok = match (&recorded_head, &current.head) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    };
    checks.push(HandoffVerificationCheck {
        name: "Git HEAD".to_string(),
        passed: head_ok,
        detail: format!(
            "交接记录 {} / 当前 {}",
            recorded_head.as_deref().unwrap_or("(无)"),
            current.head.as_deref().unwrap_or("(无)")
        ),
    });

    // 3. Working tree drift.
    let recorded_files: std::collections::BTreeSet<String> =
        handoff.git_evidence.dirty_files.iter().cloned().collect();
    let current_files: std::collections::BTreeSet<String> = current
        .changes
        .iter()
        .map(|c| format!("{} {}", c.status, c.path))
        .collect();
    let drift: Vec<String> = recorded_files
        .symmetric_difference(&current_files)
        .cloned()
        .collect();
    checks.push(HandoffVerificationCheck {
        name: "工作区状态".to_string(),
        passed: drift.is_empty(),
        detail: if drift.is_empty() {
            format!("未提交改动与交接记录一致（{} 项）", current_files.len())
        } else {
            format!("检测到状态漂移：{}", drift.join("; "))
        },
    });

    // 3b. Content drift: the file *list* can stay identical while a file's
    // bytes move on, so compare a content fingerprint of the changed set.
    let current_fingerprint = crate::git::fingerprint_of(project_root);
    let recorded_fingerprint = handoff.git_evidence.content_fingerprint.clone();
    let fingerprint_check = match (&recorded_fingerprint, &current_fingerprint) {
        (Some(recorded), Some((current, detail))) => HandoffVerificationCheck {
            name: "工作区内容指纹".to_string(),
            passed: recorded == current,
            detail: if recorded == current {
                format!("改动内容与交接记录一致（{detail}）")
            } else {
                format!(
                    "内容已变化：文件列表可能相同但字节不同（{detail}）。交接时 {}，现在 {}",
                    &recorded[..recorded.len().min(12)],
                    &current[..current.len().min(12)]
                )
            },
        },
        (None, Some((_, detail))) => HandoffVerificationCheck {
            name: "工作区内容指纹".to_string(),
            passed: true,
            detail: format!("交接记录未包含指纹（旧记录），本次已补算：{detail}"),
        },
        _ => HandoffVerificationCheck {
            name: "工作区内容指纹".to_string(),
            passed: false,
            detail: "当前目录不是 Git 仓库，无法校验内容指纹".to_string(),
        },
    };
    checks.push(fingerprint_check);

    // 4. Registered artifacts must still be there and unchanged.
    let mut missing = Vec::new();
    let mut changed = Vec::new();
    for artifact in &handoff.artifacts {
        let result = artifacts::verify(artifact, false);
        if !result.accessible {
            missing.push(format!("{}（{}）", artifact.path, result.detail));
        }
        if result.matches_recorded == Some(false) {
            changed.push(artifact.path.clone());
        }
    }
    checks.push(HandoffVerificationCheck {
        name: "必要文件".to_string(),
        passed: missing.is_empty() && changed.is_empty(),
        detail: if missing.is_empty() && changed.is_empty() {
            format!("{} 个登记产物均存在", handoff.artifacts.len())
        } else {
            format!(
                "缺失：{}；内容变化：{}",
                if missing.is_empty() {
                    "无".to_string()
                } else {
                    missing.join("; ")
                },
                if changed.is_empty() {
                    "无".to_string()
                } else {
                    changed.join("; ")
                }
            )
        },
    });

    // 5. Is the material actually enough to continue?
    let material_ok = !handoff.objective.trim().is_empty()
        && (!handoff.next_steps.is_empty() || !handoff.remaining.is_empty());
    checks.push(HandoffVerificationCheck {
        name: "资料完整度".to_string(),
        passed: material_ok,
        detail: if material_ok {
            format!(
                "目标 {} 字；下一步 {} 条；待办 {} 条",
                handoff.objective.chars().count(),
                handoff.next_steps.len(),
                handoff.remaining.len()
            )
        } else {
            "缺少目标描述或后续行动，接收方无法据此继续".to_string()
        },
    });

    checks
}

#[derive(Debug)]
pub struct AcceptOutcome {
    pub accepted: bool,
    pub message: String,
}

/// Record acceptance. A handoff can only be accepted once, so two agents can
/// never both believe they own the task. `project_root` is the directory the
/// receiver re-reads to prove the state.
pub fn accept(
    store: &Store,
    task: &mut Task,
    project_root: &Path,
    receiver_agent: &str,
    note: &str,
    native_session_restored: bool,
) -> Result<AcceptOutcome, String> {
    let mut checks = {
        let handoff = task
            .handoff
            .as_ref()
            .ok_or_else(|| "该任务没有待接手的交接资料".to_string())?;
        if handoff.stage == HandoffStage::Accepted {
            return Ok(AcceptOutcome {
                accepted: false,
                message: format!(
                    "该交接已于 {} 被接受，不能重复验收",
                    handoff.accepted_at.as_deref().unwrap_or("(未知时间)")
                ),
            });
        }
        verify(handoff, project_root)
    };
    let all_passed = checks.iter().all(|c| c.passed);

    let handoff = task.handoff.as_mut().expect("checked above");
    handoff.checks = std::mem::take(&mut checks);
    handoff.verified_at = Some(now_iso());
    handoff.stage = HandoffStage::Verified;

    if !all_passed {
        handoff.stage = HandoffStage::Refused;
        task.status = crate::model::TaskStatus::HandoffRefused;
        task.updated_at = now_iso();
        store.save_task(task).map_err(|e| e.to_string())?;
        return Ok(AcceptOutcome {
            accepted: false,
            message: "验收未通过：交接状态与当前工程不一致，已拒绝接手".to_string(),
        });
    }

    handoff.stage = HandoffStage::Accepted;
    handoff.accepted_at = Some(now_iso());
    handoff.receiver_note = Some(note.to_string());
    handoff.native_session_restored = native_session_restored;
    task.status = crate::model::TaskStatus::HandoffAccepted;
    task.updated_at = now_iso();
    store.save_task(task).map_err(|e| e.to_string())?;

    Ok(AcceptOutcome {
        accepted: true,
        message: format!("{receiver_agent} 已核实状态并接手该任务"),
    })
}

/// Render a handoff as Markdown so it can be pasted into another agent's
/// prompt or saved next to the project.
pub fn render_markdown(handoff: &Handoff) -> String {
    let mut out = String::new();
    out.push_str(&format!("# 任务交接 {}\n\n", handoff.id));
    out.push_str(&format!("- 阶段：{:?}\n", handoff.stage));
    out.push_str(&format!("- 发送方 Agent：{}\n", handoff.from_agent));
    out.push_str(&format!(
        "- 接收方 Agent：{}\n",
        handoff.to_agent.as_deref().unwrap_or("(未指定)")
    ));
    out.push_str(&format!("- 生成时间：{}\n\n", handoff.created_at));
    out.push_str(&format!("## 目标\n{}\n\n", handoff.objective));

    let list = |title: &str, items: &[String], out: &mut String| {
        out.push_str(&format!("## {title}\n"));
        if items.is_empty() {
            out.push_str("- (无)\n");
        } else {
            for item in items {
                out.push_str(&format!("- {item}\n"));
            }
        }
        out.push('\n');
    };

    out.push_str("## 有效用户要求\n");
    if handoff.requirements.is_empty() {
        out.push_str("- (未登记)\n");
    } else {
        for r in &handoff.requirements {
            out.push_str(&format!("- [{}] {} — {}", r.state, r.summary, r.source));
        }
    }
    out.push('\n');

    list("禁止事项", &handoff.forbidden, &mut out);
    list("已完成", &handoff.completed, &mut out);
    list("重要决策", &handoff.decisions, &mut out);
    list("失败尝试", &handoff.failed_attempts, &mut out);
    list("尚未完成", &handoff.remaining, &mut out);
    list("下一步行动", &handoff.next_steps, &mut out);

    out.push_str("## Git 证据\n");
    out.push_str(&format!(
        "- 根目录：{}\n- 分支：{}\n- HEAD：{}\n- 工作区干净：{}\n",
        handoff.git_evidence.root.as_deref().unwrap_or("(未知)"),
        handoff.git_evidence.branch.as_deref().unwrap_or("(未知)"),
        handoff.git_evidence.head.as_deref().unwrap_or("(未知)"),
        if handoff.git_evidence.dirty {
            "否"
        } else {
            "是"
        }
    ));
    if !handoff.git_evidence.dirty_files.is_empty() {
        out.push_str("- 未提交改动：\n");
        for f in &handoff.git_evidence.dirty_files {
            out.push_str(&format!("  - {f}\n"));
        }
    }
    out.push('\n');

    out.push_str("## 重要产物\n");
    if handoff.artifacts.is_empty() {
        out.push_str("- (未登记)\n");
    } else {
        for a in &handoff.artifacts {
            out.push_str(&format!(
                "- {}（{}）{} 字节 sha256={}\n",
                a.path,
                a.kind,
                a.size.map(|s| s.to_string()).unwrap_or_else(|| "?".into()),
                a.sha256.as_deref().unwrap_or("未计算")
            ));
        }
    }
    out.push('\n');

    out.push_str("## 接收方核验结果\n");
    if handoff.checks.is_empty() {
        out.push_str("- (接收方尚未核验)\n");
    } else {
        for c in &handoff.checks {
            out.push_str(&format!(
                "- [{}] {} — {}\n",
                if c.passed { "通过" } else { "未通过" },
                c.name,
                c.detail
            ));
        }
    }
    out.push_str(&format!(
        "\n原生会话恢复：{}\n",
        if handoff.native_session_restored {
            "是"
        } else {
            "否（历史仍可在 LJ Hub 中阅读）"
        }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, MessageBody, TaskStatus};
    use crate::test_support::fixture_repo;

    fn task_for(root: &Path) -> Task {
        Task {
            id: "t1".into(),
            project_id: "p1".into(),
            agent_id: "opencode".into(),
            title: "测试任务".into(),
            status: TaskStatus::Active,
            native_session_id: Some("native".into()),
            native_session_restored: false,
            created_at: now_iso(),
            updated_at: now_iso(),
            messages: vec![Message {
                id: "m1".into(),
                role: "user".into(),
                body: MessageBody::User { text: "hi".into() },
                created_at: now_iso(),
            }],
            handoff: None,
            last_git: Some(crate::git::evidence(root)),
            run_id: None,
            run_step_id: None,
        }
    }

    fn draft() -> HandoffDraft {
        HandoffDraft {
            to_agent: Some("codex".into()),
            objective: "完成 A".into(),
            next_steps: vec!["继续 B".into()],
            ..Default::default()
        }
    }

    #[test]
    fn clean_handoff_verifies_and_accepts_once() {
        let repo = fixture_repo();
        let store = Store::new(repo.path().join(".lj-hub-test"));
        let mut task = task_for(repo.path());
        let handoff = prepare(&task, repo.path(), vec![], draft());
        task.handoff = Some(handoff);

        let first = accept(&store, &mut task, repo.path(), "codex", "已核对", false).unwrap();
        assert!(first.accepted, "{}", first.message);

        let second = accept(&store, &mut task, repo.path(), "qoder", "再次接手", false).unwrap();
        assert!(!second.accepted, "重复验收必须被拒绝");
        assert_eq!(task.status, TaskStatus::HandoffAccepted);
    }

    #[test]
    fn git_drift_is_detected_and_refused() {
        let repo = fixture_repo();
        let store = Store::new(repo.path().join(".lj-hub-test"));
        let mut task = task_for(repo.path());
        let handoff = prepare(&task, repo.path(), vec![], draft());
        task.handoff = Some(handoff);

        // The world moves on: new commit + an untracked file.
        std::fs::write(repo.path().join("drift.txt"), "x").unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["add", "drift.txt"])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["commit", "-q", "-m", "drift"])
            .status()
            .unwrap();

        let outcome = accept(&store, &mut task, repo.path(), "codex", "", false).unwrap();
        assert!(!outcome.accepted, "Git 漂移必须导致拒绝接手");
        let checks = task.handoff.as_ref().unwrap().checks.clone();
        let head_check = checks.iter().find(|c| c.name == "Git HEAD").unwrap();
        assert!(!head_check.passed);
        assert_eq!(task.handoff.as_ref().unwrap().stage, HandoffStage::Refused);
    }

    #[test]
    fn missing_artifact_is_detected() {
        let repo = fixture_repo();
        let store = Store::new(repo.path().join(".lj-hub-test"));
        let artifact_path = repo.path().join("out.bin");
        std::fs::write(&artifact_path, b"data").unwrap();
        let artifact =
            artifacts::register(repo.path(), "a1", "out.bin", "binary", "", false).unwrap();
        std::fs::remove_file(&artifact_path).unwrap();

        let mut task = task_for(repo.path());
        let handoff = prepare(&task, repo.path(), vec![artifact], draft());
        task.handoff = Some(handoff);

        let outcome = accept(&store, &mut task, repo.path(), "codex", "", false).unwrap();
        assert!(!outcome.accepted);
        let checks = task.handoff.as_ref().unwrap().checks.clone();
        let artifact_check = checks.iter().find(|c| c.name == "必要文件").unwrap();
        assert!(!artifact_check.passed);
    }

    #[test]
    fn content_drift_with_identical_file_names_is_refused() {
        let repo = fixture_repo();
        let store = Store::new(repo.path().join(".lj-hub-test"));
        let mut task = task_for(repo.path());
        std::fs::write(repo.path().join("work.txt"), "v1").unwrap();
        let handoff = prepare(&task, repo.path(), vec![], draft());
        task.handoff = Some(handoff);

        // Same file name, same dirty-file list, different bytes.
        std::fs::write(repo.path().join("work.txt"), "v2 changed").unwrap();

        let outcome = accept(&store, &mut task, repo.path(), "codex", "", false).unwrap();
        assert!(!outcome.accepted, "文件名不变但内容变化必须导致拒绝接手");
        let checks = task.handoff.as_ref().unwrap().checks.clone();
        let fingerprint = checks
            .iter()
            .find(|c| c.name == "工作区内容指纹")
            .expect("必须包含内容指纹检查");
        assert!(!fingerprint.passed, "{}", fingerprint.detail);
    }

    #[test]
    fn incomplete_material_is_refused() {
        let repo = fixture_repo();
        let store = Store::new(repo.path().join(".lj-hub-test"));
        let mut task = task_for(repo.path());
        let handoff = prepare(
            &task,
            repo.path(),
            vec![],
            HandoffDraft {
                objective: String::new(),
                ..Default::default()
            },
        );
        task.handoff = Some(handoff);
        let outcome = accept(&store, &mut task, repo.path(), "codex", "", false).unwrap();
        assert!(!outcome.accepted, "资料不足时必须拒绝");
    }

    #[test]
    fn markdown_contains_required_sections() {
        let repo = fixture_repo();
        let task = task_for(repo.path());
        let handoff = prepare(&task, repo.path(), vec![], draft());
        let md = render_markdown(&handoff);
        for section in [
            "目标",
            "有效用户要求",
            "禁止事项",
            "Git 证据",
            "重要产物",
            "接收方核验结果",
        ] {
            assert!(md.contains(section), "缺少章节 {section}");
        }
    }
}
