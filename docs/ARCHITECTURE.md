# 架构说明

## 分层

```
┌──────────────────────────────────────────────┐
│ Vue 3 + TypeScript 界面（src/）               │
│  · 左：项目 / 任务  中：对话  右：Git / 交接  │
└───────────────┬──────────────────────────────┘
                │ Tauri invoke（命令，返回结果）
                │ Tauri event（lj://session-event，单向推送）
┌───────────────▼──────────────────────────────┐
│ Rust 核心（src-tauri/src/）                   │
│  commands.rs  ← 唯一的命令面，全部真实实现     │
│  app.rs       应用状态 / 事件落盘 / UI 通知    │
│  acp.rs       ACP 会话运行时（官方 SDK，v1）   │
│  agents.rs    六种 Agent 的登记与探测          │
│  events.rs    事件流水线（合并 + 顺序 + 解耦） │
│  git.rs       只读 Git 检查                    │
│  handoff.rs   PREPARED → VERIFIED → ACCEPTED  │
│  artifacts.rs 产物登记与 SHA-256 校验          │
│  workbuddy.rs WorkBuddy Open API 适配器        │
│  store.rs     本地 JSON 持久化（原子写入）     │
└──────────────────────────────────────────────┘
```

## 为什么是"一个会话一个线程"

每个活动会话由一个专用 OS 线程持有，线程内跑 current-thread Tokio 运行时，
外部只能通过命令通道与它交互：

```
UI ──prompt──▶ mpsc<Command> ──▶ 会话线程（Tokio current-thread）
                ◀── oneshot<PromptOutcome>（回合结果）
                ◀── EventPipeline（流式事件，异步推给 UI）
```

这样做的好处：

1. **不污染公共类型**：ACP 的 handler 不需要 `Send`，也就不必为了满足 Tauri 的
   运行时而把 UI 类型（`AppHandle`）渗进协议层。
2. **卡死的 Agent 不影响界面**：会话线程阻塞时 UI 照常响应，可以取消或直接停会话。
3. **取消真的可用**：回合在子任务里驱动，命令循环随时能发出 `session/cancel`。

## 事件流水线（events.rs）

ACP 的回调运行在协议派发循环里，慢操作会拖住整个往返。因此：

- 回调只把事件塞进无锁信道，立刻返回；
- 独立线程做**文本合并**（默认 120 ms / 8 KB 触发一次）； 
- 结构性事件（工具调用、权限请求、结束、错误）到达时先冲刷已累积文本，
  保证顺序不乱；
- 落盘与 UI 推送都在这个线程完成。

会话结束时流水线被显式关闭，保证最后一段文本一定落盘。

## 持久化（store.rs）

```
<app data>/state.json          项目、任务索引、设置
<app data>/tasks/<task>.json   单个任务的完整消息与交接资料
```

- 原子写入（临时文件 + `rename`），进程被杀不会留下半个 JSON；
- 每个任务独立文件，长对话不会反复重写索引；
- 启动时用磁盘上的任务文件重建索引，界面显示的列表与磁盘事实不会脱节；
- 索引文件损坏或缺失时以空状态启动，而不是拒绝启动。

## 事件如何变成会话历史（app.rs::apply_event）

| 事件 | 处理 |
|---|---|
| 流式文本 | 合并进同一条消息（按 task + stream 跟踪），不产生碎片 |
| 工具调用 | 按 `tool_call_id` 就地更新同一条记录，UI 不重复出现卡片 |
| 计划 / 模式 | 追加独立记录 |
| 权限请求 | 追加 `pending` 记录；用户决定后就地改成结果 |
| 原生会话 | 记录 native session id 与**是否真的恢复**，并写一条系统消息 |
| 结束 / 错误 | 关闭流并把错误落盘 |

同一条规则贯穿：**界面显示的内容一定先落盘**，两者不会各说一套。

## Agent 适配层

统一接口（见 `agents.rs` 的 `AgentDefinition` 与 `commands.rs::LaunchSpec`）：

- 可执行文件解析顺序：用户自定义路径 → `PATH` → 已知绝对路径；
- 版本探测：真的执行 `--version`；
- 能力确认：真的跑 ACP `initialize`，从 `agentCapabilities` / `authMethods` 取事实；
- 启动参数可按 Agent 覆盖（例如 TRAE 是 `traecli acp serve`）；
- WorkBuddy 不是本地 stdio Agent，走独立 HTTP 适配器，不强行套用同一传输。

新增一个 Agent 只需要在 `AGENTS` 表里加一条，不需要改会话运行核心。

## 已知限制

- 只实现 ACP v1；v2 仍是草案。
- 未向 Agent 声明 `terminal` 能力，因此不提供客户端终端。
- WorkBuddy 云端任务的 ACP 通道（SSE + POST）未实现。
- 会话结束时不自动重启 Agent，需要显式"继续"。
- 单任务串行接手是默认路径；并行 worktree 未实现（保留扩展空间）。
