use std::collections::HashSet;

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

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Ready,
    InProgress,
    Review,
    Blocked,
    Done,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "可开始",
            Self::InProgress => "进行中",
            Self::Review => "审查中",
            Self::Blocked => "阻塞",
            Self::Done => "完成",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MilestoneSummary {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PullRequestSummary {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
    pub draft: bool,
    pub merged: bool,
    pub head_sha: String,
    pub ci_state: String,
    pub review_state: String,
}

impl PullRequestSummary {
    pub fn state_label(&self) -> &'static str {
        if self.merged {
            "MERGED"
        } else if self.draft {
            "DRAFT"
        } else if self.state.eq_ignore_ascii_case("open") {
            "OPEN"
        } else {
            "CLOSED"
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummary {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub body: String,
    pub url: String,
    pub labels: Vec<String>,
    pub milestone_number: Option<u64>,
    pub milestone_title: Option<String>,
    pub parent: Option<u64>,
    pub depends_on: Vec<u64>,
    pub required: bool,
    pub is_epic: bool,
    pub linked_prs: Vec<PullRequestSummary>,
    pub status: TaskStatus,
}

impl IssueSummary {
    pub fn checklist_progress(&self) -> (usize, usize) {
        let mut done = 0usize;
        let mut total = 0usize;
        for line in self.body.lines() {
            let line = line.trim_start();
            if line.starts_with("- [ ]") {
                total += 1;
            } else if line.starts_with("- [x]") || line.starts_with("- [X]") {
                total += 1;
                done += 1;
            }
        }
        (done, total)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectSnapshot {
    pub milestones: Vec<MilestoneSummary>,
    pub issues: Vec<IssueSummary>,
}

impl ProjectSnapshot {
    pub fn recalculate_statuses(&mut self) {
        let closed: HashSet<u64> = self
            .issues
            .iter()
            .filter(|issue| issue.state.eq_ignore_ascii_case("closed"))
            .map(|issue| issue.number)
            .collect();

        for issue in &mut self.issues {
            issue.status = if issue.state.eq_ignore_ascii_case("closed") {
                TaskStatus::Done
            } else if issue
                .linked_prs
                .iter()
                .any(|pr| pr.state.eq_ignore_ascii_case("open") && !pr.draft)
            {
                TaskStatus::Review
            } else if issue
                .linked_prs
                .iter()
                .any(|pr| pr.state.eq_ignore_ascii_case("open") && pr.draft)
            {
                TaskStatus::InProgress
            } else if issue.depends_on.iter().any(|number| !closed.contains(number)) {
                TaskStatus::Blocked
            } else {
                TaskStatus::Ready
            };
        }
    }

    pub fn issue(&self, number: u64) -> Option<&IssueSummary> {
        self.issues.iter().find(|issue| issue.number == number)
    }

    pub fn children_of(&self, parent: u64) -> Vec<&IssueSummary> {
        let mut children = self
            .issues
            .iter()
            .filter(|issue| issue.parent == Some(parent))
            .collect::<Vec<_>>();
        children.sort_by_key(|issue| issue.number);
        children
    }

    pub fn root_issues_for_milestone(&self, milestone: Option<u64>) -> Vec<&IssueSummary> {
        let mut roots = self
            .issues
            .iter()
            .filter(|issue| {
                if issue.milestone_number != milestone {
                    return false;
                }
                let parent_in_same_group = issue
                    .parent
                    .and_then(|parent| self.issue(parent))
                    .map(|parent| parent.milestone_number == milestone)
                    .unwrap_or(false);
                !parent_in_same_group
            })
            .collect::<Vec<_>>();
        roots.sort_by_key(|issue| issue.number);
        roots
    }

    pub fn ready_issues(&self) -> Vec<&IssueSummary> {
        self.issues
            .iter()
            .filter(|issue| issue.status == TaskStatus::Ready)
            .collect()
    }

    pub fn overall_progress(&self) -> (usize, usize, u16) {
        progress_for(self.issues.iter().filter(|issue| issue.required))
    }

    pub fn milestone_progress(&self, milestone: Option<u64>) -> (usize, usize, u16) {
        progress_for(
            self.issues
                .iter()
                .filter(|issue| issue.required && issue.milestone_number == milestone),
        )
    }

    pub fn issue_progress(&self, root: u64) -> (usize, usize, u16) {
        let mut descendants = Vec::new();
        let mut seen = HashSet::new();
        self.collect_descendants(root, &mut seen, &mut descendants);
        progress_for(descendants.into_iter().filter(|issue| issue.required))
    }

    fn collect_descendants<'a>(
        &'a self,
        root: u64,
        seen: &mut HashSet<u64>,
        out: &mut Vec<&'a IssueSummary>,
    ) {
        if !seen.insert(root) {
            return;
        }
        if let Some(issue) = self.issue(root) {
            out.push(issue);
        }
        for child in self.children_of(root) {
            self.collect_descendants(child.number, seen, out);
        }
    }
}

fn progress_for<'a>(issues: impl Iterator<Item = &'a IssueSummary>) -> (usize, usize, u16) {
    let mut done = 0usize;
    let mut total = 0usize;
    for issue in issues {
        total += 1;
        if issue.status == TaskStatus::Done {
            done += 1;
        }
    }
    let percent = done
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100) as u16;
    (done, total, percent)
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
    #[serde(default)]
    pub parent_issue: Option<u64>,
    #[serde(default)]
    pub milestone: Option<u64>,
    #[serde(default)]
    pub required: Option<bool>,
}

impl IssueDraft {
    pub fn to_markdown(&self, repository: &str) -> String {
        let dependency_numbers = self
            .dependencies
            .iter()
            .flat_map(|value| extract_issue_numbers(value))
            .collect::<HashSet<_>>();
        let mut dependency_numbers = dependency_numbers.into_iter().collect::<Vec<_>>();
        dependency_numbers.sort_unstable();
        let parent = self
            .parent_issue
            .map(|number| number.to_string())
            .unwrap_or_else(|| "none".into());
        let depends_on = dependency_numbers
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");

        let mut out = String::new();
        out.push_str("<!-- burncloud-issue-tree\n");
        out.push_str(&format!("parent: {parent}\n"));
        out.push_str(&format!("depends_on: {depends_on}\n"));
        out.push_str(&format!(
            "required: {}\n",
            self.required.unwrap_or(true)
        ));
        out.push_str("-->\n\n");
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

pub fn extract_issue_numbers(value: &str) -> Vec<u64> {
    let chars = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] == b'#' {
            let mut end = index + 1;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            if end > index + 1 {
                if let Ok(number) = value[index + 1..end].parse::<u64>() {
                    out.push(number);
                }
                index = end;
                continue;
            }
        }
        index += 1;
    }
    out
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
    fn markdown_contains_scope_acceptance_and_tree_metadata() {
        let draft = IssueDraft {
            title: "修复路由回退".into(),
            issue_type: "BUG".into(),
            severity: "MAJOR".into(),
            risk: "R3".into(),
            confidence: "HIGH".into(),
            problem: "首个 Provider 失败后提前返回。".into(),
            current_behavior: "返回 503。".into(),
            expected_behavior: "继续下一个 Provider。".into(),
            parent_issue: Some(100),
            dependencies: vec!["#98".into(), "depends on #99".into()],
            scope: ScopeBoundary {
                allowed: vec!["Router fallback".into()],
                forbidden: vec!["Billing".into()],
            },
            acceptance_criteria: vec!["503 时继续回退".into()],
            ..IssueDraft::default()
        };
        let body = draft.to_markdown("burncloud/burncloud");
        assert!(body.contains("parent: 100"));
        assert!(body.contains("depends_on: 98,99"));
        assert!(body.contains("## 允许修改"));
        assert!(body.contains("- [ ] 503 时继续回退"));
    }

    #[test]
    fn snapshot_marks_dependencies_blocked_and_closed_done() {
        let mut snapshot = ProjectSnapshot {
            issues: vec![
                IssueSummary {
                    number: 1,
                    title: "A".into(),
                    state: "closed".into(),
                    body: String::new(),
                    url: String::new(),
                    labels: vec![],
                    milestone_number: None,
                    milestone_title: None,
                    parent: None,
                    depends_on: vec![],
                    required: true,
                    is_epic: false,
                    linked_prs: vec![],
                    status: TaskStatus::Ready,
                },
                IssueSummary {
                    number: 2,
                    title: "B".into(),
                    state: "open".into(),
                    body: String::new(),
                    url: String::new(),
                    labels: vec![],
                    milestone_number: None,
                    milestone_title: None,
                    parent: None,
                    depends_on: vec![1, 3],
                    required: true,
                    is_epic: false,
                    linked_prs: vec![],
                    status: TaskStatus::Ready,
                },
            ],
            ..ProjectSnapshot::default()
        };
        snapshot.recalculate_statuses();
        assert_eq!(snapshot.issue(1).unwrap().status, TaskStatus::Done);
        assert_eq!(snapshot.issue(2).unwrap().status, TaskStatus::Blocked);
    }

    #[test]
    fn extracts_issue_references() {
        assert_eq!(extract_issue_numbers("depends #12 and #34"), vec![12, 34]);
    }
}
