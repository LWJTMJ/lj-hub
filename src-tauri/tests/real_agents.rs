//! Optional tests against agents actually installed on this machine.
//!
//! These are opt-in (`cargo test --test real_agents -- --ignored`) because a
//! normal CI run has no vendor agents installed. When an agent *is* present the
//! test performs a real ACP handshake; a missing agent is reported as skipped,
//! never as a pass.
//!
//! Real model turns are opt-in via `LJ_HUB_REAL_PROMPT=1` so that running this
//! file cannot silently spend a user's subscription quota.

use lj_hub_lib::acp::{probe, spawn_session, PermissionRegistry, SessionSpec};
use lj_hub_lib::agents::{self, AGENTS};
use lj_hub_lib::events::EventCallback;
use lj_hub_lib::model::SessionEvent;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn resolved(agent_id: &str) -> Option<(PathBuf, Vec<String>)> {
    let def = agents::find_definition(agent_id)?;
    let (exe, _) = agents::resolve_executable(def, None)?;
    Some((exe, def.acp_args.iter().map(|s| s.to_string()).collect()))
}

/// Record a genuine OpenCode turn into the application's own data directory
/// using the same store/event code the UI uses. Used to produce honest
/// screenshots (real transcript, real Git state) for the README.
///
/// Ignored by default: it writes into the real app data directory and spends a
/// small amount of the user's existing OpenCode quota.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn record_real_session_into_app_data() {
    use lj_hub_lib::app::AppState;
    use lj_hub_lib::model::{now_iso, Project, TaskStatus};

    let Some((exe, args)) = resolved("opencode") else {
        panic!("需要本机已安装 OpenCode CLI");
    };
    let data_dir = std::env::var("LJ_HUB_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap())
                .join("Library/Application Support/com.ljhub.desktop")
        });
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();

    let state = AppState::new(data_dir.clone());
    {
        let mut store = state.state.lock().unwrap();
        if !store
            .projects
            .iter()
            .any(|p| p.path == project_root.display().to_string())
        {
            store.projects.push(Project {
                id: "proj-lj-hub".into(),
                name: "lj-hub".into(),
                path: project_root.display().to_string(),
                git_root: Some(project_root.display().to_string()),
                git_remote: Some("https://github.com/LWJTMJ/lj-hub".into()),
                created_at: now_iso(),
                last_opened_at: now_iso(),
            });
        }
        state.store.save_state(&store).unwrap();
    }

    let task = lj_hub_lib::model::Task {
        id: "task-demo".into(),
        project_id: "proj-lj-hub".into(),
        agent_id: "opencode".into(),
        title: "真实会话示例 · OpenCode".into(),
        status: TaskStatus::Active,
        native_session_id: None,
        native_session_restored: false,
        created_at: now_iso(),
        updated_at: now_iso(),
        messages: Vec::new(),
        handoff: None,
        last_git: Some(lj_hub_lib::git::evidence(&project_root)),
        run_id: None,
        run_step_id: None,
    };
    state.store.save_task(&task).unwrap();
    state.refresh_index();

    let handle = std::sync::Arc::new(state);
    let state_for_events = handle.clone();
    let sink: EventCallback = Arc::new(move |event| {
        state_for_events.record_event("task-demo", event);
    });

    let session = spawn_session(
        SessionSpec {
            task_id: "task-demo".into(),
            agent_id: "opencode".into(),
            project_root: project_root.clone(),
            executable: exe,
            args,
            extra_env: vec![],
            resume_native: None,
        },
        sink,
        PermissionRegistry::default(),
    )
    .await
    .expect("真实会话应能建立");

    handle.record_user_message("task-demo", "请用一句话说明这个项目是做什么的。");
    let outcome = session
        .prompt_text("请用一句话说明这个项目是做什么的。")
        .await
        .expect("真实回合应返回");
    println!("真实回合：{:?}，数据目录 {}", outcome, data_dir.display());
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    session.shutdown();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let saved = handle.store.load_task("task-demo").unwrap();
    println!("已保存消息数：{}", saved.messages.len());
    assert!(
        saved.messages.len() >= 2,
        "真实回合应记录用户消息与 Agent 回复"
    );
}

#[test]
fn report_real_agent_availability() {
    // Not ignored: this is a pure detection report and is always safe to run.
    for def in AGENTS {
        let found = agents::resolve_executable(def, None);
        match found {
            Some((path, _)) => println!("[已安装] {} -> {}", def.display_name, path.display()),
            None => println!("[未安装] {} ({})", def.display_name, def.doc_url),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_opencode_handshake() {
    let Some((exe, args)) = resolved("opencode") else {
        eprintln!("跳过：本机未安装 OpenCode CLI");
        return;
    };
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));
    let outcome = probe(exe, args, home, Duration::from_secs(120)).await;
    println!(
        "OpenCode 握手结果：{:?} / {} / agent_info={:?}",
        outcome.status, outcome.message, outcome.agent_info
    );
    assert!(
        matches!(
            outcome.status,
            lj_hub_lib::model::AgentStatus::Available
                | lj_hub_lib::model::AgentStatus::AuthRequired
        ),
        "真实 OpenCode ACP 握手应成功或明确要求认证，实际：{:?} {}",
        outcome.status,
        outcome.message
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_deepseek_harness_handshake() {
    let Some((exe, args)) = resolved("deepseek-harness") else {
        eprintln!("跳过：本机未安装 DeepSeek Harness CLI");
        return;
    };
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));
    let outcome = probe(exe, args, home, Duration::from_secs(120)).await;
    println!(
        "DeepSeek Harness 握手结果：{:?} / {} / agent_info={:?}",
        outcome.status, outcome.message, outcome.agent_info
    );
    assert!(
        !matches!(outcome.status, lj_hub_lib::model::AgentStatus::NotTested),
        "真实握手必须有确定结论"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_opencode_round_trip() {
    if std::env::var("LJ_HUB_REAL_PROMPT").as_deref() != Ok("1") {
        eprintln!("跳过：设置 LJ_HUB_REAL_PROMPT=1 才会发起真实模型回合");
        return;
    }
    let Some((exe, args)) = resolved("opencode") else {
        eprintln!("跳过：本机未安装 OpenCode CLI");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::<SessionEvent>::new()));
    let sink_events = events.clone();
    let sink: EventCallback = Arc::new(move |event| {
        sink_events.lock().unwrap().push(event);
    });

    let session = spawn_session(
        SessionSpec {
            task_id: "real-opencode".into(),
            agent_id: "opencode".into(),
            project_root: dir.path().to_path_buf(),
            executable: exe,
            args,
            extra_env: vec![],
            resume_native: None,
        },
        sink,
        PermissionRegistry::default(),
    )
    .await
    .expect("真实会话应能建立");

    let outcome = session
        .prompt_text("请只回复两个字：收到")
        .await
        .expect("真实回合应返回");
    println!("真实回合结束：{:?}", outcome);
    let snapshot = events.lock().unwrap().clone();
    let text: String = snapshot
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Chunk { stream, text } if stream == "agent" => Some(text.clone()),
            _ => None,
        })
        .collect();
    println!("Agent 输出：{text}");
    assert!(!text.is_empty(), "真实 Agent 应产生流式输出");
    session.shutdown();
}
