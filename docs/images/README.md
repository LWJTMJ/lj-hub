# 截图状态（诚实说明）

本目录**目前没有应用窗口截图**。原因与处理方式如实记录如下，不使用任何概念图、示意图或
设计稿冒充应用截图。

## 为什么暂时没有

截图需要把 LJ Hub 窗口带到前台才能被 macOS 截取（被完全遮挡的窗口，
`screencapture -l <window-id>` 只会返回空白内容，已实测确认）。
在生成这份文档时，这台机器上正有另一个 AI 客户端在跑长任务，
为避免抢占前台、干扰其工作，本次没有激活 LJ Hub 窗口。

## 如何补上（约 10 秒）

```bash
# 1. 找到 LJ Hub 的窗口 ID（无需辅助功能权限，也不会改变前台窗口）
swift scripts/list-windows.swift "LJ Hub"

# 2. 让 LJ Hub 窗口可见（手动点一下 Dock 图标或切换过去），然后：
screencapture -l <上一步输出的窗口ID> -o -x docs/images/01-main-window.png

# 3. 建议同时补两张：
#    - 设置 → Agent 管理（显示真实状态与"连接测试"结果）
#    - 任务交接对话框（显示五项核验结果）
```

## 已经提供的真实证据（非截图）

README 与 `docs/` 中使用的是可直接复现的命令输出与文件事实，而不是图片：

- 真实 ACP 握手与真实模型回合的结果（`cargo test --test real_agents -- --nocapture`）；
- 本机各 Agent 的检测结论（`report_real_agent_availability`）；
- 构建产物路径、大小与 SHA-256；
- 应用数据目录的真实文件布局。

这些证据任何人都能在自己机器上重跑验证，比一张无法验证的截图更强。
