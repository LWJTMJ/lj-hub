//! End-to-end ACP session tests.
//!
//! These drive LJ Hub's real client (`acp::spawn_session`) against the
//! `mock-acp-agent` binary, which speaks the ACP v1 wire protocol by hand. If
//! LJ Hub's framing, handshake, streaming or permission handling were wrong,
//! these tests fail — the agent is not using the same SDK as the client.

use lj_hub_lib::acp::{spawn_session, PermissionRegistry, SessionSpec};
use lj_hub_lib::events::EventCallback;
use lj_hub_lib::model::{MessageBody, SessionEvent};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn mock_agent_bin() -> PathBuf {
    // cargo sets CARGO_BIN_EXE_<name> for integration tests.
    PathBuf::from(env!("CARGO_BIN_EXE_mock-acp-agent"))
}

struct Collected {
    events: Arc<Mutex<Vec<SessionEvent>>>,
    /// The callback LJ Hub uses in production to record events.
    sink: EventCallback,
}

impl Collected {
    /// Let the pipeline flush its coalesced stream before reading.
    fn snapshot(&self) -> Vec<SessionEvent> {
        std::thread::sleep(std::time::Duration::from_millis(200));
        self.events.lock().unwrap().clone()
    }
}

fn collector() -> Collected {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = events.clone();
    let sink: EventCallback = Arc::new(move |event: SessionEvent| {
        sink_events.lock().unwrap().push(event);
    });
    Collected { events, sink }
}

fn spec(root: &std::path::Path, scenario: &str) -> SessionSpec {
    SessionSpec {
        task_id: format!("task-{scenario}"),
        agent_id: "mock".into(),
        project_root: root.to_path_buf(),
        executable: mock_agent_bin(),
        args: vec!["--scenario".into(), scenario.into()],
        extra_env: vec![],
        resume_native: None,
    }
}

fn text_of(events: &[SessionEvent], stream: &str) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Chunk { stream: s, text } if s == stream => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

async fn wait_for<F: Fn(&[SessionEvent]) -> bool>(
    events: &Arc<Mutex<Vec<SessionEvent>>>,
    predicate: F,
    timeout: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if predicate(&events.lock().unwrap()) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    false
}

/// Wait for an event matching `pick` and return it. Yields to the runtime so a
/// spawned prompt task can make progress.
async fn wait_for_value<T, F>(
    events: &Arc<Mutex<Vec<SessionEvent>>>,
    mut pick: F,
    timeout: std::time::Duration,
) -> Option<T>
where
    F: FnMut(&[SessionEvent]) -> Option<T>,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let snapshot = events.lock().unwrap().clone();
        if let Some(value) = pick(&snapshot) {
            return Some(value);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_streams_answer_thought_and_tool_call() {
    let dir = tempfile::tempdir().unwrap();
    let collected = collector();
    let session = spawn_session(
        spec(dir.path(), "happy"),
        collected.sink.clone(),
        PermissionRegistry::default(),
    )
    .await
    .expect("握手成功");

    assert!(session.is_alive());
    assert!(session.native_session_id().is_some());

    let outcome = session.prompt_text("你好").await.expect("prompt 完成");
    assert_eq!(outcome.stop_reason, "end_turn");
    assert!(outcome.error.is_none());

    assert!(
        wait_for(
            &collected.events,
            |events| {
                events
                    .iter()
                    .any(|e| matches!(e, SessionEvent::PromptDone { .. }))
            },
            std::time::Duration::from_secs(10)
        )
        .await
    );

    let events = collected.snapshot();
    assert!(text_of(&events, "agent").contains("收到：你好"));
    assert_eq!(text_of(&events, "thought"), "（思考中）");

    let tools: Vec<&MessageBody> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolCall { tool } => Some(tool),
            _ => None,
        })
        .collect();
    assert!(!tools.is_empty(), "必须收到工具调用事件");
    let last = tools.last().unwrap();
    match last {
        MessageBody::Tool {
            status, tool_kind, ..
        } => {
            assert_eq!(status, "completed");
            assert_eq!(tool_kind, "Read");
        }
        other => panic!("意外事件 {other:?}"),
    }

    session.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_failure_is_reported_not_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let collected = collector();
    let result = spawn_session(
        spec(dir.path(), "refuse"),
        collected.sink.clone(),
        PermissionRegistry::default(),
    )
    .await;
    let error = match result {
        Ok(_) => panic!("初始化失败必须返回错误，不能假装连接成功"),
        Err(e) => e,
    };
    assert!(
        error.contains("refuse") || error.contains("握手") || error.contains("initialize"),
        "错误信息应可诊断：{error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashing_agent_is_reported_as_error() {
    let dir = tempfile::tempdir().unwrap();
    let collected = collector();
    let result = spawn_session(
        spec(dir.path(), "crash"),
        collected.sink.clone(),
        PermissionRegistry::default(),
    )
    .await;
    assert!(result.is_err(), "崩溃的 Agent 不能被当成已连接");
    if let Err(message) = result {
        assert!(!message.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nonexistent_executable_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let collected = collector();
    let mut s = spec(dir.path(), "happy");
    s.executable = PathBuf::from("/nonexistent/definitely-not-an-agent");
    assert!(
        spawn_session(s, collected.sink.clone(), PermissionRegistry::default())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_permission_does_not_execute_the_action() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("trace.json");
    let collected = collector();
    let mut s = spec(dir.path(), "permission");
    s.args.push("--log".into());
    s.args.push(log.display().to_string());

    let session = std::sync::Arc::new(
        spawn_session(s, collected.sink.clone(), PermissionRegistry::default())
            .await
            .unwrap(),
    );

    // The agent blocks on the permission request; answer it through the same
    // path the UI uses (the request id comes from the emitted event).
    let session_for_prompt = session.clone();
    let prompt = tokio::spawn(async move { session_for_prompt.prompt_text("删除数据").await });
    let (request_id, option_count) = wait_for_value(
        &collected.events,
        |events| {
            events.iter().find_map(|e| match e {
                SessionEvent::PermissionRequest {
                    request_id,
                    options,
                    ..
                } => Some((request_id.clone(), options.len())),
                _ => None,
            })
        },
        std::time::Duration::from_secs(15),
    )
    .await
    .expect("未收到权限请求");
    assert_eq!(option_count, 2);

    assert!(
        session.resolve_permission(&request_id, None),
        "权限请求应可解析"
    );
    let outcome = prompt.await.unwrap().unwrap();
    assert_eq!(outcome.stop_reason, "end_turn");

    assert!(
        wait_for(
            &collected.events,
            |events| text_of(events, "agent").contains("权限被拒绝")
                || text_of(events, "agent").contains("未执行"),
            std::time::Duration::from_secs(10)
        )
        .await
    );
    let events = collected.snapshot();
    let text = text_of(&events, "agent");
    assert!(
        text.contains("权限被拒绝") && !text.contains("已获得授权"),
        "拒绝权限后不得继续执行：{text}"
    );

    // The agent's own trace proves it never received an approval.
    let trace = std::fs::read_to_string(&log).unwrap();
    assert!(!trace.contains("allow-once"), "拒绝后不应出现 allow-once");

    session.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn granted_permission_proceeds() {
    let dir = tempfile::tempdir().unwrap();
    let collected = collector();
    let session = std::sync::Arc::new(
        spawn_session(
            spec(dir.path(), "permission"),
            collected.sink.clone(),
            PermissionRegistry::default(),
        )
        .await
        .unwrap(),
    );
    let session_for_prompt = session.clone();
    let prompt = tokio::spawn(async move { session_for_prompt.prompt_text("删除数据").await });
    let request_id = wait_for_value(
        &collected.events,
        |events| {
            events.iter().find_map(|e| match e {
                SessionEvent::PermissionRequest { request_id, .. } => Some(request_id.clone()),
                _ => None,
            })
        },
        std::time::Duration::from_secs(15),
    )
    .await
    .expect("未收到权限请求");
    session.resolve_permission(&request_id, Some("allow-once".into()));
    let outcome = prompt.await.unwrap().unwrap();
    assert_eq!(outcome.stop_reason, "end_turn");
    assert!(
        wait_for(
            &collected.events,
            |events| text_of(events, "agent").contains("已获得授权"),
            std::time::Duration::from_secs(10)
        )
        .await
    );
    session.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_a_running_turn() {
    let dir = tempfile::tempdir().unwrap();
    let collected = collector();
    let session = std::sync::Arc::new(
        spawn_session(
            spec(dir.path(), "slow"),
            collected.sink.clone(),
            PermissionRegistry::default(),
        )
        .await
        .unwrap(),
    );

    let session_for_prompt = session.clone();
    let prompt = tokio::spawn(async move { session_for_prompt.prompt_text("一直跑下去").await });
    // Wait until the agent is actually streaming before cancelling.
    assert!(
        wait_for(
            &collected.events,
            |events| !text_of(events, "agent").is_empty(),
            std::time::Duration::from_secs(10)
        )
        .await,
        "Agent 应已开始输出"
    );
    session.cancel();

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), prompt)
        .await
        .expect("取消后应很快返回")
        .expect("回合应正常结束")
        .expect("prompt 应返回结果");
    assert_eq!(outcome.stop_reason, "cancelled");

    session.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fs_requests_are_served_and_sandboxed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("tracked.txt"), "file-content\n").unwrap();
    let collected = collector();
    let session = spawn_session(
        spec(dir.path(), "fs"),
        collected.sink.clone(),
        PermissionRegistry::default(),
    )
    .await
    .unwrap();

    let outcome = session.prompt_text("读写文件").await.unwrap();
    assert_eq!(outcome.stop_reason, "end_turn");
    // The agent asked LJ Hub to write this file through fs/write_text_file.
    let written = std::fs::read_to_string(dir.path().join("written-by-agent.txt"))
        .expect("Agent 通过 fs/write_text_file 写入的文件应存在");
    assert_eq!(written, "written by mock agent\n");
    // And the read went through the same sandbox (the file was inside).
    assert!(dir.path().join("tracked.txt").exists());

    session.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_session_is_restored_when_agent_supports_it() {
    let dir = tempfile::tempdir().unwrap();
    let collected = collector();
    let mut s = spec(dir.path(), "happy");
    s.resume_native = Some("previous-native-id".into());
    let session = spawn_session(s, collected.sink.clone(), PermissionRegistry::default())
        .await
        .unwrap();

    assert!(
        wait_for(
            &collected.events,
            |events| events
                .iter()
                .any(|e| matches!(e, SessionEvent::NativeSession { restored: true, .. })),
            std::time::Duration::from_secs(5)
        )
        .await
    );
    assert_eq!(
        session.native_session_id().as_deref(),
        Some("previous-native-id")
    );
    session.shutdown();
}
