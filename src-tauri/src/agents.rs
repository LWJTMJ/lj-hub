//! Agent integration registry: the six required agents plus one clearly
//! separated CodeBuddy Code compatibility entry.
//!
//! Nothing in this module assumes an agent works. It resolves an executable,
//! asks the executable for its version, and reports what it found. Whether an
//! agent can actually be driven is decided by a real ACP handshake in
//! `acp::probe`, and until that handshake has run the status stays
//! `NOT_TESTED`.

use crate::model::{
    AgentCapabilitiesReport, AgentOverride, AgentReport, AgentStatus, TransportKind,
};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentId {
    Codex,
    WorkBuddy,
    Qoder,
    QoderCn,
    Trae,
    DeepSeekHarness,
    OpenCode,
    CodeBuddyCode,
}

pub struct AgentDefinition {
    pub id: &'static str,
    pub display_name: &'static str,
    pub vendor: &'static str,
    pub transport: TransportKind,
    /// Command names resolved through `PATH`.
    pub path_names: &'static [&'static str],
    /// Absolute candidate paths (`~` is expanded).
    pub candidates: &'static [&'static str],
    /// Arguments that start the ACP server.
    pub acp_args: &'static [&'static str],
    /// Arguments used to read the version.
    pub version_args: &'static [&'static str],
    /// Package spec used when only the official npm adapter is available.
    /// Pinned on purpose: unpinned `npx latest` is not acceptable for a
    /// released build.
    pub npm_adapter: Option<&'static str>,
    pub doc_url: &'static str,
    pub notes: &'static str,
    /// True for entries that exist only as a compatibility path and must not be
    /// presented as a full integration of the vendor product.
    pub compatibility_only: bool,
}

/// Codex ACP adapter version that LJ Hub pins when the user chooses to let
/// LJ Hub run the official adapter through `npx`.
pub const CODEX_ACP_PINNED_SPEC: &str = "@agentclientprotocol/codex-acp@0.9.0";

pub const AGENTS: &[AgentDefinition] = &[
    AgentDefinition {
        id: "codex",
        display_name: "Codex",
        vendor: "OpenAI",
        transport: TransportKind::AcpStdio,
        path_names: &["codex-acp"],
        candidates: &[
            "~/.local/bin/codex-acp",
            "~/.npm-global/bin/codex-acp",
            "/opt/homebrew/bin/codex-acp",
            "/usr/local/bin/codex-acp",
        ],
        acp_args: &[],
        version_args: &["--version"],
        npm_adapter: Some(CODEX_ACP_PINNED_SPEC),
        doc_url: "https://github.com/agentclientprotocol/codex-acp",
        notes:
            "LJ Hub drives Codex through the official ACP adapter. The adapter needs the Codex CLI \
                and its existing login; LJ Hub does not replace it with a paid API key. \
                The archived zed-industries/codex-acp repository is not used.",
        compatibility_only: false,
    },
    AgentDefinition {
        id: "workbuddy",
        display_name: "WorkBuddy",
        vendor: "Tencent",
        transport: TransportKind::OpenApiHttp,
        path_names: &[],
        candidates: &[],
        acp_args: &[],
        version_args: &[],
        npm_adapter: None,
        doc_url: "https://open.workbuddy.cn/docs/openapi",
        notes: "Tencent WorkBuddy Open API. Requires an approved developer application and OAuth \
                credentials; LJ Hub therefore reports CONFIG_REQUIRED until such credentials are \
                installed. No connectivity is simulated.",
        compatibility_only: false,
    },
    AgentDefinition {
        id: "qoder",
        display_name: "Qoder (国际版)",
        vendor: "Alibaba",
        transport: TransportKind::AcpStdio,
        path_names: &["qoder"],
        candidates: &[
            "~/.local/bin/qoder",
            "/opt/homebrew/bin/qoder",
            "/Applications/Qoder.app/Contents/Resources/bin/qoder",
            "~/.qoder/bin/qoder",
        ],
        acp_args: &["--acp"],
        version_args: &["--version"],
        npm_adapter: None,
        doc_url: "https://qoder.com/",
        notes: "International Qoder CLI. Uses the CLI and login state already present on this \
                machine; LJ Hub never reinstalls or replaces the vendor CLI.",
        compatibility_only: false,
    },
    AgentDefinition {
        id: "qodercn",
        display_name: "Qoder CN (中国版)",
        vendor: "Alibaba",
        transport: TransportKind::AcpStdio,
        path_names: &["qodercn"],
        candidates: &[
            "~/.local/bin/qodercn",
            "/opt/homebrew/bin/qodercn",
            "/Applications/Qoder CN.app/Contents/Resources/bin/qodercn",
            "~/.qoder/bin/qodercn",
        ],
        acp_args: &["--acp"],
        version_args: &["--version"],
        npm_adapter: None,
        doc_url: "https://qoder.com/",
        notes: "Qoder China build. Kept separate from the international build because accounts, \
                models and entitlements differ.",
        compatibility_only: false,
    },
    AgentDefinition {
        id: "trae",
        display_name: "TRAE",
        vendor: "ByteDance",
        transport: TransportKind::AcpStdio,
        path_names: &["traecli"],
        candidates: &[
            "~/.local/bin/traecli",
            "/opt/homebrew/bin/traecli",
            "~/.trae/bin/traecli",
            "/Applications/Trae CN.app/Contents/Resources/app/bin/traecli",
        ],
        acp_args: &["acp", "serve"],
        version_args: &["--version"],
        npm_adapter: None,
        doc_url: "https://www.trae.ai/",
        notes:
            "TRAE CLI ACP server (`traecli acp serve`). The exact subcommand is verified through \
                the startup handshake rather than assumed.",
        compatibility_only: false,
    },
    AgentDefinition {
        id: "deepseek-harness",
        display_name: "DeepSeek Harness",
        vendor: "DeepSeek",
        transport: TransportKind::AcpStdio,
        path_names: &["dsh"],
        candidates: &[
            "~/.dsh/bin/dsh",
            "~/.local/bin/dsh",
            "/opt/homebrew/bin/dsh",
            "/Applications/DeepSeek Harness.app/Contents/Resources/runtime/cli/bin/dsh",
        ],
        acp_args: &["acp"],
        version_args: &["--version"],
        npm_adapter: None,
        doc_url: "https://github.com/deepseek-ai/deepseek-harness",
        notes:
            "DeepSeek Harness is evolving quickly, so its launch and compatibility logic is kept \
                behind this one adapter entry with no Harness internals copied into LJ Hub.",
        compatibility_only: false,
    },
    AgentDefinition {
        id: "opencode",
        display_name: "OpenCode",
        vendor: "OpenCode",
        transport: TransportKind::AcpStdio,
        path_names: &["opencode"],
        candidates: &[
            "~/.local/bin/opencode",
            "/opt/homebrew/bin/opencode",
            "~/.opencode/bin/opencode",
            "~/.bun/bin/opencode",
            "/Applications/OpenCode.app/Contents/Resources/opencode-cli",
        ],
        acp_args: &["acp"],
        version_args: &["--version"],
        npm_adapter: None,
        doc_url: "https://opencode.ai/",
        notes: "Uses the OpenCode CLI already installed on this machine, including its existing \
                configuration and login state.",
        compatibility_only: false,
    },
    AgentDefinition {
        id: "codebuddy",
        display_name: "CodeBuddy Code (兼容入口)",
        vendor: "Tencent",
        transport: TransportKind::AcpStdio,
        path_names: &["codebuddy"],
        candidates: &[
            "~/.local/bin/codebuddy",
            "/opt/homebrew/bin/codebuddy",
            "~/.codebuddy/bin/codebuddy",
        ],
        acp_args: &["--acp"],
        version_args: &["--version"],
        npm_adapter: None,
        doc_url: "https://www.codebuddy.ai/",
        notes: "Independent compatibility entry for the CodeBuddy Code CLI. This is NOT the \
                WorkBuddy integration and must never be presented as one.",
        compatibility_only: true,
    },
];

pub fn find_definition(id: &str) -> Option<&'static AgentDefinition> {
    AGENTS.iter().find(|a| a.id == id)
}

fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// `PATH` lookup without pulling in an extra crate.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = expand_home(name);
        return if is_executable(&p) { Some(p) } else { None };
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Resolve the executable that should be launched for an agent.
///
/// Order: explicit user override, then `PATH`, then the known absolute
/// locations. Returns the path and whether it came from the override.
pub fn resolve_executable(
    def: &AgentDefinition,
    over: Option<&AgentOverride>,
) -> Option<(PathBuf, bool)> {
    if let Some(o) = over {
        if let Some(exe) = o.executable.as_ref().filter(|s| !s.trim().is_empty()) {
            let p = expand_home(exe.trim());
            if is_executable(&p) {
                return Some((p, true));
            }
            // An override that points at a missing file is reported as such by
            // the caller through the status message; do not silently fall back.
            return None;
        }
    }
    for name in def.path_names {
        if let Some(p) = which(name) {
            return Some((p, false));
        }
    }
    for cand in def.candidates {
        let p = expand_home(cand);
        if is_executable(&p) {
            return Some((p, false));
        }
    }
    None
}

/// True when the override was configured but unusable. Lets the UI explain the
/// difference between "not installed" and "your override is wrong".
pub fn override_is_broken(_def: &AgentDefinition, over: Option<&AgentOverride>) -> Option<String> {
    let o = over?;
    let exe = o.executable.as_ref().filter(|s| !s.trim().is_empty())?;
    let p = expand_home(exe.trim());
    if is_executable(&p) {
        None
    } else {
        Some(format!("配置的可执行文件不存在或不可执行：{}", p.display()))
    }
}

pub fn base_report(def: &AgentDefinition) -> AgentReport {
    AgentReport {
        id: def.id.to_string(),
        display_name: def.display_name.to_string(),
        vendor: def.vendor.to_string(),
        transport: def.transport,
        status: AgentStatus::NotTested,
        executable: None,
        version: None,
        acp_args: def.acp_args.iter().map(|s| s.to_string()).collect(),
        overridden: false,
        message: String::new(),
        auth_methods: Vec::new(),
        agent_info: None,
        capabilities: AgentCapabilitiesReport::default(),
        doc_url: def.doc_url.to_string(),
        notes: def.notes.to_string(),
        last_tested_at: None,
    }
}

/// Build a report whose status is based purely on local facts (executable +
/// version). It never claims the agent is usable.
pub fn local_report(def: &'static AgentDefinition, over: Option<&AgentOverride>) -> AgentReport {
    let mut report = base_report(def);

    if def.transport == TransportKind::OpenApiHttp {
        report.status = AgentStatus::ConfigRequired;
        report.message = "WorkBuddy 通过官方 Open API 接入，需要已审核的开发者应用与 OAuth 凭据；\
                          未配置凭据前不进行任何连通性声明。"
            .to_string();
        return report;
    }

    if let Some(broken) = override_is_broken(def, over) {
        report.status = AgentStatus::Error;
        report.message = broken;
        return report;
    }

    match resolve_executable(def, over) {
        Some((path, overridden)) => {
            report.executable = Some(path.display().to_string());
            report.overridden = overridden;
            report.status = AgentStatus::NotTested;
            report.message = if def.compatibility_only {
                "已找到可执行文件；这是一个兼容入口，尚未通过 ACP 握手验证。".to_string()
            } else {
                "已找到可执行文件，尚未进行 ACP 握手测试。".to_string()
            };
        }
        None => {
            report.status = AgentStatus::NotInstalled;
            report.message = match def.npm_adapter {
                Some(spec) => format!(
                    "未找到本机可执行文件。可安装官方适配器后由 LJ Hub 启动：npx -y {spec}（固定版本，不会自动升级）。"
                ),
                None => "未在本机检测到该 Agent 的可执行文件。".to_string(),
            };
        }
    }
    report
}

/// Ask an executable for its version. Any failure is reported verbatim; it is
/// never converted into a success.
pub async fn probe_version(exe: &Path, args: &[&str]) -> Result<String, String> {
    use tokio::process::Command;
    let mut cmd = Command::new(exe);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let fut = cmd.output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(20), fut).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("启动失败：{e}")),
        Err(_) => return Err("版本探测超时（20 秒）".to_string()),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stdout
        .lines()
        .chain(stderr.lines())
        .map(|l| l.trim())
        .find(|l| !l.is_empty());
    match line {
        Some(l) => Ok(l.to_string()),
        None => Err(format!(
            "命令未输出可识别的版本信息（退出码 {:?}）",
            out.status.code()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_required_agents_are_registered() {
        for id in [
            "codex",
            "workbuddy",
            "qoder",
            "qodercn",
            "trae",
            "deepseek-harness",
            "opencode",
        ] {
            assert!(find_definition(id).is_some(), "missing agent {id}");
        }
    }

    #[test]
    fn codebuddy_is_marked_as_compatibility_only() {
        let def = find_definition("codebuddy").unwrap();
        assert!(def.compatibility_only);
        assert_ne!(def.id, "workbuddy");
    }

    #[test]
    fn workbuddy_never_reports_available_without_credentials() {
        let def = find_definition("workbuddy").unwrap();
        let report = local_report(def, None);
        assert_eq!(report.status, AgentStatus::ConfigRequired);
        assert!(report.executable.is_none());
    }

    #[test]
    fn unknown_agent_is_not_reported_available() {
        // A missing binary must never be upgraded to AVAILABLE.
        let def = find_definition("opencode").unwrap();
        let over = AgentOverride {
            executable: Some("/definitely/not/here/opencode".into()),
            args: vec![],
            allow_npm_adapter: false,
        };
        let report = local_report(def, Some(&over));
        assert_eq!(report.status, AgentStatus::Error);
    }

    #[test]
    fn path_lookup_finds_sh() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }
}
