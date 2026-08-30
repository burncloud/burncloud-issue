use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use tempfile::TempDir;

use crate::models::{AgentResponse, ChatMessage, DuplicateCandidate, MessageRole};

#[derive(Debug, Clone, Copy)]
pub enum AgentMode {
    Chat,
    Finalize,
    QualityGate,
}

#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub local_repo: PathBuf,
    pub repository: String,
    pub timeout: Duration,
    pub model: Option<String>,
}

pub struct CodexExecution {
    pub receiver: Receiver<Result<AgentResponse, String>>,
    pub cancel: Arc<AtomicBool>,
}

impl CodexConfig {
    pub fn start(
        &self,
        messages: Vec<ChatMessage>,
        mode: AgentMode,
        duplicates: Vec<DuplicateCandidate>,
    ) -> CodexExecution {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = Arc::clone(&cancel);
        let config = self.clone();
        thread::spawn(move || {
            let result = run_codex(&config, &messages, mode, &duplicates, &task_cancel)
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
        });
        CodexExecution {
            receiver: rx,
            cancel,
        }
    }
}

pub fn codex_available() -> bool {
    Command::new("codex")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn run_codex(
    config: &CodexConfig,
    messages: &[ChatMessage],
    mode: AgentMode,
    duplicates: &[DuplicateCandidate],
    cancel: &Arc<AtomicBool>,
) -> Result<AgentResponse> {
    if !codex_available() {
        return Err(anyhow!(
            "未检测到本地 Codex CLI。请先确认 `codex --version` 可以运行。"
        ));
    }

    let temp = TempDir::new().context("create Codex temp directory")?;
    let schema = temp.path().join("issue-agent-schema.json");
    let response_file = temp.path().join("response.json");
    let stderr_file = temp.path().join("codex.stderr.log");
    fs::write(&schema, OUTPUT_SCHEMA).context("write Codex output schema")?;

    let prompt = build_prompt(config, messages, mode, duplicates);
    let stderr = File::create(&stderr_file).context("create Codex stderr log")?;
    let mut command = Command::new("codex");
    command
        .arg("exec")
        .arg("--ephemeral")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--output-schema")
        .arg(&schema)
        .arg("--output-last-message")
        .arg(&response_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));

    if config.local_repo.join(".git").exists() || config.local_repo.join("Cargo.toml").exists() {
        command.arg("-C").arg(&config.local_repo);
    } else {
        command.arg("--skip-git-repo-check");
    }
    if let Some(model) = &config.model {
        command.arg("-m").arg(model);
    }
    command.arg(prompt);

    let started = Instant::now();
    let mut child = command.spawn().context("启动本地 Codex CLI")?;
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!("Codex 对话已取消"));
        }
        if started.elapsed() >= config.timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "Codex 超过 {} 秒未返回，已终止",
                config.timeout.as_secs()
            ));
        }
        if let Some(status) = child.try_wait().context("poll Codex process")? {
            break status;
        }
        thread::sleep(Duration::from_millis(100));
    };

    if !status.success() {
        let stderr = fs::read_to_string(&stderr_file).unwrap_or_default();
        return Err(anyhow!(
            "Codex 执行失败 (exit {:?}): {}",
            status.code(),
            tail(&stderr, 4000)
        ));
    }

    let raw = fs::read_to_string(&response_file).context("读取 Codex 结构化回复")?;
    serde_json::from_str(&raw).with_context(|| format!("解析 Codex JSON: {}", tail(&raw, 2000)))
}

fn build_prompt(
    config: &CodexConfig,
    messages: &[ChatMessage],
    mode: AgentMode,
    duplicates: &[DuplicateCandidate],
) -> String {
    let mode_text = match mode {
        AgentMode::Chat => "CHAT：继续一问一答。每轮最多提出一个最重要的澄清问题；如果信息已经足够，可以同步维护 draft，但不要宣称已经创建 Issue。",
        AgentMode::Finalize => "FINALIZE：根据全部对话形成完整 Issue 草稿并执行第一轮质量门禁。任何关键事实不明确时必须返回 NEEDS_EVIDENCE 或 NEEDS_SPLIT，而不是猜。",
        AgentMode::QualityGate => "QUALITY_GATE：结合重复 Issue 候选做最终质量审查。只有真实证据、问题定义、边界、验收标准、大小和去重全部通过时才允许 READY。",
    };

    let conversation = messages
        .iter()
        .map(|message| {
            let role = match message.role {
                MessageRole::User => "USER",
                MessageRole::Assistant => "ASSISTANT",
                MessageRole::System => "SYSTEM",
            };
            format!("[{role}] {}", message.content)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let duplicate_text = if duplicates.is_empty() {
        "无重复候选。".to_string()
    } else {
        duplicates
            .iter()
            .map(|item| format!("#{} [{}] {} {}", item.number, item.state, item.title, item.url))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"你是 BurnCloud Issue Factory 的对话式 Issue Engineer。目标仓库：{repository}。

你运行在 read-only 模式。可以阅读本地代码来验证用户提到的文件、模块和事实，但绝对不能修改代码、创建 Issue、提交 Git、执行破坏性操作，也不能把推测写成事实。

=== BurnCloud Issue Standard v1 ===
每个正式 Issue 必须回答：
1. WHY：为什么这是一个真实问题。
2. WHAT：当前行为和预期行为的差异。
3. EVIDENCE：真实证据，优先文件/函数/日志/测试/PR；没有证据就明确 NEEDS_EVIDENCE。
4. ROOT CAUSE：只有证据支持时才给确定根因；否则降低 root_cause_confidence。
5. WHERE：影响组件和范围。
6. BOUNDARY：明确允许修改与禁止修改。
7. DONE：每条验收标准都必须可通过测试、命令、API 或明确 UI 行为验证。
8. SIZE：原则上一个 Issue 对应一个可以独立 Review 的 PR；太大返回 NEEDS_SPLIT。
9. DEDUP：创建前必须检查重复/相关/回归；疑似重复不能 READY。
10. USER CONSENT：你永远没有创建 GitHub Issue 的权限。最终创建必须由应用获得使用者明确确认。

Quality Gate 只允许以下最终状态：READY / NEEDS_EVIDENCE / DUPLICATE / NEEDS_SPLIT / BLOCKED / REJECTED。
Quality checks 至少覆盖 Evidence、Problem、Root Cause、Duplicate、Scope、Size、Acceptance、Risk、Dependencies；READY 时这些 check 必须全部 PASS，blockers 必须为空。

Issue 类型优先使用 BUG / SECURITY / ARCHITECTURE / REFACTOR / TEST / PERFORMANCE / UX / RELIABILITY / FOLLOW_UP。
severity 使用 BLOCKER / MAJOR / MINOR / NIT；risk 使用 R0-R4；confidence 使用 HIGH / MEDIUM / LOW。
所有面向人的文字使用简体中文；代码路径、函数名、命令和协议标识保持原样。

对话规则：
- 不要一次问一串问题；CHAT 模式每轮最多问一个最关键问题。
- 优先追问会改变 Issue 边界、真实性或验收方式的信息。
- 用户不懂代码时，你应主动通过只读代码检查补足技术上下文，而不是要求用户解释实现细节。
- 不要把“重构整个系统”“全面优化”这样的任务直接判 READY，必须拆分。
- 不要因为用户要求创建就跳过 Quality Gate。

当前模式：{mode_text}

=== 对话 ===
{conversation}

=== GitHub 重复候选 ===
{duplicate_text}

请严格按照 output schema 返回 JSON。assistant_message 是这一轮给用户看的自然语言回复；questions 最多 1 条。"#,
        repository = config.repository,
        mode_text = mode_text,
        conversation = conversation,
        duplicate_text = duplicate_text,
    )
}

fn tail(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut start = value.len() - max;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_string()
}

const OUTPUT_SCHEMA: &str = r##"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "additionalProperties": false,
  "required": ["assistant_message", "stage", "questions", "draft", "quality_gate"],
  "properties": {
    "assistant_message": {"type": "string"},
    "stage": {"type": "string", "enum": ["CLARIFYING", "DRAFTING", "READY", "BLOCKED"]},
    "questions": {"type": "array", "maxItems": 1, "items": {"type": "string"}},
    "draft": {
      "anyOf": [
        {"type": "null"},
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["title", "issue_type", "severity", "risk", "confidence", "problem", "current_behavior", "expected_behavior", "evidence", "root_cause", "root_cause_confidence", "affected_components", "scope", "acceptance_criteria", "test_requirements", "dependencies", "labels", "duplicate_of"],
          "properties": {
            "title": {"type": "string"},
            "issue_type": {"type": "string"},
            "severity": {"type": "string"},
            "risk": {"type": "string"},
            "confidence": {"type": "string"},
            "problem": {"type": "string"},
            "current_behavior": {"type": "string"},
            "expected_behavior": {"type": "string"},
            "evidence": {"type": "array", "items": {"$ref": "#/$defs/evidence"}},
            "root_cause": {"type": "string"},
            "root_cause_confidence": {"type": "string"},
            "affected_components": {"type": "array", "items": {"type": "string"}},
            "scope": {"$ref": "#/$defs/scope"},
            "acceptance_criteria": {"type": "array", "items": {"type": "string"}},
            "test_requirements": {"type": "array", "items": {"type": "string"}},
            "dependencies": {"type": "array", "items": {"type": "string"}},
            "labels": {"type": "array", "items": {"type": "string"}},
            "duplicate_of": {"type": ["integer", "null"]}
          }
        }
      ]
    },
    "quality_gate": {"$ref": "#/$defs/qualityGate"}
  },
  "$defs": {
    "evidence": {
      "type": "object",
      "additionalProperties": false,
      "required": ["source", "location", "fact"],
      "properties": {
        "source": {"type": "string"},
        "location": {"type": "string"},
        "fact": {"type": "string"}
      }
    },
    "scope": {
      "type": "object",
      "additionalProperties": false,
      "required": ["allowed", "forbidden"],
      "properties": {
        "allowed": {"type": "array", "items": {"type": "string"}},
        "forbidden": {"type": "array", "items": {"type": "string"}}
      }
    },
    "qualityCheck": {
      "type": "object",
      "additionalProperties": false,
      "required": ["name", "status", "evidence"],
      "properties": {
        "name": {"type": "string"},
        "status": {"type": "string", "enum": ["PASS", "FAIL", "WARN"]},
        "evidence": {"type": "string"}
      }
    },
    "qualityGate": {
      "type": "object",
      "additionalProperties": false,
      "required": ["status", "checks", "blockers"],
      "properties": {
        "status": {"type": "string", "enum": ["READY", "NEEDS_EVIDENCE", "DUPLICATE", "NEEDS_SPLIT", "BLOCKED", "REJECTED"]},
        "checks": {"type": "array", "items": {"$ref": "#/$defs/qualityCheck"}},
        "blockers": {"type": "array", "items": {"type": "string"}}
      }
    }
  }
}"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_schema_is_valid_json() {
        let _: serde_json::Value = serde_json::from_str(OUTPUT_SCHEMA).unwrap();
    }

    #[test]
    fn prompt_requires_one_question_and_user_consent() {
        let config = CodexConfig {
            local_repo: PathBuf::from("../burncloud"),
            repository: "burncloud/burncloud".into(),
            timeout: Duration::from_secs(300),
            model: None,
        };
        let prompt = build_prompt(
            &config,
            &[ChatMessage::user("登录跳转有问题")],
            AgentMode::Chat,
            &[],
        );
        assert!(prompt.contains("每轮最多问一个"));
        assert!(prompt.contains("USER CONSENT"));
    }
}
