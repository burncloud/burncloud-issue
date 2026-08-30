# BurnCloud Issue

`burncloud-issue` 是 BurnCloud 的对话式 Issue Factory。它使用本地 Codex CLI 通过一问一答把模糊问题整理成符合 **BurnCloud Issue Standard v1** 的 GitHub Issue。

## 设计原则

- AI 每轮最多追问一个最关键的问题。
- Codex 以 `read-only` sandbox 运行，可以读取本地 `../burncloud` 来核验证据，但不能修改代码。
- Issue 必须包含问题、当前行为、预期行为、证据、根因、影响范围、修改边界、验收标准和测试要求。
- 最终创建前自动执行 GitHub Issue 去重和 Issue Quality Gate。
- 只有 `READY` 才允许创建。
- **AI 永远不能自行创建 Issue。** 使用者必须在最终确认框中明确按 `Y` 同意。

## 启动

默认目录结构：

```text
Work/
├── burncloud/
└── burncloud-issue/
```

确认本机 Codex 已登录：

```bash
codex --version
```

然后：

```bash
cargo run
```

默认目标仓库为 `burncloud/burncloud`，本地只读代码目录为 `../burncloud`。

也可以指定：

```bash
cargo run -- --repo burncloud/burncloud --local-repo ../burncloud
```

如需覆盖 Codex 模型：

```bash
cargo run -- --codex-model <model>
```

## GitHub 写入认证

搜索公开 Issue 不要求 Token；真正创建 Issue 时需要 GitHub 写权限。程序依次尝试：

1. `GITHUB_TOKEN`
2. `GH_TOKEN`
3. `gh auth token`

推荐先运行：

```bash
gh auth login
```

## 键盘操作

```text
Enter          发送当前输入
Tab/Shift+Tab  在输入、对话、Issue 草稿之间切换焦点
↑ ↓            滚动当前阅读区域
← →            输入框移动光标；阅读区左右切换
PgUp/PgDn      大段翻页
Home/End       到顶部/底部；输入框到行首/行尾
F2             最终生成 + GitHub 去重 + Issue Quality Gate
F4             请求创建 Issue（仅 READY 可用）
Y              在最终确认框中明确同意创建
N / Esc        取消创建，返回继续修改
Ctrl+C         取消当前 Codex 任务
Ctrl+Q         退出
```

## 标准流程

```text
用户描述问题
    ↓
Codex 一问一答澄清
    ↓
形成 Issue Draft
    ↓
F2
    ↓
Finalize Draft
    ↓
Search existing GitHub Issues
    ↓
Issue Quality Gate
    ├─ READY
    ├─ NEEDS_EVIDENCE
    ├─ DUPLICATE
    ├─ NEEDS_SPLIT
    ├─ BLOCKED
    └─ REJECTED
    ↓
READY + F4
    ↓
确认框
    ↓
用户按 Y
    ↓
Create GitHub Issue
```

任何新的对话都会让之前的最终 `READY` 失效，必须重新执行 `F2`。因此旧的质量判断不能被拿来提交已经修改过的 Issue。
