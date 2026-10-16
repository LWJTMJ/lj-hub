//! Tencent WorkBuddy Open API adapter.
//!
//! WorkBuddy is reachable through an official Open API (OAuth 2.1 + REST,
//! plus an ACP channel for cloud sessions). It is **not** a local stdio agent,
//! so it gets its own transport adapter instead of being forced into the local
//! process model.
//!
//! Endpoint shapes below follow the public documentation at
//! <https://open.workbuddy.cn/docs/openapi> (认证授权 / 本地助理 / 云端任务).
//!
//! Security rules baked into this module:
//!  - `client_secret` is a confidential-client credential. LJ Hub never ships
//!    one and never writes it into the repository; if the user has one they
//!    provide it locally, and the file is created with mode 0600.
//!  - Tokens are never included in error messages, logs or UI payloads.
//!  - Nothing is reported as connected unless an authenticated call succeeded.

use crate::model::AgentStatus;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_BASE_URL: &str = "https://www.workbuddy.cn";
/// Docs warn the token lifetime varies per sandbox; LJ Hub always asks for a
/// fresh token via `GET /tasks/{id}` instead of assuming a duration.
pub const TOKEN_ENDPOINT: &str = "/openapi/v2/token";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkBuddyConfig {
    #[serde(default)]
    pub base_url: Option<String>,
    pub client_id: Option<String>,
    /// Confidential-client secret. Optional: without it LJ Hub can still use an
    /// access token obtained elsewhere, but it cannot perform a code exchange.
    pub client_secret: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub open_id: Option<String>,
    pub redirect_uri: Option<String>,
}

impl WorkBuddyConfig {
    pub fn base(&self) -> String {
        self.base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string()
    }

    /// Is there enough material to attempt an authenticated call?
    pub fn has_token(&self) -> bool {
        self.access_token
            .as_deref()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false)
    }
}

/// Credentials live outside the repository, with owner-only permissions.
pub fn credentials_path(data_dir: &Path) -> PathBuf {
    data_dir.join("credentials").join("workbuddy.json")
}

pub fn load_config(data_dir: &Path) -> WorkBuddyConfig {
    let path = credentials_path(data_dir);
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_config(data_dir: &Path, config: &WorkBuddyConfig) -> Result<(), String> {
    let path = credentials_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct ApiEnvelope<T> {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    data: Option<T>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalAssistantStatus {
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentMessage {
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub message_id: String,
    pub role: String,
    #[serde(default)]
    pub content: Vec<String>,
    #[serde(default)]
    pub msg_type: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudTask {
    pub task_id: String,
    pub status: String,
    #[serde(default)]
    pub name: Option<String>,
    /// ACP connection address for the cloud sandbox session.
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub expire_at: Option<i64>,
    // `token` is deliberately NOT deserialized into this struct so it cannot be
    // accidentally echoed into the UI or a log.
}

/// Response of a token exchange; the token itself is kept in memory only.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub open_id: Option<String>,
}

#[derive(Debug)]
pub enum ApiError {
    /// No credentials installed at all.
    NotConfigured(String),
    /// 401/403: a token is needed or was rejected.
    Unauthorized(String),
    /// Network/transport problem.
    Transport(String),
    /// Anything else, with the server's own message.
    Server(String),
}

impl ApiError {
    pub fn message(&self) -> String {
        match self {
            ApiError::NotConfigured(m)
            | ApiError::Unauthorized(m)
            | ApiError::Transport(m)
            | ApiError::Server(m) => m.clone(),
        }
    }

    pub fn status(&self) -> AgentStatus {
        match self {
            ApiError::NotConfigured(_) => AgentStatus::ConfigRequired,
            ApiError::Unauthorized(_) => AgentStatus::AuthRequired,
            ApiError::Transport(_) => AgentStatus::Error,
            ApiError::Server(_) => AgentStatus::Error,
        }
    }
}

pub struct WorkBuddyClient {
    config: WorkBuddyConfig,
    http: reqwest::Client,
}

impl WorkBuddyClient {
    pub fn new(config: WorkBuddyConfig) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(format!("lj-hub/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ApiError::Transport(format!("HTTP 客户端初始化失败：{e}")))?;
        Ok(Self { config, http })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.base(), path)
    }

    fn require_token(&self) -> Result<&str, ApiError> {
        self.config
            .access_token
            .as_deref()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                ApiError::NotConfigured(
                "尚未配置 WorkBuddy 访问令牌。需要先完成官方 OAuth 授权（开发者应用审核通过后）。"
                    .to_string(),
            )
            })
    }

    /// WorkBuddy wraps most responses in `{code,msg,request_id,data}`.
    fn handle<T: for<'de> Deserialize<'de>>(
        &self,
        status: reqwest::StatusCode,
        body: &str,
    ) -> Result<T, ApiError> {
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(ApiError::Unauthorized(format!(
                "WorkBuddy 返回 {status}：令牌缺失、无效或权限不足（scope 不足）。"
            )));
        }
        let value: serde_json::Value = serde_json::from_str(body).map_err(|e| {
            ApiError::Server(format!("无法解析 WorkBuddy 响应（HTTP {status}）：{e}"))
        })?;
        if value.get("code").is_some() {
            // Envelope style: {code, msg, request_id, data}
            let envelope: ApiEnvelope<T> = serde_json::from_value(value).map_err(|e| {
                ApiError::Server(format!("无法解析 WorkBuddy 响应（HTTP {status}）：{e}"))
            })?;
            let code = envelope.code.unwrap_or(0);
            if code != 0 {
                return Err(ApiError::Server(format!(
                    "WorkBuddy 业务错误 code={code}：{}（request_id={}）",
                    envelope.msg.unwrap_or_default(),
                    envelope.request_id.unwrap_or_else(|| "-".into())
                )));
            }
            envelope
                .data
                .ok_or_else(|| ApiError::Server("WorkBuddy 响应缺少 data 字段".to_string()))
        } else {
            // Some endpoints (e.g. POST /tasks) answer with a bare object, as
            // shown in the official documentation.
            serde_json::from_value(value).map_err(|e| {
                ApiError::Server(format!("无法解析 WorkBuddy 响应（HTTP {status}）：{e}"))
            })
        }
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, ApiError> {
        let token = self.require_token()?.to_string();
        let response = self
            .http
            .get(self.url(path))
            .bearer_auth(token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| ApiError::Transport(format!("请求失败：{e}")))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        self.handle(status, &body)
    }

    /// GET /openapi/v2/localassistant — PC 端本地助理在线状态。
    pub async fn local_assistant_status(&self) -> Result<LocalAssistantStatus, ApiError> {
        self.get("/openapi/v2/localassistant").await
    }

    /// GET /openapi/v2/user/profile
    pub async fn profile(&self) -> Result<serde_json::Value, ApiError> {
        self.get("/openapi/v2/user/profile").await
    }

    /// POST /openapi/v2/localassistant/message
    pub async fn send_local_message(
        &self,
        content: &str,
        msg_type: &str,
    ) -> Result<SentMessage, ApiError> {
        let token = self.require_token()?.to_string();
        let response = self
            .http
            .post(self.url("/openapi/v2/localassistant/message"))
            .bearer_auth(token)
            .header("Accept", "application/json")
            .json(&serde_json::json!({ "content": content, "msg_type": msg_type }))
            .send()
            .await
            .map_err(|e| ApiError::Transport(format!("请求失败：{e}")))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        self.handle(status, &body)
    }

    /// GET /openapi/v2/localassistant/message — pagination or incremental mode.
    pub async fn local_history(
        &self,
        limit: u32,
        offset: u32,
        since_message_id: Option<&str>,
    ) -> Result<Vec<HistoryMessage>, ApiError> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            messages: Vec<HistoryMessage>,
        }
        let path = match since_message_id {
            Some(id) => format!(
                "/openapi/v2/localassistant/message?message_id={}",
                urlencode(id)
            ),
            None => format!("/openapi/v2/localassistant/message?limit={limit}&offset={offset}"),
        };
        let page: Page = self.get(&path).await?;
        Ok(page.messages)
    }

    /// POST /openapi/v2/tasks — create a cloud task; the reply carries the ACP
    /// link for the sandbox session.
    pub async fn create_task(
        &self,
        prompt: &str,
        name: Option<&str>,
    ) -> Result<CloudTask, ApiError> {
        let token = self.require_token()?.to_string();
        let payload = match name {
            Some(n) => serde_json::json!({ "prompt": prompt, "name": n }),
            None => serde_json::json!({ "prompt": prompt }),
        };
        let response = self
            .http
            .post(self.url("/openapi/v2/tasks"))
            .bearer_auth(token)
            .header("Accept", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| ApiError::Transport(format!("请求失败：{e}")))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        self.handle(status, &body)
    }

    /// GET /openapi/v2/tasks/{task_id} — refresh the ACP link and token.
    pub async fn get_task(&self, task_id: &str) -> Result<CloudTask, ApiError> {
        self.get(&format!("/openapi/v2/tasks/{}", urlencode(task_id)))
            .await
    }

    /// POST /openapi/v2/token with `grant_type=authorization_code`.
    ///
    /// This is a confidential-client flow: the documentation states that
    /// `client_secret` must never be exposed in frontend or client code. LJ Hub
    /// therefore only performs it when the user has explicitly installed their
    /// own credentials locally, and reports CONFIG_REQUIRED otherwise.
    pub async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
    ) -> Result<TokenResponse, ApiError> {
        let client_id = self
            .config
            .client_id
            .clone()
            .ok_or_else(|| ApiError::NotConfigured("缺少 client_id".to_string()))?;
        let client_secret = self.config.client_secret.clone().ok_or_else(|| {
            ApiError::NotConfigured(
                "缺少 client_secret：WorkBuddy 的授权码流程属于机密客户端，LJ Hub 不在客户端内置任何\
                 密钥。请在本机凭据文件中配置你自己的应用密钥，或改由你自建的后端完成换票。"
                    .to_string(),
            )
        })?;
        let response = self
            .http
            .post(self.url(TOKEN_ENDPOINT))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", &client_id),
                ("client_secret", &client_secret),
            ])
            .send()
            .await
            .map_err(|e| ApiError::Transport(format!("请求失败：{e}")))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ApiError::Unauthorized(format!(
                "换取令牌失败（HTTP {status}），请检查授权码与回调地址是否一致。"
            )));
        }
        serde_json::from_str(&body).map_err(|e| ApiError::Server(format!("令牌响应无法解析：{e}")))
    }

    /// POST /openapi/v2/token with `grant_type=refresh_token`.
    pub async fn refresh(&self) -> Result<TokenResponse, ApiError> {
        let client_id = self
            .config
            .client_id
            .clone()
            .ok_or_else(|| ApiError::NotConfigured("缺少 client_id".to_string()))?;
        let client_secret = self
            .config
            .client_secret
            .clone()
            .ok_or_else(|| ApiError::NotConfigured("缺少 client_secret".to_string()))?;
        let refresh_token = self
            .config
            .refresh_token
            .clone()
            .ok_or_else(|| ApiError::NotConfigured("缺少 refresh_token".to_string()))?;
        let response = self
            .http
            .post(self.url(TOKEN_ENDPOINT))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh_token),
                ("client_id", &client_id),
                ("client_secret", &client_secret),
            ])
            .send()
            .await
            .map_err(|e| ApiError::Transport(format!("请求失败：{e}")))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ApiError::Unauthorized(format!(
                "刷新令牌失败（HTTP {status}）"
            )));
        }
        serde_json::from_str(&body).map_err(|e| ApiError::Server(format!("令牌响应无法解析：{e}")))
    }

    /// The OAuth authorize URL the user opens in a browser.
    pub fn authorize_url(&self, scope: Option<&str>, state: &str) -> Result<String, ApiError> {
        let client_id = self
            .config
            .client_id
            .clone()
            .ok_or_else(|| ApiError::NotConfigured("缺少 client_id".to_string()))?;
        let redirect_uri = self
            .config
            .redirect_uri
            .clone()
            .ok_or_else(|| ApiError::NotConfigured("缺少 redirect_uri".to_string()))?;
        let mut url = format!(
            "{}/openapi/v2/authorize?response_type=code&client_id={}&redirect_uri={}&state={}",
            self.config.base(),
            urlencode(&client_id),
            urlencode(&redirect_uri),
            urlencode(state)
        );
        if let Some(s) = scope {
            url.push_str(&format!("&scope={}", urlencode(s)));
        }
        Ok(url)
    }
}

/// Minimal percent-encoding for query values. Avoids pulling in a URL crate
/// for the few parameters used here.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A tiny HTTP/1.1 server: enough to exercise the adapter against real
    /// bytes without any network access or vendor account.
    struct MockServer {
        base: String,
        requests: Arc<Mutex<Vec<(String, String, String)>>>,
        shutdown: Arc<Mutex<bool>>,
    }

    impl MockServer {
        fn start(routes: Vec<(&'static str, u16, &'static str)>) -> MockServer {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let shutdown = Arc::new(Mutex::new(false));
            let requests_thread = requests.clone();
            let shutdown_thread = shutdown.clone();
            listener.set_nonblocking(true).unwrap();
            std::thread::spawn(move || {
                let routes: Vec<(String, u16, String)> = routes
                    .into_iter()
                    .map(|(p, s, b)| (p.to_string(), s, b.to_string()))
                    .collect();
                loop {
                    if *shutdown_thread.lock().unwrap() {
                        return;
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            // On BSD/macOS an accepted socket inherits the
                            // listener's non-blocking flag; restore blocking IO
                            // for the request reader below.
                            let _ = stream.set_nonblocking(false);
                            let routes = routes.clone();
                            let requests = requests_thread.clone();
                            std::thread::spawn(move || {
                                let mut reader = BufReader::new(&stream);
                                let mut request_line = String::new();
                                if reader.read_line(&mut request_line).is_err() {
                                    return;
                                }
                                let parts: Vec<&str> = request_line.split_whitespace().collect();
                                let method = parts.first().copied().unwrap_or("").to_string();
                                let path = parts.get(1).copied().unwrap_or("").to_string();
                                let mut auth = String::new();
                                let mut length = 0usize;
                                loop {
                                    let mut line = String::new();
                                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                        break;
                                    }
                                    let trimmed = line.trim_end();
                                    if trimmed.is_empty() {
                                        break;
                                    }
                                    let lower = trimmed.to_ascii_lowercase();
                                    if lower.starts_with("authorization:") {
                                        auth = trimmed[14..].trim().to_string();
                                    }
                                    if lower.starts_with("content-length:") {
                                        length = trimmed[15..].trim().parse().unwrap_or(0);
                                    }
                                }
                                if length > 0 {
                                    let mut body = vec![0u8; length];
                                    use std::io::Read;
                                    let _ = reader.read_exact(&mut body);
                                }
                                requests.lock().unwrap().push((method, path.clone(), auth));
                                let matched =
                                    routes.iter().find(|(p, _, _)| path.starts_with(p.as_str()));
                                let (status, body) = match matched {
                                    Some((_, s, b)) => (*s, b.clone()),
                                    None => {
                                        (404, "{\"code\":404,\"msg\":\"not found\"}".to_string())
                                    }
                                };
                                let mut response = format!(
                                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(),
                                    body
                                );
                                if status != 200 {
                                    response = response.replace("HTTP/1.1 200 OK", "HTTP/1.1 404");
                                }
                                let _ = stream.write_all(response.as_bytes());
                            });
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(_) => return,
                    }
                }
            });
            MockServer {
                base: format!("http://127.0.0.1:{}", addr.port()),
                requests,
                shutdown,
            }
        }

        fn calls(&self) -> Vec<(String, String, String)> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            *self.shutdown.lock().unwrap() = true;
        }
    }

    fn config_for(server: &MockServer) -> WorkBuddyConfig {
        WorkBuddyConfig {
            base_url: Some(server.base.clone()),
            access_token: Some("test-token".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn local_assistant_status_is_parsed() {
        let server = MockServer::start(vec![(
            "/openapi/v2/localassistant",
            200,
            "{\"code\":0,\"msg\":\"success\",\"request_id\":\"r1\",\"data\":{\"online\":true}}",
        )]);
        let client = WorkBuddyClient::new(config_for(&server)).unwrap();
        let status = client.local_assistant_status().await.unwrap();
        assert!(status.online);
        assert_eq!(server.calls()[0].1, "/openapi/v2/localassistant");
        assert_eq!(server.calls()[0].2, "Bearer test-token");
    }

    #[tokio::test]
    async fn history_messages_are_parsed() {
        let server = MockServer::start(vec![(
            "/openapi/v2/localassistant/message",
            200,
            "{\"code\":0,\"msg\":\"success\",\"data\":{\"messages\":[{\"message_id\":\"m1\",\"role\":\"assistant\",\"content\":[\"你好\"],\"msg_type\":\"text\",\"created_at\":\"2026-07-30T10:00:05Z\"}]}}",
        )]);
        let client = WorkBuddyClient::new(config_for(&server)).unwrap();
        let messages = client.local_history(20, 0, None).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, vec!["你好".to_string()]);
    }

    #[tokio::test]
    async fn unauthorized_is_reported_as_auth_required_not_success() {
        let server = MockServer::start(vec![(
            "/openapi/v2/localassistant",
            401,
            "{\"code\":\"invalid_token\"}",
        )]);
        let client = WorkBuddyClient::new(config_for(&server)).unwrap();
        let error = client.local_assistant_status().await.unwrap_err();
        assert_eq!(error.status(), AgentStatus::AuthRequired);
    }

    #[tokio::test]
    async fn missing_token_is_config_required() {
        let server = MockServer::start(vec![]);
        let config = WorkBuddyConfig {
            base_url: Some(server.base.clone()),
            ..Default::default()
        };
        let client = WorkBuddyClient::new(config).unwrap();
        let error = client.local_assistant_status().await.unwrap_err();
        assert_eq!(error.status(), AgentStatus::ConfigRequired);
        assert!(server.calls().is_empty(), "没有令牌时不应发出请求");
    }

    #[tokio::test]
    async fn business_error_code_is_not_treated_as_success() {
        let server = MockServer::start(vec![(
            "/openapi/v2/tasks",
            200,
            "{\"code\":1,\"msg\":\"invalid_request\",\"request_id\":\"r9\"}",
        )]);
        let client = WorkBuddyClient::new(config_for(&server)).unwrap();
        let error = client.create_task("hi", None).await.unwrap_err();
        assert!(error.message().contains("invalid_request"));
    }

    #[tokio::test]
    async fn cloud_task_does_not_leak_token_field() {
        let server = MockServer::start(vec![(
            "/openapi/v2/tasks",
            200,
            "{\"task_id\":\"t-1\",\"status\":\"working\",\"name\":\"n\",\"link\":\"https://acp.example/s/1\",\"token\":\"sk-sandbox-secret\",\"expire_at\":1786087200}",
        )]);
        let client = WorkBuddyClient::new(config_for(&server)).unwrap();
        let task = client.create_task("hi", Some("n")).await.unwrap();
        assert_eq!(task.task_id, "t-1");
        let serialized = serde_json::to_string(&task).unwrap();
        assert!(
            !serialized.contains("secret"),
            "ACP token 绝不能被序列化进 UI 或存储：{serialized}"
        );
    }

    #[test]
    fn authorize_url_encodes_parameters() {
        let config = WorkBuddyConfig {
            client_id: Some("app_1".into()),
            redirect_uri: Some("https://example.com/cb?x=1".into()),
            ..Default::default()
        };
        let client = WorkBuddyClient::new(config).unwrap();
        let url = client
            .authorize_url(Some("user.task.readable"), "state1")
            .unwrap();
        assert!(url.starts_with("https://www.workbuddy.cn/openapi/v2/authorize?response_type=code"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fexample.com%2Fcb%3Fx%3D1"));
    }

    #[test]
    fn urlencode_escapes_reserved_characters() {
        assert_eq!(urlencode("a b/c?d=e&f"), "a%20b%2Fc%3Fd%3De%26f");
    }
}
