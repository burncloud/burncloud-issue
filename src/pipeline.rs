use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc,
    },
    thread,
    time::Duration,
};

use anyhow::{anyhow, Result};

use crate::{
    codex::{AgentMode, CodexConfig, CodexExecution},
    github::GithubClient,
    models::{AgentResponse, ChatMessage, DuplicateCandidate},
};

pub struct FinalizeExecution {
    pub receiver: Receiver<Result<FinalizeResult, String>>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub struct FinalizeResult {
    pub response: AgentResponse,
    pub duplicates: Vec<DuplicateCandidate>,
}

pub fn start_finalize(
    codex: CodexConfig,
    github: GithubClient,
    messages: Vec<ChatMessage>,
) -> FinalizeExecution {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let task_cancel = Arc::clone(&cancel);
    thread::spawn(move || {
        let result = finalize(&codex, &github, &messages, &task_cancel)
            .map_err(|error| format!("{error:#}"));
        let _ = tx.send(result);
    });
    FinalizeExecution {
        receiver: rx,
        cancel,
    }
}

fn finalize(
    codex: &CodexConfig,
    github: &GithubClient,
    messages: &[ChatMessage],
    cancel: &Arc<AtomicBool>,
) -> Result<FinalizeResult> {
    let first = codex.start(messages.to_vec(), AgentMode::Finalize, vec![]);
    let mut response = wait_for_codex(first, cancel)?;
    let draft = response
        .draft
        .clone()
        .ok_or_else(|| anyhow!("Codex 尚未形成可检查的 Issue 草稿，请继续对话补充信息"))?;

    if cancel.load(Ordering::Relaxed) {
        return Err(anyhow!("Issue 生成已取消"));
    }
    let duplicates = github.search_duplicates(&draft.title)?;

    let mut quality_messages = messages.to_vec();
    quality_messages.push(ChatMessage::assistant(response.assistant_message.clone()));
    quality_messages.push(ChatMessage {
        role: crate::models::MessageRole::System,
        content: format!(
            "FINALIZE_DRAFT_JSON:\n{}",
            serde_json::to_string_pretty(&draft)?
        ),
    });
    let quality = codex.start(
        quality_messages,
        AgentMode::QualityGate,
        duplicates.clone(),
    );
    let quality_response = wait_for_codex(quality, cancel)?;
    if quality_response.draft.is_some() {
        response.draft = quality_response.draft;
    }
    response.assistant_message = quality_response.assistant_message;
    response.stage = quality_response.stage;
    response.questions = quality_response.questions;
    response.quality_gate = quality_response.quality_gate;

    Ok(FinalizeResult {
        response,
        duplicates,
    })
}

fn wait_for_codex(execution: CodexExecution, cancel: &Arc<AtomicBool>) -> Result<AgentResponse> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            execution.cancel.store(true, Ordering::Relaxed);
            return Err(anyhow!("Issue 生成已取消"));
        }
        match execution.receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => return result.map_err(anyhow::Error::msg),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("Codex worker unexpectedly disconnected"));
            }
        }
    }
}
