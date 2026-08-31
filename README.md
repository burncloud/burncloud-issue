# BurnCloud Issue

`burncloud-issue` 是 BurnCloud 在 Architecture 与 Implementation 之间的 **Issue Dependency Tree**。

它的首页不是聊天框，而是一棵持续从 GitHub 重建的工程任务树，用来回答四个问题：

```text
我们要做什么？
现在做到哪里？
下一步应该做什么？
每个 Issue 对应的 Pull Request / CI / Review 情况怎么样？
```

## 核心职责

```text
burncloud-architect
      ↓
Architecture / Milestone
      ↓
burncloud-issue
      ↓
Issue Dependency Tree
      ↓
burncloud-harness
      ↓
Pull Request
      ↓
burncloud-review
      ↓
Evidence / Merge
      ↓
burncloud-issue 自动刷新状态
```

`burncloud-issue` **不负责重新设计架构**。它只负责把已经确定的架构组织成可以独立完成、独立 Review 的 Issue，并持续跟踪完成度。

## 任务状态

状态由 GitHub 事实和依赖自动计算，不由 AI 凭感觉填写：

```text
[可开始]  Issue open，所有依赖已完成，没有正在开发的 PR
[进行中]  存在关联 Draft PR
[审查中]  存在关联 Open PR
[阻塞]    依赖 Issue 尚未完成
[完成]    Issue 已关闭
```

Milestone / Epic 的完成度只统计 `required` Issue。Optional Issue 不会稀释主进度。

## Issue Tree Metadata

新创建的 Issue 会写入一个机器可读块：

```text
<!-- burncloud-issue-tree
parent: 123
depends_on: 120,121
required: true
-->
```

因此程序可以稳定重建：

```text
Milestone
└─ Epic / Issue
   ├─ PR
   └─ Child Issue
      └─ PR
```

历史 Issue 没有关系元数据时不会由 AI 擅自重组；它们会保留在当前 Milestone 根层或“未归类”。

## GitHub / PR 状态

程序同步：

- GitHub Issues
- Milestones
- Issue 父子关系与依赖
- PR 对 Issue 的引用
- Open PR 的 Draft / Open 状态
- GitHub Check Runs（CI）
- PR Review 状态

关联 PR 会直接作为 Issue 的子节点显示。

## Chat 仍然保留，但不是首页

在任务树选中 Milestone 或 Issue 后按 `C` 才进入 Issue 对话模式。

Codex 的职责是把当前节点下面的“剩余工程任务”整理成一个小而清晰的 Issue，而不是重新决定产品架构。

创建流程仍然保持强制人工确认：

```text
C 进入对话
↓
Codex 一问一答
↓
F2 Finalize + Duplicate Check + Quality Gate
↓
READY
↓
F4
↓
最终确认框
↓
Y
↓
GitHub Create Issue
↓
自动刷新任务树
```

**AI 永远没有直接创建 GitHub Issue 的权限。**

## 启动

默认目录结构：

```text
Work/
├── burncloud/
└── burncloud-issue/
```

确认本机 Codex：

```bash
codex --version
```

然后：

```bash
cargo run
```

默认目标仓库：`burncloud/burncloud`

默认只读代码目录：`../burncloud`

也可以指定：

```bash
cargo run -- --repo burncloud/burncloud --local-repo ../burncloud
```

## GitHub 认证

程序依次尝试：

1. `GITHUB_TOKEN`
2. `GH_TOKEN`
3. `gh auth token`

推荐：

```bash
gh auth login
```

## 任务树键盘操作

```text
↑ ↓            选择节点
← →            收起 / 展开 / 进入详情
Enter          展开或收起节点
Tab            Task Tree / Detail 切换
PgUp/PgDn      滚动详情
Home/End       树顶部/底部或详情顶部/底部
C              从当前节点创建子 Issue
R              重新同步 GitHub 状态
Ctrl+Q         退出
```

## Issue 对话键盘操作

```text
Enter          发送
Tab/Shift+Tab  对话 / 输入 / 草稿切换
↑ ↓            滚动
PgUp/PgDn      翻页
F2             最终检查
F4             请求创建（仅 READY）
Y              最终确认创建
N              取消确认
Esc            返回任务树
Ctrl+C         取消 Codex
Ctrl+Q         退出
```

## BurnCloud Issue Standard v1

每个正式 Issue 仍然必须包含：

- 问题
- 当前行为
- 预期行为
- 真实证据
- 根因与置信度
- 影响组件
- 允许修改 / 禁止修改
- 可验证验收标准
- 测试要求
- 依赖
- Risk / Severity / Confidence

原则上：**一个 Issue 对应一个可以独立 Review 的 PR。**
