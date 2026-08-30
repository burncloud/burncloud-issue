use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EvidenceItem {
    pub source: String,
    pub location: String,
    pub fact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScopeBoundary {
    pub allowed: Vec<String>,
    pub forbidden: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IssueDraft {
    pub title: String,
    pub issue_type: String,
    pub severity: String,
    pub risk: String,
    pub confidence: String,
    pub problem: String,
    pub current_behavior: String,
    pub expected_behavior: String,
    pub evidence: Vec<EvidenceItem>,
    pub root_cause: String,
    pub root_cause_confidence: String,
    pub affected_components: Vec<String>,
    pub scope: ScopeBoundary,
    pub acceptance_criteria: Vec<String>,
    pub test_requirements: Vec<String>,
    pub dependencies: Vec<String>,
    pub labels: Vec<String>,
    pub duplicate_of: Option<u64>,
}

impl IssueDraft {
    pub fn to_markdown(&self, repository: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Type: {}  \nSeverity: {}  \nRisk: {}  \nConfidence: {}  \nRepository: `{}`\n\n",
            self.issue_type, self.severity, self.risk, self.confidence, repository
        ));
        section(&mut out, "问题", &self.problem);
        section(&mut out, "当前行为", &self.current_behavior);
        section(&mut out, "预期行为", &self.expected_behavior);

        out.push_str("## 证据\n\n");
        if self.evidence.is_empty() {
            out.push_str("- 尚无可验证证据。\n\n");
        } else {
            for item in &self.evidence {
                out.push_str(&format!(
                    "- **{}** · `{}` — {}\n",
                    item.source, item.location, item.fact
                ));
            }
            out.push('\n');
        }

        section(&mut out, "根因", &self.root_cause);
        out.push_str(&format!("根因置信度：{}\n\n", self.root_cause_confidence));

        list_section(&mut out, "影响范围", &self.affected_components, false);
        list_section(&mut out, "允许修改", &self.scope.allowed, false);
        list_section(&mut out, "禁止修改", &self.scope.forbidden, false);
        list_section(&mut out, "验收标准", &self.acceptance_criteria, true);
        list_section(&mut out, "测试要求", &self.test_requirements, true);
        list_section(&mut out, "依赖", &self.dependencies, false);

        if let Some(number) = self.duplicate_of {
            out.push_str(&format!("## 重复关系\n\n可能重复于 #{number}\n\n"));
        }
        out.push_str("---\nGenerated with BurnCloud Issue Standard v1.\n");
        out
    }
}

fn section(out: &mut String, title: &str, body: &str) {
    out.push_str(&format!("## {title}\n\n"));
    if body.trim().is_empty() {
        out.push_str("尚未明确。\n\n");
    } else {
        out.push_str(body.trim());
        out.push_str("\n\n");
    }
}

fn list_section(out: &mut String, title: &str, values: &[String], checklist: bool) {
    out.push_str(&format!("## {title}\n\n"));
    if values.is_empty() {
        out.push_str("- 尚未明确。\n\n");
        return;
    }
    for value in values {
        if checklist {
            out.push_str(&format!("- [ ] {}\n", value.trim()));
        } else {
            out.push_str(&format!("- {}\n", value.trim()));
        }
    }
    out.push('\n');
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QualityCheck {
    pub name: String,
    pub status: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QualityGate {
    pub status: String,
    pub checks: Vec<QualityCheck>,
    pub blockers: Vec<String>,
}

impl QualityGate {
    pub fn is_ready(&self) -> bool {
        self.status.eq_ignore_ascii_case("READY")
            && self
                .checks
                .iter()
                .all(|check| check.status.eq_ignore_ascii_case("PASS"))
            && self.blockers.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentResponse {
    pub assistant_message: String,
    pub stage: String,
    pub questions: Vec<String>,
    pub draft: Option<IssueDraft>,
    pub quality_gate: QualityGate,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DuplicateCandidate {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_requires_every_quality_check_to_pass() {
        let gate = QualityGate {
            status: "READY".into(),
            checks: vec![QualityCheck {
                name: "Evidence".into(),
                status: "PASS".into(),
                evidence: "真实日志".into(),
            }],
            blockers: vec![],
        };
        assert!(gate.is_ready());
    }

    #[test]
    fn markdown_contains_scope_and_acceptance_sections() {
        let draft = IssueDraft {
            title: "修复路由回退".into(),
            issue_type: "BUG".into(),
            severity: "MAJOR".into(),
            risk: "R3".into(),
            confidence: "HIGH".into(),
            problem: "首个 Provider 失败后提前返回。".into(),
            current_behavior: "返回 503。".into(),
            expected_behavior: "继续下一个 Provider。".into(),
            scope: ScopeBoundary {
                allowed: vec!["Router fallback".into()],
                forbidden: vec!["Billing".into()],
            },
            acceptance_criteria: vec!["503 时继续回退".into()],
            ..IssueDraft::default()
        };
        let body = draft.to_markdown("burncloud/burncloud");
        assert!(body.contains("## 允许修改"));
        assert!(body.contains("## 禁止修改"));
        assert!(body.contains("- [ ] 503 时继续回退"));
    }
}
