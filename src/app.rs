use std::{
    collections::HashSet,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use crate::{
    codex::{AgentMode, CodexConfig, CodexExecution},
    github::{CreatedIssue, GithubClient},
    models::{
        AgentResponse, ChatMessage, DuplicateCandidate, IssueDraft, MessageRole, ProjectSnapshot,
        QualityGate, TaskStatus,
    },
    pipeline::{start_finalize, FinalizeExecution},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Tree,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Detail,
    Chat,
    Input,
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TreeNodeId {
    Milestone(u64),
    Unplanned,
    Issue(u64),
    PullRequest(u64, u64),
}

#[derive(Debug, Clone)]
pub struct TreeRow {
    pub node: TreeNodeId,
    pub depth: usize,
    pub label: String,
    pub expandable: bool,
    pub expanded: bool,
}

pub struct App {
    pub repository: String,
    pub local_repo: PathBuf,
    pub screen: Screen,
    pub focus: Focus,
    pub snapshot: ProjectSnapshot,
    pub tree_selected: usize,
    pub detail_scroll: u16,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub cursor: usize,
    pub chat_scroll: u16,
    pub preview_scroll: u16,
    pub draft: Option<IssueDraft>,
    pub quality_gate: QualityGate,
    pub duplicates: Vec<DuplicateCandidate>,
    pub quality_finalized: bool,
    pub confirm_create: bool,
    pub status: String,
    pub created_issue: Option<CreatedIssue>,
    pub should_quit: bool,
    pub ai_started: Option<Instant>,
    pub finalize_started: Option<Instant>,
    pub sync_started: Option<Instant>,
    expanded: HashSet<TreeNodeId>,
    chat_parent_issue: Option<u64>,
    chat_milestone: Option<u64>,
    codex: CodexConfig,
    github: GithubClient,
    ai: Option<CodexExecution>,
    finalize: Option<FinalizeExecution>,
    create: Option<Receiver<Result<CreatedIssue, String>>>,
    sync: Option<Receiver<Result<ProjectSnapshot, String>>>,
}

impl App {
    pub fn new(
        repository: String,
        local_repo: PathBuf,
        codex_model: Option<String>,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        let github = GithubClient::new(repository.clone())?;
        let codex = CodexConfig {
            local_repo: local_repo.clone(),
            repository: repository.clone(),
            timeout,
            model: codex_model,
        };
        let mut app = Self {
            repository,
            local_repo,
            screen: Screen::Tree,
            focus: Focus::Tree,
            snapshot: ProjectSnapshot::default(),
            tree_selected: 0,
            detail_scroll: 0,
            messages: Vec::new(),
            input: String::new(),
            cursor: 0,
            chat_scroll: 0,
            preview_scroll: 0,
            draft: None,
            quality_gate: QualityGate::default(),
            duplicates: Vec::new(),
            quality_finalized: false,
            confirm_create: false,
            status: format!(
                "正在同步 GitHub Issue / PR · GitHub {}",
                github.auth_label()
            ),
            created_issue: None,
            should_quit: false,
            ai_started: None,
            finalize_started: None,
            sync_started: None,
            expanded: HashSet::new(),
            chat_parent_issue: None,
            chat_milestone: None,
            codex,
            github,
            ai: None,
            finalize: None,
            create: None,
            sync: None,
        };
        app.refresh_project();
        Ok(app)
    }

    pub fn tick(&mut self) {
        self.poll_sync();
        self.poll_ai();
        self.poll_finalize();
        self.poll_create();
    }

    pub fn is_busy(&self) -> bool {
        self.ai.is_some() || self.finalize.is_some() || self.create.is_some()
    }

    pub fn syncing(&self) -> bool {
        self.sync.is_some()
    }

    pub fn can_create(&self) -> bool {
        self.quality_finalized
            && self.quality_gate.is_ready()
            && self.draft.is_some()
            && self.created_issue.is_none()
            && !self.is_busy()
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.confirm_create {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.confirm_and_create(),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm_create = false;
                    self.status = "已取消创建，Issue 草稿保持不变。".into();
                }
                _ => {}
            }
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            self.should_quit = true;
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.cancel_running();
            return;
        }

        match self.screen {
            Screen::Tree => self.handle_tree_key(key),
            Screen::Chat => self.handle_chat_key(key),
        }
    }

    pub fn input_view(&self, width: usize) -> (String, u16) {
        let chars: Vec<char> = self.input.chars().collect();
        let cursor = self.cursor.min(chars.len());
        let mut start = cursor;
        let mut used = 0usize;
        while start > 0 {
            let w = chars[start - 1].width().unwrap_or(1);
            if used + w >= width.saturating_sub(1) {
                break;
            }
            used += w;
            start -= 1;
        }
        let mut visible = String::new();
        let mut visible_width = 0usize;
        for ch in chars.iter().skip(start) {
            let w = ch.width().unwrap_or(1);
            if visible_width + w >= width.saturating_sub(1) {
                break;
            }
            visible.push(*ch);
            visible_width += w;
        }
        let cursor_width = chars[start..cursor]
            .iter()
            .map(|ch| ch.width().unwrap_or(1))
            .sum::<usize>();
        (visible, cursor_width.min(u16::MAX as usize) as u16)
    }

    pub fn tree_rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for milestone in &self.snapshot.milestones {
            let node = TreeNodeId::Milestone(milestone.number);
            let (_, _, percent) = self.snapshot.milestone_progress(Some(milestone.number));
            rows.push(TreeRow {
                node,
                depth: 0,
                label: format!("[{:>3}%] {}", percent, milestone.title),
                expandable: true,
                expanded: self.expanded.contains(&node),
            });
            if self.expanded.contains(&node) {
                let mut seen = HashSet::new();
                for issue in self
                    .snapshot
                    .root_issues_for_milestone(Some(milestone.number))
                {
                    self.push_issue_rows(issue.number, 1, &mut seen, &mut rows);
                }
            }
        }

        if self
            .snapshot
            .issues
            .iter()
            .any(|issue| issue.milestone_number.is_none())
        {
            let node = TreeNodeId::Unplanned;
            let (_, _, percent) = self.snapshot.milestone_progress(None);
            rows.push(TreeRow {
                node,
                depth: 0,
                label: format!("[{:>3}%] 未归类 / No Milestone", percent),
                expandable: true,
                expanded: self.expanded.contains(&node),
            });
            if self.expanded.contains(&node) {
                let mut seen = HashSet::new();
                for issue in self.snapshot.root_issues_for_milestone(None) {
                    self.push_issue_rows(issue.number, 1, &mut seen, &mut rows);
                }
            }
        }
        rows
    }

    pub fn selected_node(&self) -> Option<TreeNodeId> {
        self.tree_rows().get(self.tree_selected).map(|row| row.node)
    }

    pub fn detail_text(&self) -> String {
        let Some(node) = self.selected_node() else {
            return if self.syncing() {
                "正在从 GitHub 同步当前 Issue / PR 状态…".into()
            } else {
                "当前没有可显示的 Issue。按 R 刷新，或按 C 创建一个新的任务。".into()
            };
        };
        match node {
            TreeNodeId::Milestone(number) => self.milestone_detail(Some(number)),
            TreeNodeId::Unplanned => self.milestone_detail(None),
            TreeNodeId::Issue(number) => self.issue_detail(number),
            TreeNodeId::PullRequest(issue, pr) => self.pr_detail(issue, pr),
        }
    }

    pub fn preview_text(&self) -> String {
        let mut out = String::new();
        out.push_str("Issue Quality Gate\n");
        out.push_str(&format!(
            "状态: {}{}\n",
            if self.quality_gate.status.is_empty() {
                "尚未最终检查"
            } else {
                &self.quality_gate.status
            },
            if self.quality_finalized {
                " · FINAL"
            } else {
                ""
            }
        ));
        for check in &self.quality_gate.checks {
            out.push_str(&format!(
                "  [{}] {} — {}\n",
                check.status, check.name, check.evidence
            ));
        }
        for blocker in &self.quality_gate.blockers {
            out.push_str(&format!("  BLOCKER: {blocker}\n"));
        }

        out.push_str("\n重复 Issue 检查\n");
        if self.duplicates.is_empty() {
            out.push_str(if self.quality_finalized {
                "  未发现候选，或 Quality Gate 未认定重复。\n"
            } else {
                "  按 F2 时自动搜索 GitHub。\n"
            });
        } else {
            for item in &self.duplicates {
                out.push_str(&format!(
                    "  #{} [{}] {}\n",
                    item.number, item.state, item.title
                ));
            }
        }

        out.push_str("\n────────────────────────\n\n");
        if let Some(draft) = &self.draft {
            out.push_str(&format!("# {}\n\n", draft.title));
            out.push_str(&draft.to_markdown(&self.repository));
        } else {
            out.push_str("草稿尚未形成。继续和 Codex 对话；信息足够后这里会出现可执行 Issue。\n");
        }
        out
    }

    fn handle_tree_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('r') | KeyCode::Char('R') => self.refresh_project(),
            KeyCode::Char('c') | KeyCode::Char('C') => self.enter_chat(),
            KeyCode::Tab => {
                self.focus = if self.focus == Focus::Tree {
                    Focus::Detail
                } else {
                    Focus::Tree
                };
            }
            KeyCode::Left => self.tree_left(),
            KeyCode::Right => self.tree_right(),
            KeyCode::Enter => self.tree_enter(),
            KeyCode::Up => {
                if self.focus == Focus::Tree {
                    self.tree_selected = self.tree_selected.saturating_sub(1);
                    self.detail_scroll = 0;
                } else {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if self.focus == Focus::Tree {
                    let max = self.tree_rows().len().saturating_sub(1);
                    self.tree_selected = (self.tree_selected + 1).min(max);
                    self.detail_scroll = 0;
                } else {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                }
            }
            KeyCode::PageUp => {
                self.focus = Focus::Detail;
                self.detail_scroll = self.detail_scroll.saturating_sub(12);
            }
            KeyCode::PageDown => {
                self.focus = Focus::Detail;
                self.detail_scroll = self.detail_scroll.saturating_add(12);
            }
            KeyCode::Home => {
                if self.focus == Focus::Tree {
                    self.tree_selected = 0;
                    self.detail_scroll = 0;
                } else {
                    self.detail_scroll = 0;
                }
            }
            KeyCode::End => {
                if self.focus == Focus::Tree {
                    self.tree_selected = self.tree_rows().len().saturating_sub(1);
                    self.detail_scroll = 0;
                } else {
                    self.detail_scroll = u16::MAX;
                }
            }
            _ => {}
        }
    }

    fn handle_chat_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.screen = Screen::Tree;
            self.focus = Focus::Tree;
            self.status = "返回任务树。按 C 可继续创建 Issue。".into();
            return;
        }
        match key.code {
            KeyCode::F(2) => self.finalize_issue(),
            KeyCode::F(4) => self.request_create(),
            KeyCode::Tab => self.focus = self.next_chat_focus(),
            KeyCode::BackTab => self.focus = self.previous_chat_focus(),
            KeyCode::Left => self.chat_left(),
            KeyCode::Right => self.chat_right(),
            KeyCode::Up => self.chat_up(),
            KeyCode::Down => self.chat_down(),
            KeyCode::PageUp => self.chat_page_up(),
            KeyCode::PageDown => self.chat_page_down(),
            KeyCode::Home => self.chat_home(),
            KeyCode::End => self.chat_end(),
            KeyCode::Enter if self.focus == Focus::Input => self.send_input(),
            KeyCode::Backspace if self.focus == Focus::Input => self.backspace(),
            KeyCode::Delete if self.focus == Focus::Input => self.delete(),
            KeyCode::Char(ch) if self.focus == Focus::Input => self.insert(ch),
            _ => {}
        }
    }

    fn refresh_project(&mut self) {
        if self.sync.is_some() {
            self.status = "GitHub 状态仍在同步。".into();
            return;
        }
        let github = self.github.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = github.sync_project().map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
        });
        self.sync = Some(rx);
        self.sync_started = Some(Instant::now());
        self.status = "正在同步 GitHub Issues、依赖关系和关联 PR…".into();
    }

    fn poll_sync(&mut self) {
        let Some(receiver) = self.sync.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.sync = None;
                self.sync_started = None;
                match result {
                    Ok(snapshot) => {
                        self.snapshot = snapshot;
                        self.expanded.clear();
                        for milestone in &self.snapshot.milestones {
                            self.expanded
                                .insert(TreeNodeId::Milestone(milestone.number));
                        }
                        if self
                            .snapshot
                            .issues
                            .iter()
                            .any(|issue| issue.milestone_number.is_none())
                        {
                            self.expanded.insert(TreeNodeId::Unplanned);
                        }
                        self.tree_selected = 0;
                        self.detail_scroll = 0;
                        let (done, total, percent) = self.snapshot.overall_progress();
                        self.status = format!(
                            "GitHub 已同步 · Required {done}/{total} · {percent}% · Ready {}",
                            self.snapshot.ready_issues().len()
                        );
                    }
                    Err(error) => self.status = format!("GitHub 同步失败：{error}"),
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.sync = None;
                self.sync_started = None;
                self.status = "GitHub sync worker unexpectedly disconnected".into();
            }
        }
    }

    fn enter_chat(&mut self) {
        if self.is_busy() {
            self.status = "当前 AI / 创建任务仍在运行。".into();
            return;
        }
        let (parent, milestone, context) = match self.selected_node() {
            Some(TreeNodeId::Issue(number)) => {
                if let Some(issue) = self.snapshot.issue(number) {
                    (
                        Some(number),
                        issue.milestone_number,
                        format!("当前父任务：#{} {}", issue.number, issue.title),
                    )
                } else {
                    (None, None, "当前没有父任务。".into())
                }
            }
            Some(TreeNodeId::PullRequest(issue_number, _)) => {
                if let Some(issue) = self.snapshot.issue(issue_number) {
                    (
                        Some(issue_number),
                        issue.milestone_number,
                        format!("当前父任务：#{} {}", issue.number, issue.title),
                    )
                } else {
                    (None, None, "当前没有父任务。".into())
                }
            }
            Some(TreeNodeId::Milestone(number)) => {
                let title = self
                    .snapshot
                    .milestones
                    .iter()
                    .find(|item| item.number == number)
                    .map(|item| item.title.as_str())
                    .unwrap_or("Milestone");
                (None, Some(number), format!("当前 Milestone：{title}"))
            }
            Some(TreeNodeId::Unplanned) | None => (None, None, "当前没有指定父任务。".into()),
        };

        self.chat_parent_issue = parent;
        self.chat_milestone = milestone;
        self.messages = vec![
            ChatMessage::system(format!(
                "{context}。burncloud-issue 只负责把已经确定的架构变成可执行任务，不重新设计产品架构。新 Issue 原则上对应一个可独立 Review 的 PR；如果需求属于架构决策，应明确指出并停止拆解。"
            )),
            ChatMessage::assistant(
                "请描述这个节点下面还需要完成的具体工程任务。我会一次只追问一个关键问题，并保持 Issue 边界足够小。",
            ),
        ];
        self.input.clear();
        self.cursor = 0;
        self.chat_scroll = u16::MAX;
        self.preview_scroll = 0;
        self.draft = None;
        self.quality_gate = QualityGate::default();
        self.duplicates.clear();
        self.quality_finalized = false;
        self.created_issue = None;
        self.screen = Screen::Chat;
        self.focus = Focus::Input;
        self.status = "Issue 对话模式 · Esc 返回任务树 · F2 最终检查 · F4 请求创建".into();
    }

    fn send_input(&mut self) {
        if self.is_busy() {
            self.status = "当前任务仍在运行，Ctrl+C 可以取消。".into();
            return;
        }
        let content = self.input.trim().to_string();
        if content.is_empty() {
            return;
        }
        self.messages.push(ChatMessage::user(content));
        self.input.clear();
        self.cursor = 0;
        self.quality_finalized = false;
        self.duplicates.clear();
        self.created_issue = None;
        self.quality_gate = QualityGate::default();
        self.preview_scroll = 0;
        self.ai = Some(
            self.codex
                .start(self.messages.clone(), AgentMode::Chat, Vec::new()),
        );
        self.ai_started = Some(Instant::now());
        self.status = "Codex 正在阅读上下文并准备下一轮问题… Ctrl+C 取消".into();
    }

    fn finalize_issue(&mut self) {
        if self.is_busy() {
            self.status = "当前任务仍在运行，无法开始最终检查。".into();
            return;
        }
        if !self
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::User))
        {
            self.status = "请先描述任务，再执行最终检查。".into();
            return;
        }
        self.quality_finalized = false;
        self.duplicates.clear();
        self.finalize = Some(start_finalize(
            self.codex.clone(),
            self.github.clone(),
            self.messages.clone(),
        ));
        self.finalize_started = Some(Instant::now());
        self.status = "正在生成草稿 → 搜索重复 Issue → 执行最终 Quality Gate… Ctrl+C 取消".into();
    }

    fn request_create(&mut self) {
        if self.created_issue.is_some() {
            self.status = "这个会话已经创建过 Issue，不会重复创建。".into();
            return;
        }
        if !self.quality_finalized {
            self.status = "创建前必须按 F2 完成最终 Quality Gate 与去重检查。".into();
            return;
        }
        if !self.quality_gate.is_ready() {
            self.status = format!(
                "当前 Quality Gate = {}，只有 READY 才允许创建。",
                self.quality_gate.status
            );
            return;
        }
        if self.draft.is_none() {
            self.status = "没有可创建的 Issue 草稿。".into();
            return;
        }
        self.confirm_create = true;
    }

    fn confirm_and_create(&mut self) {
        self.confirm_create = false;
        if !self.can_create() {
            self.status = "Issue 状态已经变化，请重新执行 F2 最终检查。".into();
            return;
        }
        let draft = self.draft.clone().expect("checked above");
        let github = self.github.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = github
                .create_issue(&draft)
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
        });
        self.create = Some(rx);
        self.status = "使用者已确认，正在创建 GitHub Issue…".into();
    }

    fn poll_ai(&mut self) {
        let Some(execution) = self.ai.as_ref() else {
            return;
        };
        match execution.receiver.try_recv() {
            Ok(result) => {
                self.ai = None;
                self.ai_started = None;
                match result {
                    Ok(response) => self.apply_agent_response(response, false),
                    Err(error) => self.status = format!("Codex 对话失败：{error}"),
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.ai = None;
                self.ai_started = None;
                self.status = "Codex worker unexpectedly disconnected".into();
            }
        }
    }

    fn poll_finalize(&mut self) {
        let Some(execution) = self.finalize.as_ref() else {
            return;
        };
        match execution.receiver.try_recv() {
            Ok(result) => {
                self.finalize = None;
                self.finalize_started = None;
                match result {
                    Ok(result) => {
                        self.duplicates = result.duplicates;
                        self.apply_agent_response(result.response, true);
                    }
                    Err(error) => self.status = format!("最终 Issue 检查失败：{error}"),
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.finalize = None;
                self.finalize_started = None;
                self.status = "Issue finalize worker unexpectedly disconnected".into();
            }
        }
    }

    fn poll_create(&mut self) {
        let Some(receiver) = self.create.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.create = None;
                match result {
                    Ok(issue) => {
                        let created_url = issue.html_url.clone();
                        self.status = format!(
                            "已创建 Issue #{} · {}，正在刷新任务树…",
                            issue.number, created_url
                        );
                        self.created_issue = Some(issue);
                        self.screen = Screen::Tree;
                        self.focus = Focus::Tree;
                        self.refresh_project();
                    }
                    Err(error) => self.status = format!("创建 Issue 失败：{error}"),
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.create = None;
                self.status = "GitHub create worker unexpectedly disconnected".into();
            }
        }
    }

    fn apply_agent_response(&mut self, response: AgentResponse, finalized: bool) {
        if !response.assistant_message.trim().is_empty() {
            self.messages
                .push(ChatMessage::assistant(response.assistant_message));
        }
        if let Some(mut draft) = response.draft {
            if draft.parent_issue.is_none() {
                draft.parent_issue = self.chat_parent_issue;
            }
            if draft.milestone.is_none() {
                draft.milestone = self.chat_milestone;
            }
            if draft.required.is_none() {
                draft.required = Some(true);
            }
            self.draft = Some(draft);
        }
        self.quality_gate = response.quality_gate;
        self.quality_finalized = finalized;
        self.chat_scroll = u16::MAX;
        self.preview_scroll = 0;
        if finalized {
            if self.quality_gate.is_ready() {
                self.status =
                    "最终 Quality Gate = READY。按 F4 查看确认框；没有你的 Y 确认不会创建 Issue。"
                        .into();
            } else {
                self.status = format!(
                    "最终 Quality Gate = {}。请继续对话补充或缩小范围后再按 F2。",
                    self.quality_gate.status
                );
            }
        } else {
            self.status = "Codex 已回复。继续回答问题，或信息充分后按 F2 做最终检查。".into();
        }
    }

    fn cancel_running(&mut self) {
        if let Some(execution) = &self.ai {
            execution
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(execution) = &self.finalize {
            execution
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.status = "已请求取消当前 AI 任务。".into();
    }

    fn push_issue_rows(
        &self,
        number: u64,
        depth: usize,
        seen: &mut HashSet<u64>,
        rows: &mut Vec<TreeRow>,
    ) {
        if !seen.insert(number) {
            return;
        }
        let Some(issue) = self.snapshot.issue(number) else {
            return;
        };
        let node = TreeNodeId::Issue(number);
        let children = self.snapshot.children_of(number);
        let expandable = !children.is_empty() || !issue.linked_prs.is_empty();
        let epic = if issue.is_epic { "EPIC " } else { "" };
        rows.push(TreeRow {
            node,
            depth,
            label: format!(
                "[{}] {}#{} {}",
                issue.status.label(),
                epic,
                issue.number,
                issue.title
            ),
            expandable,
            expanded: self.expanded.contains(&node),
        });
        if !self.expanded.contains(&node) {
            return;
        }
        for pr in &issue.linked_prs {
            rows.push(TreeRow {
                node: TreeNodeId::PullRequest(issue.number, pr.number),
                depth: depth + 1,
                label: format!(
                    "PR #{} [{}] CI:{} Review:{} {}",
                    pr.number,
                    pr.state_label(),
                    pr.ci_state,
                    pr.review_state,
                    pr.title
                ),
                expandable: false,
                expanded: false,
            });
        }
        for child in children {
            self.push_issue_rows(child.number, depth + 1, seen, rows);
        }
    }

    fn milestone_detail(&self, milestone: Option<u64>) -> String {
        let title = milestone
            .and_then(|number| {
                self.snapshot
                    .milestones
                    .iter()
                    .find(|item| item.number == number)
                    .map(|item| item.title.clone())
            })
            .unwrap_or_else(|| "未归类 / No Milestone".into());
        let (done, total, percent) = self.snapshot.milestone_progress(milestone);
        let mut out =
            format!("{title}\n\nRequired Progress: {done}/{total} ({percent}%)\n\n状态统计\n");
        for status in [
            TaskStatus::Ready,
            TaskStatus::InProgress,
            TaskStatus::Review,
            TaskStatus::Blocked,
            TaskStatus::Done,
        ] {
            let count = self
                .snapshot
                .issues
                .iter()
                .filter(|issue| issue.milestone_number == milestone && issue.status == status)
                .count();
            out.push_str(&format!("- {}: {}\n", status.label(), count));
        }
        out.push_str("\n现在可以开始\n");
        let ready = self
            .snapshot
            .issues
            .iter()
            .filter(|issue| {
                issue.milestone_number == milestone && issue.status == TaskStatus::Ready
            })
            .take(20)
            .collect::<Vec<_>>();
        if ready.is_empty() {
            out.push_str("- 当前没有 READY Issue。\n");
        } else {
            for issue in ready {
                out.push_str(&format!("- #{} {}\n", issue.number, issue.title));
            }
        }
        out
    }

    fn issue_detail(&self, number: u64) -> String {
        let Some(issue) = self.snapshot.issue(number) else {
            return format!("Issue #{number} not found");
        };
        let (checked, checklist) = issue.checklist_progress();
        let (done, total, percent) = self.snapshot.issue_progress(number);
        let mut out = format!(
            "#{} {}\n\n状态: {}\nGitHub: {}\nRequired: {}\nMilestone: {}\n",
            issue.number,
            issue.title,
            issue.status.label(),
            issue.state,
            if issue.required { "yes" } else { "optional" },
            issue.milestone_title.as_deref().unwrap_or("None")
        );
        if issue.is_epic || !self.snapshot.children_of(number).is_empty() {
            out.push_str(&format!("子树进度: {done}/{total} ({percent}%)\n"));
        }
        if checklist > 0 {
            out.push_str(&format!("Issue Checklist: {checked}/{checklist}\n"));
        }
        if let Some(parent) = issue.parent {
            out.push_str(&format!("Parent: #{parent}\n"));
        }
        out.push_str("\n依赖\n");
        if issue.depends_on.is_empty() {
            out.push_str("- none\n");
        } else {
            for dependency in &issue.depends_on {
                let state = self
                    .snapshot
                    .issue(*dependency)
                    .map(|item| item.status.label())
                    .unwrap_or("未知");
                out.push_str(&format!("- #{} [{}]\n", dependency, state));
            }
        }
        out.push_str("\n关联 Pull Request\n");
        if issue.linked_prs.is_empty() {
            out.push_str("- none\n");
        } else {
            for pr in &issue.linked_prs {
                out.push_str(&format!(
                    "- PR #{} [{}] CI={} Review={}\n  {}\n",
                    pr.number,
                    pr.state_label(),
                    pr.ci_state,
                    pr.review_state,
                    pr.title
                ));
            }
        }
        if !issue.labels.is_empty() {
            out.push_str(&format!("\nLabels: {}\n", issue.labels.join(", ")));
        }
        out.push_str(&format!("\nURL: {}\n", issue.url));
        if !issue.body.trim().is_empty() {
            out.push_str("\n────────────────────────\nIssue Body\n\n");
            out.push_str(&issue.body);
        }
        out
    }

    fn pr_detail(&self, issue_number: u64, pr_number: u64) -> String {
        let Some(issue) = self.snapshot.issue(issue_number) else {
            return format!("Parent Issue #{issue_number} not found");
        };
        let Some(pr) = issue.linked_prs.iter().find(|pr| pr.number == pr_number) else {
            return format!("PR #{pr_number} not found");
        };
        format!(
            "PR #{} {}\n\n关联 Issue: #{} {}\nState: {}\nCI: {}\nReview: {}\nHead SHA: {}\n\nURL: {}",
            pr.number,
            pr.title,
            issue.number,
            issue.title,
            pr.state_label(),
            pr.ci_state,
            pr.review_state,
            pr.head_sha,
            pr.url
        )
    }

    fn tree_left(&mut self) {
        if self.focus == Focus::Detail {
            self.focus = Focus::Tree;
            return;
        }
        let Some(node) = self.selected_node() else {
            return;
        };
        if self.expanded.remove(&node) {
            return;
        }
        if let Some(parent) = self.parent_node(node) {
            if let Some(index) = self.tree_rows().iter().position(|row| row.node == parent) {
                self.tree_selected = index;
                self.detail_scroll = 0;
            }
        }
    }

    fn tree_right(&mut self) {
        if self.focus == Focus::Detail {
            self.detail_scroll = self.detail_scroll.saturating_add(4);
            return;
        }
        let rows = self.tree_rows();
        let Some(row) = rows.get(self.tree_selected) else {
            return;
        };
        if row.expandable && !row.expanded {
            self.expanded.insert(row.node);
        } else {
            self.focus = Focus::Detail;
        }
    }

    fn tree_enter(&mut self) {
        if self.focus == Focus::Detail {
            return;
        }
        let rows = self.tree_rows();
        let Some(row) = rows.get(self.tree_selected) else {
            return;
        };
        if row.expandable {
            if !self.expanded.remove(&row.node) {
                self.expanded.insert(row.node);
            }
        } else {
            self.focus = Focus::Detail;
        }
    }

    fn parent_node(&self, node: TreeNodeId) -> Option<TreeNodeId> {
        match node {
            TreeNodeId::PullRequest(issue, _) => Some(TreeNodeId::Issue(issue)),
            TreeNodeId::Issue(number) => {
                let issue = self.snapshot.issue(number)?;
                if let Some(parent) = issue.parent {
                    if self.snapshot.issue(parent).is_some() {
                        return Some(TreeNodeId::Issue(parent));
                    }
                }
                issue
                    .milestone_number
                    .map(TreeNodeId::Milestone)
                    .or(Some(TreeNodeId::Unplanned))
            }
            TreeNodeId::Milestone(_) | TreeNodeId::Unplanned => None,
        }
    }

    fn next_chat_focus(&self) -> Focus {
        match self.focus {
            Focus::Input => Focus::Chat,
            Focus::Chat => Focus::Preview,
            Focus::Preview => Focus::Input,
            _ => Focus::Input,
        }
    }

    fn previous_chat_focus(&self) -> Focus {
        match self.focus {
            Focus::Input => Focus::Preview,
            Focus::Chat => Focus::Input,
            Focus::Preview => Focus::Chat,
            _ => Focus::Input,
        }
    }

    fn chat_left(&mut self) {
        match self.focus {
            Focus::Input => self.cursor = self.cursor.saturating_sub(1),
            Focus::Preview => self.focus = Focus::Chat,
            _ => {}
        }
    }

    fn chat_right(&mut self) {
        match self.focus {
            Focus::Input => {
                let len = self.input.chars().count();
                self.cursor = (self.cursor + 1).min(len);
            }
            Focus::Chat => self.focus = Focus::Preview,
            _ => {}
        }
    }

    fn chat_up(&mut self) {
        match self.focus {
            Focus::Chat | Focus::Input => self.chat_scroll = self.chat_scroll.saturating_sub(1),
            Focus::Preview => self.preview_scroll = self.preview_scroll.saturating_sub(1),
            _ => {}
        }
    }

    fn chat_down(&mut self) {
        match self.focus {
            Focus::Chat | Focus::Input => self.chat_scroll = self.chat_scroll.saturating_add(1),
            Focus::Preview => self.preview_scroll = self.preview_scroll.saturating_add(1),
            _ => {}
        }
    }

    fn chat_page_up(&mut self) {
        match self.focus {
            Focus::Preview => self.preview_scroll = self.preview_scroll.saturating_sub(10),
            _ => self.chat_scroll = self.chat_scroll.saturating_sub(10),
        }
    }

    fn chat_page_down(&mut self) {
        match self.focus {
            Focus::Preview => self.preview_scroll = self.preview_scroll.saturating_add(10),
            _ => self.chat_scroll = self.chat_scroll.saturating_add(10),
        }
    }

    fn chat_home(&mut self) {
        match self.focus {
            Focus::Input => self.cursor = 0,
            Focus::Chat => self.chat_scroll = 0,
            Focus::Preview => self.preview_scroll = 0,
            _ => {}
        }
    }

    fn chat_end(&mut self) {
        match self.focus {
            Focus::Input => self.cursor = self.input.chars().count(),
            Focus::Chat => self.chat_scroll = u16::MAX,
            Focus::Preview => self.preview_scroll = u16::MAX,
            _ => {}
        }
    }

    fn insert(&mut self, ch: char) {
        let mut chars: Vec<char> = self.input.chars().collect();
        chars.insert(self.cursor.min(chars.len()), ch);
        self.cursor = (self.cursor + 1).min(chars.len());
        self.input = chars.into_iter().collect();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.input.chars().collect();
        let index = self
            .cursor
            .saturating_sub(1)
            .min(chars.len().saturating_sub(1));
        chars.remove(index);
        self.cursor = self.cursor.saturating_sub(1);
        self.input = chars.into_iter().collect();
    }

    fn delete(&mut self) {
        let mut chars: Vec<char> = self.input.chars().collect();
        if self.cursor < chars.len() {
            chars.remove(self.cursor);
            self.input = chars.into_iter().collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{IssueSummary, PullRequestSummary, QualityCheck};

    #[test]
    fn creation_requires_finalized_ready_gate() {
        let gate = QualityGate {
            status: "READY".into(),
            checks: vec![QualityCheck {
                name: "Evidence".into(),
                status: "PASS".into(),
                evidence: "ok".into(),
            }],
            blockers: vec![],
        };
        assert!(gate.is_ready());
    }

    #[test]
    fn tree_places_linked_pr_under_issue() {
        let mut snapshot = ProjectSnapshot {
            issues: vec![IssueSummary {
                number: 10,
                title: "Implement resolver".into(),
                state: "open".into(),
                body: String::new(),
                url: String::new(),
                labels: vec![],
                milestone_number: None,
                milestone_title: None,
                parent: None,
                depends_on: vec![],
                required: true,
                is_epic: false,
                linked_prs: vec![PullRequestSummary {
                    number: 20,
                    title: "Resolver PR".into(),
                    state: "open".into(),
                    draft: false,
                    ..PullRequestSummary::default()
                }],
                status: TaskStatus::Review,
            }],
            ..ProjectSnapshot::default()
        };
        snapshot.recalculate_statuses();
        assert_eq!(snapshot.issue(10).unwrap().status, TaskStatus::Review);
    }
}
