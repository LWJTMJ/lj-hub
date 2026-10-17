# Agent 接入与实测状态

本文件记录**本机实测证据**，不是设计目标。状态含义：

| 状态 | 含义 |
|---|---|
| `AVAILABLE` | 真实 ACP 握手通过；可以开始会话 |
| `NOT_INSTALLED` | 本机未找到可执行文件 |
| `AUTH_REQUIRED` | Agent 自己声明需要认证（`authMethods` 非空） |
| `CONFIG_REQUIRED` | 缺少外部授权或凭据（如 WorkBuddy 开发者应用） |
| `UNSUPPORTED` | 协议层不支持当前用法 |
| `ERROR` | 可执行文件存在但启动/握手失败 |
| `NOT_TESTED` | 尚未验证——**不得视为可用** |

## 实测记录（2026-10-16，macOS 26.6.2 / arm64）

| Agent | 状态 | 证据 |
|---|---|---|
| OpenCode | `AVAILABLE` | `/Applications/OpenCode.app/Contents/Resources/opencode-cli`，`opencode acp` 握手返回 agentInfo `OpenCode 2.0.30`；并完成一轮真实模型回合（`stop_reason=end_turn`，回复"收到"） |
| DeepSeek Harness | `AVAILABLE` | `/Applications/DeepSeek Harness.app/Contents/Resources/runtime/cli/bin/dsh`，`dsh acp` 握手返回 `deepseek-harness-acp 0.0.1`；真实模型回合**未测试** |
| Codex | `NOT_INSTALLED` | 本机无 `codex-acp`，亦无 Codex CLI。接入方式为官方 `@agentclientprotocol/codex-acp`；LJ Hub **只在用户显式启用后**用固定版本 `npx` 启动它，不使用 `latest` |
| WorkBuddy | `CONFIG_REQUIRED` | Open API 适配器已实现并通过协议级测试（含本地 mock HTTP 服务）；正式接入需开放平台审核通过的第三方应用 + OAuth 凭据 |
| Qoder（国际版） | `NOT_INSTALLED` | 本机装有 `Qoder.app`，但 PATH 中无 `qoder` CLI |
| Qoder CN（中国版） | `NOT_INSTALLED` | 本机装有 `Qoder CN.app`，但无 `qodercn` CLI |
| TRAE | `NOT_INSTALLED` | 本机装有 `Trae CN.app`，但无 `traecli` |
| CodeBuddy Code | `NOT_INSTALLED` | 独立兼容入口；**不代表 WorkBuddy 已接入** |

同一台机器上再次确认的方式：

```bash
cd src-tauri
cargo test --test real_agents -- --nocapture
```

输出会逐条列出"已安装 / 未安装"与真实握手结论。

## 各 Agent 的启动方式

| Agent | 启动命令 | 认证 |
|---|---|---|
| Codex | 官方适配器（用户提供路径，或固定版本 npx） | 复用 Codex 已有登录，不强制改用付费 API Key |
| WorkBuddy | 官方 Open API（HTTPS） | OAuth 2.1 授权码（机密客户端） |
| Qoder | `qoder --acp` | 复用 Qoder CLI 已有登录 |
| Qoder CN | `qodercn --acp` | 复用 Qoder CN CLI 已有登录 |
| TRAE | `traecli acp serve` | 复用 TRAE CLI 已有登录 |
| DeepSeek Harness | `dsh acp`（等价于 `dsh --profile acp`） | 复用 Harness 已有配置 |
| OpenCode | `opencode acp` | 复用 OpenCode 已有配置（本机为 `opencode-login`） |

LJ Hub **不会**：替换你的 CLI、覆盖你的登录状态、自动升级厂商工具、
在未启用时运行 `npx latest`、或把厂商账号迁移到它自己的存储里。

## 能力分级（严格区分，不可互相冒充）

LJ Hub 对每个 Agent 分别记录：

| 级别 | 判据 | 谁能设置 |
|---|---|---|
| 已安装 | 目录/应用存在 | 检测 |
| 可执行文件可发现 | 解析到可执行路径（PATH → 已知路径 → 用户覆盖） | 检测 |
| 协议握手成功 | 真实 ACP `initialize` 成功 | 「连接测试」 |
| 认证要求 | Agent 在握手时声明 `authMethods` | 「连接测试」 |
| **真实模型回合成功** | 真的发一轮 prompt 并拿到回答 | 「验证真实回合」（会消耗额度，需手动点击） |
| **可用于自动调度** | 握手成功且真实回合已验证通过 | 由上一行推导 |

安装、握手都不能让某个 Agent 获得「可用于自动接力」的状态。
真实回合失败的 Agent 会被记住并停止被调度，界面显示供应商原文错误。

若某个 CLI 需要环境变量（例如 DeepSeek Harness 需要 `DEEPSEEK_API_KEY`），
在 Agent 设置里填写**变量名**即可：LJ Hub 只从自己的进程环境读取值并传给子进程，
**不保存、不记录、不回显**该值。

## 自定义与诊断

设置 → Agent 管理：

- 每个 Agent 显示：状态、可执行文件路径、版本、真实握手返回的 agentInfo、
  认证方式列表、已确认能力（load_session / resume / list / 图片 / 终端）；
- 可手动覆盖可执行文件路径与启动参数；
- **连接测试**会真的启动该 Agent 并跑一次 `initialize`，只有通过才会把状态改为
  `AVAILABLE` / `AUTH_REQUIRED`；
- 任何未测试的条目一律显示 `NOT_TESTED`。

## 已知的外部阻塞

1. **WorkBuddy**：个人无法自行发放 `client_id` / `client_secret`，需要开放平台的应用审核。
   在拿到凭据之前，LJ Hub 只提供可测试的适配器 + 明确的 `CONFIG_REQUIRED`，
   不提供"看起来已连接"的假象。
2. **Codex**：本机未安装 Codex CLI；LJ Hub 不会代你安装或登录。
3. **Qoder / Qoder CN / TRAE**：本机只有 GUI 应用，官方 CLI 未安装，
   因此无法验证 CLI 的 ACP 子命令是否与文档一致。
