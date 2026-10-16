# 截图

本目录中的图片是**真实运行中的 LJ Hub 窗口截图**，不是概念图或设计稿。

| 文件 | 内容 |
|---|---|
| `01-main-window.png` | 主窗口：左侧真实项目/任务列表，中间是真实的 OpenCode 会话记录（真实工具调用 `glob` / `read` 与真实回答），右侧是真实的 Git 路径、原生会话 id 与"原生会话未恢复，仍保留可阅读的历史"提示 |
| `02-agent-management.png` | 设置 → Agent 管理：各组件的真实检测状态（Codex `NOT_INSTALLED`、WorkBuddy `CONFIG_REQUIRED`、Qoder / Qoder CN `NOT_INSTALLED`）与"尚未进行 ACP 握手测试，不能视为可用"的说明 |

## 截图是怎么来的（可复现）

被完全遮挡的窗口在 macOS 上截不出内容（`screencapture -l` 会返回空白），
所以 LJ Hub 提供了一个**仅用于文档取证**的启动参数，不会改变你的前台应用焦点：

```bash
# 1) 让窗口浮到最上层（不激活应用，不抢键盘焦点）
LJ_HUB_SCREENSHOT_MODE=topmost \
  "./src-tauri/target/release/bundle/macos/LJ Hub.app/Contents/MacOS/lj-hub" &

# 2) 找到窗口 ID（CoreGraphics，无需辅助功能权限）
swift scripts/list-windows.swift "LJ Hub"

# 3) 截取该窗口
screencapture -l <窗口ID> -o -x docs/images/01-main-window.png

# 需要对话框截图时，让界面直接打开对应视图：
LJ_HUB_SCREENSHOT_MODE=topmost LJ_HUB_SCREENSHOT_OPEN=settings \
  "./src-tauri/target/release/bundle/macos/LJ Hub.app/Contents/MacOS/lj-hub" &
```

这两个环境变量在未设置时**完全不生效**；它们只用于文档取证，不参与任何业务逻辑。

## 数据来源

截图里的会话内容来自一次真实回合：LJ Hub 通过 ACP 驱动本机已安装的 OpenCode，
在 `/Users/jayli/AI/lj-hub` 项目里问了"请用一句话说明这个项目是做什么的"，
OpenCode 读取了 `README.md`、`package.json` 等文件后给出了回答，全部由 LJ Hub 自己落盘。
没有任何演示数据、假会话或占位文本。
