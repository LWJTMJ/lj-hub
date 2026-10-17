# 阶段交付报告：自动接力（Phase 0–3）

日期：2026-10-17 · 分支：`feat/auto-relay-scheduler` → 已 ff-only 合入 `main`
提交：`9caabfd`（接力核心）、`23fbb38`（审计与设计文档）、`d0de8e5`（能力分级 UI）

本报告严格区分：设计完成 / 代码实现 / 单元测试通过 / 模拟端到端通过 /
真实 Agent 测试通过 / 用户环境实测可用。

---

## 1. 实际实现了什么

**Phase 0（审计与方案）**
- 只读审计 FreeBuddy v0.10.45（commit `5e117a5`），逐条核对指定源码，产出对照报告
  `docs/RELAY_AUDIT_AND_DESIGN.md`，含 `文件:行号` 证据与"仅存在于文档"的清单。
- 结论：FreeBuddy 真实实现了调度选择、阶段门、审核循环、控制标记、有界上下文；
  **没有**结果 Git 核验、没有步骤超时/自动重试、没有 worktree 合并与冲突处理、
  没有 WorkBuddy/TRAE 接入；交接简报的裁剪是静默的，中断步骤是直接重跑。

**Phase 1（自动串行接力）—— 代码实现 + 模拟端到端通过 + 真实通过**
- `src/scheduler.rs`：纯函数状态机（阶段顺序 + 槽位 + 依赖 + 全局单写者 + 循环上限 +
  计划校验 + 上下文组装 + 标记解析）。
- `src/run_driver.rs`：驱动（每次 tick 一个动作、立即落盘、失败学习、人工处置）。
- `src/runs.rs`：运行持久化与重启核对。
- `src/sessions.rs`：与交互式聊天**共用**的会话/回合代码路径。

**Phase 2（自动返工与可靠恢复）—— 代码实现 + 模拟端到端通过**
- 验收 FAIL → 自动返工，返工提示包含验收者给出的具体问题（测试逐字校验）。
- `max_loops` / `max_attempts` / 每步超时 / 运行级 `max_step_executions` 全部有界。
- 中断步骤：不被假设成功、不被盲目重跑，转 `NEEDS_HUMAN` 并附上开始时的 Git 事实。
- 幂等：`attempts` 在回合开始前自增；已完成步骤不会再被选中。

**Phase 3（并行安全）—— 代码实现 + 单元测试通过**
- `src/worktree.rs`：独立 worktree + 真实分支 + 索引映射；脏主工作区拒绝创建；
  合并前干跑（`merge-tree --write-tree`）；真合并失败自动 `merge --abort` 回滚；
  删除时拒绝丢弃未提交成果。

**Phase 4（扩展适配）—— 部分完成**
- 审计得到证据并修正 Qoder 的可执行名（`qodercli` 优先，其次 `qoder`）。
- 新增「真实回合验证」与能力分级：握手 ≠ 真实回合 ≠ 可调度。
- WorkBuddy 仍为 `CONFIG_REQUIRED`（官方要求审核凭据），未伪造。

## 2. 修改/新增文件

新增：`scheduler.rs`(1326)、`run_driver.rs`(1025)、`worktree.rs`(643)、`runs.rs`(302)、
`sessions.rs`(239)、`tests/relay.rs`(690)、`docs/RELAY_AUDIT_AND_DESIGN.md`(149)、
`src/components/RunsDialog.vue`(511)。
修改：`commands.rs`(+414)、`app.rs`(+203)、`model.rs`、`git.rs`（内容指纹）、
`handoff.rs`（指纹检查）、`agents.rs`、`mock_acp_agent.rs`（relay 场景）、
`README.md`、`docs/AGENT_INTEGRATION.md`、`SettingsDialog.vue`、`Sidebar.vue`、
`App.vue`、`api.ts`、`types.ts`、`Cargo.toml`（tempfile 转正式依赖）。

## 3. 采用了哪些 FreeBuddy 设计（自行实现，未复制代码）

阶段顺序与阶段内并行度、全局单写者、依赖门控、
审核循环判定与上限、`REVIEW_STATUS`/`UNRESOLVED` 控制标记、
有界上游上下文（24k/12k/32 块，60% 头 + 40% 尾，可见截断标记）、
摘要截断时保留控制标记、worktree 路径放在应用数据目录并做包含性校验。

## 4. 完全复用 LJ Hub 原有代码的部分

ACP 会话运行时与取消语义、事件流水线、权限与 fs 沙箱、Git 只读白名单、
交接三阶段与处置、产物登记与 SHA-256、本地原子持久化、Tauri 命令面与界面框架。

## 5. 仍未解决

1. 真实两厂商接力未完成：本机只有 OpenCode 具备可用的真实模型凭据；
   DeepSeek Harness 握手正常但真实回合缺 `DEEPSEEK_API_KEY`。
   需要用户提供环境变量（可在设置里填变量名，值不落盘）。
2. 跨运行并行调度尚未在 UI 自动化（机制已就绪）。
3. 验收依赖 Agent 输出规定标记；不遵守则落为"未给出可识别结论"并交人工。
4. WorkBuddy 云端 ACP 通道（SSE + POST）未实现。
5. 计划质量取决于规划 Agent；LJ Hub 只做结构性校验。

## 6. 完成真实测试的 Agent

- **OpenCode**：真实 ACP 握手、真实执行回合、真实验收回合（`REVIEW_STATUS: PASS`）。
- **DeepSeek Harness**：真实 ACP 握手成功；真实回合失败（缺 API key），
  LJ Hub 如实报告并将其标记为不可调度。

## 7. 测试结果

| 套件 | 结果 |
|---|---|
| `cargo test --lib`（含 scheduler 13、worktree 7、git 指纹 3、handoff 指纹 1） | 68 passed / 0 failed |
| `cargo test --test acp_session`（手写线协议 mock agent） | 9 passed / 0 failed |
| `cargo test --test relay`（模拟接力） | 6 passed / 0 failed |
| `cargo test --test real_agents` | 4 passed / 1 ignored |
| `cargo test --test relay -- --ignored`（真实回合，消耗额度） | 2 passed（含 1 次完整真实接力 DONE） |
| `cargo clippy --all-targets -- -D warnings` | 0 error |
| `vue-tsc --noEmit` / `vite build` / `tauri build --bundles app` | 通过（app 20.61 MiB） |

未运行：Windows/Linux 构建（非目标平台）、跨运行并行调度（未实现）。

## 8. Git 提交哈希

`9caabfd` → `23fbb38` → `d0de8e5`（已 ff-only 合入 `main`）

## 9. 下一阶段建议

1. 真实两厂商接力：由用户提供 `DEEPSEEK_API_KEY`（或等价的凭据服务）后复跑
   `real_relay_reports_a_blocked_reviewer_honestly` 与完整接力测试。
2. 跨运行并行：把不同运行分配到不同 worktree，并在 UI 上提供统一的验收/合并入口
   （`worktree::integrate` 已具备干跑、冲突检测与自动回滚）。
3. 运行级成本可视化：把每次真实回合的耗时/用量汇总到运行详情。
4. 计划模板：为常见工作流（实现+验收、调研+实现+验收）提供内置模板，减少规划偏差。
