# 第三方组件与许可

LJ Hub 自身的源代码以 MIT 许可发布（见 `LICENSE`）。
它链接、打包或以源码形式使用的第三方组件如下。本文件不改变任何第三方组件的许可条款。

## 运行时直接依赖（Rust）

| 组件 | 版本 | 许可 | 用途 |
|---|---|---|---|
| `agent-client-protocol` | 3.3.x | Apache-2.0 | 官方 ACP Rust SDK（协议 v1 类型、连接、进程传输） |
| `agent-client-protocol-schema` | 1.11.x | Apache-2.0 | ACP 协议 schema 类型（由 SDK 引入） |
| `tauri` / `tauri-build` / `tauri-plugin-dialog` | 2.x | Apache-2.0 OR MIT | 桌面应用框架与文件夹选择对话框 |
| `tokio` | 1.x | MIT | 异步运行时与进程管理 |
| `serde` / `serde_json` | 1.x | Apache-2.0 OR MIT | 序列化与 JSON-RPC 消息编解码 |
| `reqwest` | 0.12.x | Apache-2.0 OR MIT | WorkBuddy Open API 的 HTTP 客户端 |
| `rustls` / `webpki-roots`（经 reqwest） | — | Apache-2.0 / ISC / MIT | TLS 实现与根证书 |
| `sha2` | 0.10.x | Apache-2.0 OR MIT | 重要产物内容校验 |
| `hex` | 0.4.x | Apache-2.0 OR MIT | 哈希十六进制表示 |
| `uuid` | 1.x | Apache-2.0 OR MIT | 本地记录标识 |
| `chrono` | 0.4.x | Apache-2.0 OR MIT | 时间戳 |
| `tracing` / `tracing-subscriber` | 0.1.x / 0.3.x | MIT | 结构化日志（仅 stderr） |
| `thiserror` / `anyhow` | 1.x / 2.x / 1.x | Apache-2.0 OR MIT | 错误处理 |

## 构建时依赖（前端）

| 组件 | 版本 | 许可 | 用途 |
|---|---|---|---|
| `vue` | 3.5.x | MIT | 界面框架 |
| `vite` / `@vitejs/plugin-vue` | 8.x / 6.x | MIT | 构建工具 |
| `typescript` / `vue-tsc` | 5.9.x / 3.x | Apache-2.0 | 类型检查 |
| `@tauri-apps/api` / `@tauri-apps/cli` / `@tauri-apps/plugin-dialog` | 2.x | Apache-2.0 OR MIT | 前端与 Tauri 核心通信、CLI |

## 被驱动的 Agent（不是本仓库的一部分）

LJ Hub 通过 ACP 启动**你自己安装的** Agent 进程，不打包、不再分发它们的代码，
也不改变它们的许可与登录方式：

- Codex（官方 `agentclientprotocol/codex-acp` 适配器，由 `npx` 按固定版本拉取）
- WorkBuddy（腾讯 WorkBuddy 开放平台 Open API）
- Qoder / Qoder CN（Alibaba）
- TRAE（ByteDance）
- DeepSeek Harness（DeepSeek）
- OpenCode
- CodeBuddy Code（作为独立兼容入口）

各 Agent 的商标、版权与许可归其各自所有者。LJ Hub 与这些厂商**没有**官方合作关系或品牌授权，
项目文档也不会作出此类声明。

## 参考过的开源实现

设计与接入方式参考了 ACP 官方文档、官方 SDK 示例，以及社区客户端（如 `acp-ui`、`paseo`）的公开说明。
**本仓库没有复制这些项目的源代码**；若将来移植任何源码，会在本文件登记来源、上游版本与许可，
并在相关文件保留原始版权声明。
