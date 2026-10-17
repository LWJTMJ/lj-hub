//! A protocol-level mock ACP agent used by the automated tests.
//!
//! It deliberately implements the ACP v1 wire format by hand (newline
//! delimited JSON-RPC 2.0 over stdio) instead of using the SDK's agent role.
//! That way the tests exercise LJ Hub's client against an independent
//! implementation of the protocol rather than against the same library.
//!
//! Scenarios (selected with `--scenario`):
//!   happy        stream text + a tool call, then finish
//!   needs-auth   advertise an auth method so the client must report AUTH_REQUIRED
//!   refuse       fail the initialize request
//!   permission   ask for permission before touching a file
//!   slow         stay busy until cancelled
//!   crash        exit immediately (spawn/handshake failure)
//!   fs           exercise fs/read_text_file and fs/write_text_file
//!   relay        act as planner / worker / reviewer for the relay tests, based
//!                on the ROLE marker in the prompt, and work in the session cwd
//!
//! Every inbound request/notification is appended to the file given by
//! `--log <path>`, which lets tests assert exactly what the agent saw.

use serde_json::{json, Value};
use std::io::Write;

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    Happy,
    NeedsAuth,
    Refuse,
    Permission,
    Slow,
    Crash,
    Fs,
    Relay,
}

fn parse_scenario() -> (Scenario, Option<PathBuf>, Option<String>) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut scenario = Scenario::Happy;
    let mut log = None;
    let mut resume_id = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--scenario" => {
                if let Some(v) = args.get(i + 1) {
                    scenario = match v.as_str() {
                        "happy" => Scenario::Happy,
                        "needs-auth" => Scenario::NeedsAuth,
                        "refuse" => Scenario::Refuse,
                        "permission" => Scenario::Permission,
                        "slow" => Scenario::Slow,
                        "crash" => Scenario::Crash,
                        "fs" => Scenario::Fs,
                        "relay" => Scenario::Relay,
                        other => {
                            eprintln!("unknown scenario {other}");
                            Scenario::Happy
                        }
                    };
                }
                i += 2;
            }
            "--log" => {
                log = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--resume-id" => {
                resume_id = args.get(i + 1).cloned();
                i += 2;
            }
            _ => i += 1,
        }
    }
    (scenario, log, resume_id)
}

fn main() {
    let (scenario, log_path, resume_id) = parse_scenario();
    if scenario == Scenario::Crash {
        eprintln!("mock-agent: intentional failure");
        std::process::exit(3);
    }

    let mut out = std::io::stdout();
    let mut session_id = resume_id.unwrap_or_else(|| "mock-session-1".to_string());
    let mut log_lines: Vec<Value> = Vec::new();
    // Working directory announced by session/new; the relay scenarios write
    // their evidence here, exactly like a real agent would.
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

    // Read one message per call. A held `StdinLock` would deadlock the nested
    // reads used by the permission/fs scenarios.
    while let Some(line) = line_or_eof() {
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("mock-agent: bad json: {e}");
                continue;
            }
        };
        log_lines.push(msg.clone());

        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();

        match method {
            "initialize" => {
                if scenario == Scenario::Refuse {
                    reply_err(&mut out, id, -32603, "mock agent refuses to initialize");
                    continue;
                }
                let auth = if scenario == Scenario::NeedsAuth {
                    json!([{ "id": "mock-login", "name": "Mock 登录",
                             "description": "测试用认证方式" }])
                } else {
                    json!([])
                };
                reply(
                    &mut out,
                    id,
                    json!({
                        "protocolVersion": 1,
                        "agentCapabilities": {
                            "loadSession": true,
                            "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
                            "sessionCapabilities": { "resume": {}, "list": {} }
                        },
                        "authMethods": auth,
                        "agentInfo": { "name": "mock-acp-agent", "version": "0.1.0" }
                    }),
                );
            }
            "authenticate" => reply(&mut out, id, json!({})),
            "session/new" => {
                if let Some(dir) = msg.pointer("/params/cwd").and_then(|v| v.as_str()) {
                    cwd = PathBuf::from(dir);
                }
                session_id = format!("mock-session-{}", log_lines.len());
                reply(
                    &mut out,
                    id,
                    json!({ "sessionId": session_id,
                            "modes": { "currentModeId": "default",
                                       "availableModes": [{ "id": "default", "name": "Default" }] } }),
                );
            }
            "session/load" => {
                session_id = msg
                    .pointer("/params/sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&session_id)
                    .to_string();
                reply(&mut out, id, json!({}));
            }
            "session/prompt" => {
                let prompt_text = extract_prompt_text(&msg);

                if scenario == Scenario::Relay {
                    // Answer strictly according to the role marker LJ Hub puts
                    // in the prompt, so the test exercises the real scheduler
                    // and not the mock's imagination.
                    let answer = if prompt_text.contains("ROLE: PLANNER") {
                        relay_plan_json()
                    } else if prompt_text.contains("ROLE: REVIEWER") {
                        // Fail once (while the marker file exists), then pass.
                        let marker = cwd.join("relay-fail-once.marker");
                        if marker.exists() {
                            let _ = std::fs::remove_file(&marker);
                            "验收发现 1 个问题：\n- 交付物缺少必要的测试记录\nREVIEW_STATUS: FAIL\nUNRESOLVED: 1".to_string()
                        } else {
                            "核对完成：HEAD、改动清单与登记产物一致。\nREVIEW_STATUS: PASS"
                                .to_string()
                        }
                    } else if prompt_text.contains("工作模式：read") {
                        // Read-only step: a well-behaved agent must not modify
                        // the workspace here, and the mock behaves like one.
                        format!(
                            "只读分析完成（cwd={}）。\nSTEP_SUMMARY: 只读步骤未修改任何文件",
                            cwd.display()
                        )
                    } else {
                        // Write step: leave real evidence in the working directory.
                        let out_file = cwd.join("worker-output.txt");
                        let previous = std::fs::read_to_string(&out_file).unwrap_or_default();
                        let line = format!("attempt {}\n", previous.lines().count() + 1);
                        let _ = std::fs::write(&out_file, format!("{previous}{line}"));
                        format!(
                            "已在 {} 写入证据。\nSTEP_SUMMARY: 完成 (cwd={})",
                            out_file.display(),
                            cwd.display()
                        )
                    };
                    notify(
                        &mut out,
                        json!({
                            "sessionId": session_id,
                            "update": { "sessionUpdate": "agent_message_chunk",
                                        "content": { "type": "text", "text": answer } }
                        }),
                    );
                    reply(&mut out, id, json!({ "stopReason": "end_turn" }));
                    if let Some(path) = &log_path {
                        let _ =
                            std::fs::write(path, serde_json::to_vec_pretty(&log_lines).unwrap());
                    }
                    continue;
                }
                notify(
                    &mut out,
                    json!({
                        "sessionId": session_id,
                        "update": { "sessionUpdate": "agent_message_chunk",
                                    "content": { "type": "text", "text": format!("收到：{prompt_text}") } }
                    }),
                );
                notify(
                    &mut out,
                    json!({
                        "sessionId": session_id,
                        "update": { "sessionUpdate": "agent_thought_chunk",
                                    "content": { "type": "text", "text": "（思考中）" } }
                    }),
                );
                notify(
                    &mut out,
                    json!({
                        "sessionId": session_id,
                        "update": { "sessionUpdate": "tool_call",
                                    "toolCallId": "tool-1",
                                    "title": "读取 mock.txt",
                                    "kind": "read",
                                    "status": "in_progress",
                                    "locations": [{ "path": "mock.txt" }] }
                    }),
                );

                match scenario {
                    Scenario::Fs => {
                        // Ask the client to read and then write a file, which
                        // exercises LJ Hub's fs handlers and its sandbox.
                        let read_id = next_id();
                        let read_id_expected = read_id.clone();
                        request(
                            &mut out,
                            read_id,
                            "fs/read_text_file",
                            json!({ "sessionId": session_id, "path": "tracked.txt" }),
                        );
                        let write_id = next_id();
                        let write_id_expected = write_id.clone();
                        request(
                            &mut out,
                            write_id,
                            "fs/write_text_file",
                            json!({ "sessionId": session_id,
                                    "path": "written-by-agent.txt",
                                    "content": "written by mock agent\n" }),
                        );
                        // Wait for both responses before finishing.
                        let mut got = 0;
                        while got < 2 {
                            if let Some(resp) = read_one(&line_or_eof()) {
                                log_lines.push(resp.clone());
                                if resp.get("id") == Some(&read_id_expected)
                                    || resp.get("id") == Some(&write_id_expected)
                                {
                                    got += 1;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    Scenario::Permission => {
                        let perm_id = next_id();
                        let perm_id_expected = perm_id.clone();
                        request(
                            &mut out,
                            perm_id,
                            "session/request_permission",
                            json!({
                                "sessionId": session_id,
                                "toolCall": { "toolCallId": "tool-2",
                                              "title": "删除 production 数据" },
                                "options": [
                                    { "optionId": "allow-once", "name": "允许一次", "kind": "allow_once" },
                                    { "optionId": "reject-once", "name": "拒绝", "kind": "reject_once" }
                                ]
                            }),
                        );
                        let response = loop {
                            match read_one(&line_or_eof()) {
                                Some(v) => {
                                    log_lines.push(v.clone());
                                    if v.get("id") == Some(&perm_id_expected) {
                                        break v;
                                    }
                                }
                                None => break Value::Null,
                            }
                        };
                        let outcome = response
                            .pointer("/result/outcome")
                            .cloned()
                            .unwrap_or(json!({ "outcome": "cancelled" }));
                        let allowed = outcome
                            .get("optionId")
                            .and_then(|v| v.as_str())
                            .map(|s| s.starts_with("allow"))
                            .unwrap_or(false);
                        let text = if allowed {
                            "已获得授权，执行操作。"
                        } else {
                            // The whole point: a denied permission must not turn
                            // into an unapproved action.
                            "权限被拒绝，未执行任何操作。"
                        };
                        notify(
                            &mut out,
                            json!({
                                "sessionId": session_id,
                                "update": { "sessionUpdate": "agent_message_chunk",
                                            "content": { "type": "text", "text": text } }
                            }),
                        );
                    }
                    Scenario::Slow => {
                        // Keep emitting until session/cancel arrives.
                        loop {
                            notify(
                                &mut out,
                                json!({
                                    "sessionId": session_id,
                                    "update": { "sessionUpdate": "agent_message_chunk",
                                                "content": { "type": "text", "text": "." } }
                                }),
                            );
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            if let Some(line) = line_or_eof() {
                                if let Some(v) = read_one(&Some(line)) {
                                    log_lines.push(v.clone());
                                    if v.get("method").and_then(|m| m.as_str())
                                        == Some("session/cancel")
                                    {
                                        reply(&mut out, id, json!({ "stopReason": "cancelled" }));
                                        break;
                                    }
                                }
                            }
                            if line_is_eof() {
                                break;
                            }
                        }
                        continue;
                    }
                    _ => {}
                }

                notify(
                    &mut out,
                    json!({
                        "sessionId": session_id,
                        "update": { "sessionUpdate": "tool_call_update",
                                    "toolCallId": "tool-1",
                                    "status": "completed" }
                    }),
                );
                reply(&mut out, id, json!({ "stopReason": "end_turn" }));
            }
            "session/cancel" => { /* notification, nothing to answer */ }
            _ => {
                if id.is_some() {
                    reply_err(
                        &mut out,
                        id,
                        -32601,
                        &format!("mock agent: unknown {method}"),
                    );
                }
            }
        }

        if let Some(path) = &log_path {
            let _ = std::fs::write(path, serde_json::to_vec_pretty(&log_lines).unwrap());
        }
    }
}

/// A minimal but valid plan: one write step reviewed by `codex`, then a read
/// step that depends on it.
fn relay_plan_json() -> String {
    json!({
        "phases": [
            { "id": "p1", "title": "实现", "parallelism": 1, "gate": "all_done" },
            { "id": "p2", "title": "汇总", "parallelism": 1, "gate": "all_done" }
        ],
        "steps": [
            {
                "id": "s1", "phase": "p1", "title": "实现功能",
                "prompt": "在项目里实现功能并留下证据文件",
                "agent": "opencode", "mode": "write",
                "depends_on": [], "verify_by": "codex",
                "max_loops": 3, "max_attempts": 2, "consumes": []
            },
            {
                "id": "s2", "phase": "p2", "title": "汇总结果",
                "prompt": "阅读上游结果并总结",
                "agent": "opencode", "mode": "read",
                "depends_on": ["s1"], "verify_by": null,
                "max_loops": 1, "max_attempts": 1, "consumes": ["s1"]
            }
        ]
    })
    .to_string()
}

fn extract_prompt_text(msg: &Value) -> String {
    msg.pointer("/params/prompt")
        .and_then(|p| p.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn next_id() -> Value {
    use std::sync::atomic::{AtomicI64, Ordering};
    static COUNTER: AtomicI64 = AtomicI64::new(1000);
    json!(COUNTER.fetch_add(1, Ordering::SeqCst))
}

fn reply(out: &mut impl Write, id: Option<Value>, result: Value) {
    let msg = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    write_line(out, &msg);
}

fn reply_err(out: &mut impl Write, id: Option<Value>, code: i64, message: &str) {
    let msg = json!({ "jsonrpc": "2.0", "id": id,
                      "error": { "code": code, "message": message } });
    write_line(out, &msg);
}

fn notify(out: &mut impl Write, params: Value) {
    let msg = json!({ "jsonrpc": "2.0", "method": "session/update", "params": params });
    write_line(out, &msg);
}

fn request(out: &mut impl Write, id: Value, method: &str, params: Value) {
    let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    write_line(out, &msg);
}

fn write_line(out: &mut impl Write, msg: &Value) {
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

fn line_or_eof() -> Option<String> {
    let mut buf = String::new();
    match std::io::stdin().read_line(&mut buf) {
        Ok(0) => None,
        Ok(_) => {
            if buf.trim().is_empty() {
                None
            } else {
                Some(buf)
            }
        }
        Err(_) => None,
    }
}

fn line_is_eof() -> bool {
    false
}

fn read_one(line: &Option<String>) -> Option<Value> {
    let line = line.as_ref()?;
    serde_json::from_str(line).ok()
}
