use std::{
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
    models::{AgentResponse, ChatMessage, DuplicateCandidate, IssueDraft, QualityGate},
    pipeline::{start_finalize, FinalizeExecution},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Chat,
    Input,
    Preview,
}

pub struct App {
    pub repository: String,
    pub local_repo: PathBuf,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub cursor: usize,
    pub focus: Focus,
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
    codex: CodexConfig,
    github: GithubClient,
    ai: Option<CodexExecution>,
    finalize: Option<FinalizeExecution>,
    create: Option<Receiver<Result<CreatedIssue, String>>>,
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
        Ok(Self {
            repository,
            local_repo,
            messages: vec![ChatMessage::assistant(
                "请先用你自己的话描述想解决的问题。我会一次只追问一个最关键的问题，并逐步把它整理成可执行、可验证的 BurnCloud Issue。",
            )],
            input: String::new(),
            cursor: 0,
            focus: Focus::Input,
            chat_scroll: u16::MAX,
            preview_scroll: 0,
            draft: None,
            quality_gate: QualityGate::default(),
            duplicates: Vec::new(),
            quality_finalized: false,
            confirm_create: false,
            status: format!(
                "就绪 · GitHub {} · Enter 发送 · F2 最终检查 · F4 创建",
                github.auth_label()
            ),
            created_issue: None,
            should_quit: false,
            ai_started: None,
            finalize_started: None,
            codex,
            github,
            ai: None,
            finalize: None,
            create: None,
        })
    }

    pub fn tick(&mut self) {
        self.poll_ai();
        self.poll_finalize();
        self.poll_create();
    }

    pub fn is_busy(&self) -> bool {
        self.ai.is_some() || self.finalize.is_some() || self.create.is_some()
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

        match key.code {
            KeyCode::F(2) => self.finalize_issue(),
            KeyCode::F(4) => self.request_create(),
            KeyCode::Tab => self.focus = self.next_focus(),
            KeyCode::BackTab => self.focus = self.previous_focus(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            KeyCode::PageUp => self.page_up(),
            KeyCode::PageDown => self.page_down(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            KeyCode::Enter if self.focus == Focus::Input => self.send_input(),
            KeyCode::Backspace if self.focus == Focus::Input => self.backspace(),
            KeyCode::Delete if self.focus == Focus::Input => self.delete(),
            KeyCode::Char(ch) if self.focus == Focus::Input => self.insert(ch),
            _ => {}
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
            .any(|message| matches!(message.role, crate::models::MessageRole::User))
        {
            self.status = "请先描述问题，再执行最终检查。".into();
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
                        self.status =
                            format!("已创建 Issue #{} · {}", issue.number, issue.html_url);
                        self.messages.push(ChatMessage::assistant(format!(
                            "Issue #{} 已创建：{}",
                            issue.number, issue.html_url
                        )));
                        self.created_issue = Some(issue);
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
        if let Some(draft) = response.draft {
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
            self.status = "Codex 已回复。继续回答问题，或在信息充分后按 F2 做最终检查。".into();
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

    fn next_focus(&self) -> Focus {
        match self.focus {
            Focus::Input => Focus::Chat,
            Focus::Chat => Focus::Preview,
            Focus::Preview => Focus::Input,
        }
    }

    fn previous_focus(&self) -> Focus {
        match self.focus {
            Focus::Input => Focus::Preview,
            Focus::Chat => Focus::Input,
            Focus::Preview => Focus::Chat,
        }
    }

    fn left(&mut self) {
        match self.focus {
            Focus::Input => self.cursor = self.cursor.saturating_sub(1),
            Focus::Preview => self.focus = Focus::Chat,
            Focus::Chat => {}
        }
    }

    fn right(&mut self) {
        match self.focus {
            Focus::Input => {
                let len = self.input.chars().count();
                self.cursor = (self.cursor + 1).min(len);
            }
            Focus::Chat => self.focus = Focus::Preview,
            Focus::Preview => {}
        }
    }

    fn up(&mut self) {
        match self.focus {
            Focus::Chat | Focus::Input => self.chat_scroll = self.chat_scroll.saturating_sub(1),
            Focus::Preview => self.preview_scroll = self.preview_scroll.saturating_sub(1),
        }
    }

    fn down(&mut self) {
        match self.focus {
            Focus::Chat | Focus::Input => self.chat_scroll = self.chat_scroll.saturating_add(1),
            Focus::Preview => self.preview_scroll = self.preview_scroll.saturating_add(1),
        }
    }

    fn page_up(&mut self) {
        match self.focus {
            Focus::Preview => self.preview_scroll = self.preview_scroll.saturating_sub(10),
            _ => self.chat_scroll = self.chat_scroll.saturating_sub(10),
        }
    }

    fn page_down(&mut self) {
        match self.focus {
            Focus::Preview => self.preview_scroll = self.preview_scroll.saturating_add(10),
            _ => self.chat_scroll = self.chat_scroll.saturating_add(10),
        }
    }

    fn home(&mut self) {
        match self.focus {
            Focus::Input => self.cursor = 0,
            Focus::Chat => self.chat_scroll = 0,
            Focus::Preview => self.preview_scroll = 0,
        }
    }

    fn end(&mut self) {
        match self.focus {
            Focus::Input => self.cursor = self.input.chars().count(),
            Focus::Chat => self.chat_scroll = u16::MAX,
            Focus::Preview => self.preview_scroll = u16::MAX,
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
    use crate::models::QualityCheck;

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
        // App::can_create additionally requires quality_finalized + draft + no prior creation.
    }
}
