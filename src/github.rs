use std::{collections::{HashMap, HashSet}, env, process::Command, time::Duration};

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};

use crate::models::{
    extract_issue_numbers, DuplicateCandidate, IssueDraft, IssueSummary, MilestoneSummary,
    ProjectSnapshot, PullRequestSummary, TaskStatus,
};

#[derive(Debug, Clone)]
pub struct GithubClient {
    repository: String,
    client: Client,
    token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatedIssue {
    pub number: u64,
    pub html_url: String,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    number: u64,
    title: String,
    state: String,
    html_url: String,
}

#[derive(Debug, Serialize)]
struct CreateIssueRequest<'a> {
    title: &'a str,
    body: &'a str,
    labels: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    milestone: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiLabel {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiMilestone {
    number: u64,
    title: String,
    state: String,
    body: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiIssue {
    number: u64,
    title: String,
    state: String,
    body: Option<String>,
    html_url: String,
    labels: Vec<ApiLabel>,
    milestone: Option<ApiMilestone>,
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiHead {
    sha: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiPullRequest {
    number: u64,
    title: String,
    state: String,
    body: Option<String>,
    html_url: String,
    draft: bool,
    merged_at: Option<String>,
    head: ApiHead,
}

#[derive(Debug, Deserialize)]
struct CheckRunsResponse {
    check_runs: Vec<ApiCheckRun>,
}

#[derive(Debug, Deserialize)]
struct ApiCheckRun {
    status: String,
    conclusion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiReview {
    state: String,
}

impl GithubClient {
    pub fn new(repository: impl Into<String>) -> Result<Self> {
        let repository = repository.into();
        if repository.split('/').count() != 2 {
            return Err(anyhow!("仓库必须使用 owner/name 格式: {repository}"));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("burncloud-issue/0.2")
            .build()
            .context("build GitHub HTTP client")?;
        Ok(Self {
            repository,
            client,
            token: resolve_token(),
        })
    }

    pub fn auth_label(&self) -> &'static str {
        if self.token.is_some() {
            "authenticated"
        } else {
            "anonymous (create disabled)"
        }
    }

    pub fn sync_project(&self) -> Result<ProjectSnapshot> {
        let api_issues = self.list_issues()?;
        let pulls = self.list_pull_requests()?;
        let issue_numbers = api_issues
            .iter()
            .filter(|issue| issue.pull_request.is_none())
            .map(|issue| issue.number)
            .collect::<HashSet<_>>();

        let mut linked: HashMap<u64, Vec<PullRequestSummary>> = HashMap::new();
        let mut pr_cache: HashMap<u64, PullRequestSummary> = HashMap::new();
        for pull in pulls {
            let mut text = pull.title.clone();
            if let Some(body) = &pull.body {
                text.push('\n');
                text.push_str(body);
            }
            let refs = extract_issue_numbers(&text)
                .into_iter()
                .filter(|number| issue_numbers.contains(number))
                .collect::<HashSet<_>>();
            if refs.is_empty() {
                continue;
            }
            let summary = if let Some(existing) = pr_cache.get(&pull.number) {
                existing.clone()
            } else {
                let summary = self.summarize_pr(&pull);
                pr_cache.insert(pull.number, summary.clone());
                summary
            };
            for number in refs {
                linked.entry(number).or_default().push(summary.clone());
            }
        }

        let mut milestones = HashMap::<u64, MilestoneSummary>::new();
        let mut issues = Vec::new();
        for issue in api_issues.into_iter().filter(|issue| issue.pull_request.is_none()) {
            let body = issue.body.unwrap_or_default();
            let relation = parse_tree_relation(&body);
            let labels = issue.labels.into_iter().map(|label| label.name).collect::<Vec<_>>();
            let is_epic = labels.iter().any(|label| {
                label.eq_ignore_ascii_case("epic")
                    || label.eq_ignore_ascii_case("type:epic")
                    || label.eq_ignore_ascii_case("type/epic")
            });
            let (milestone_number, milestone_title) = if let Some(milestone) = issue.milestone {
                milestones.entry(milestone.number).or_insert_with(|| MilestoneSummary {
                    number: milestone.number,
                    title: milestone.title.clone(),
                    state: milestone.state,
                    description: milestone.body.unwrap_or_default(),
                });
                (Some(milestone.number), Some(milestone.title))
            } else {
                (None, None)
            };
            let mut linked_prs = linked.remove(&issue.number).unwrap_or_default();
            linked_prs.sort_by_key(|pr| pr.number);
            issues.push(IssueSummary {
                number: issue.number,
                title: issue.title,
                state: issue.state,
                body,
                url: issue.html_url,
                labels,
                milestone_number,
                milestone_title,
                parent: relation.parent,
                depends_on: relation.depends_on,
                required: relation.required,
                is_epic,
                linked_prs,
                status: TaskStatus::Ready,
            });
        }

        let mut snapshot = ProjectSnapshot {
            milestones: milestones.into_values().collect(),
            issues,
        };
        snapshot.milestones.sort_by_key(|milestone| milestone.number);
        snapshot.issues.sort_by_key(|issue| issue.number);
        snapshot.recalculate_statuses();
        Ok(snapshot)
    }

    pub fn search_duplicates(&self, title: &str) -> Result<Vec<DuplicateCandidate>> {
        if title.trim().is_empty() {
            return Ok(Vec::new());
        }
        let query = format!(
            "repo:{} is:issue in:title {}",
            self.repository,
            title.trim()
        );
        let request = self
            .client
            .get("https://api.github.com/search/issues")
            .query(&[("q", query.as_str()), ("per_page", "8")]);
        let response = self.auth(request).send().context("search GitHub issues")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(anyhow!("GitHub Issue 搜索失败 {status}: {body}"));
        }
        let payload: SearchResponse = response.json().context("decode GitHub issue search")?;
        Ok(payload
            .items
            .into_iter()
            .map(|item| DuplicateCandidate {
                number: item.number,
                title: item.title,
                state: item.state,
                url: item.html_url,
            })
            .collect())
    }

    pub fn create_issue(&self, draft: &IssueDraft) -> Result<CreatedIssue> {
        if self.token.is_none() {
            return Err(anyhow!(
                "没有 GitHub 写入凭据。请设置 GITHUB_TOKEN / GH_TOKEN，或先执行 `gh auth login`。"
            ));
        }
        let body = draft.to_markdown(&self.repository);
        let url = format!("https://api.github.com/repos/{}/issues", self.repository);
        let request = self.client.post(url).json(&CreateIssueRequest {
            title: &draft.title,
            body: &body,
            labels: &draft.labels,
            milestone: draft.milestone,
        });
        let response = self.auth(request).send().context("create GitHub issue")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(anyhow!("创建 GitHub Issue 失败 {status}: {body}"));
        }
        response.json().context("decode created GitHub issue")
    }

    fn list_issues(&self) -> Result<Vec<ApiIssue>> {
        self.paginated("issues", &[("state", "all"), ("sort", "updated"), ("direction", "desc")])
    }

    fn list_pull_requests(&self) -> Result<Vec<ApiPullRequest>> {
        self.paginated("pulls", &[("state", "all"), ("sort", "updated"), ("direction", "desc")])
    }

    fn paginated<T>(&self, endpoint: &str, query: &[(&str, &str)]) -> Result<Vec<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut out = Vec::new();
        for page in 1..=10u32 {
            let url = format!("https://api.github.com/repos/{}/{}", self.repository, endpoint);
            let mut request = self.client.get(url).query(query);
            request = request.query(&[("per_page", "100"), ("page", &page.to_string())]);
            let response = self.auth(request).send().with_context(|| format!("list GitHub {endpoint}"))?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                return Err(anyhow!("GitHub {endpoint} 读取失败 {status}: {body}"));
            }
            let page_items: Vec<T> = response.json().with_context(|| format!("decode GitHub {endpoint}"))?;
            let len = page_items.len();
            out.extend(page_items);
            if len < 100 {
                break;
            }
        }
        Ok(out)
    }

    fn summarize_pr(&self, pull: &ApiPullRequest) -> PullRequestSummary {
        let should_enrich = pull.state.eq_ignore_ascii_case("open");
        PullRequestSummary {
            number: pull.number,
            title: pull.title.clone(),
            state: pull.state.clone(),
            url: pull.html_url.clone(),
            draft: pull.draft,
            merged: pull.merged_at.is_some(),
            head_sha: pull.head.sha.clone(),
            ci_state: if should_enrich {
                self.check_state(&pull.head.sha).unwrap_or_else(|_| "unknown".into())
            } else {
                "n/a".into()
            },
            review_state: if should_enrich {
                self.review_state(pull.number).unwrap_or_else(|_| "unknown".into())
            } else {
                "n/a".into()
            },
        }
    }

    fn check_state(&self, sha: &str) -> Result<String> {
        let url = format!(
            "https://api.github.com/repos/{}/commits/{}/check-runs?per_page=100",
            self.repository, sha
        );
        let response = self
            .auth(self.client.get(url))
            .header("Accept", "application/vnd.github+json")
            .send()
            .context("read GitHub check runs")?;
        if !response.status().is_success() {
            return Ok("unknown".into());
        }
        let payload: CheckRunsResponse = response.json().context("decode check runs")?;
        if payload.check_runs.is_empty() {
            return Ok("unknown".into());
        }
        if payload.check_runs.iter().any(|run| run.status != "completed") {
            return Ok("pending".into());
        }
        if payload.check_runs.iter().any(|run| {
            matches!(
                run.conclusion.as_deref(),
                Some("failure" | "cancelled" | "timed_out" | "action_required" | "startup_failure")
            )
        }) {
            return Ok("failure".into());
        }
        Ok("success".into())
    }

    fn review_state(&self, number: u64) -> Result<String> {
        let url = format!(
            "https://api.github.com/repos/{}/pulls/{}/reviews?per_page=100",
            self.repository, number
        );
        let response = self.auth(self.client.get(url)).send().context("read PR reviews")?;
        if !response.status().is_success() {
            return Ok("unknown".into());
        }
        let reviews: Vec<ApiReview> = response.json().context("decode PR reviews")?;
        if reviews
            .iter()
            .any(|review| review.state.eq_ignore_ascii_case("CHANGES_REQUESTED"))
        {
            Ok("changes_requested".into())
        } else if reviews
            .iter()
            .any(|review| review.state.eq_ignore_ascii_case("APPROVED"))
        {
            Ok("approved".into())
        } else {
            Ok("pending".into())
        }
    }

    fn auth(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some(token) = &self.token {
            request.bearer_auth(token)
        } else {
            request
        }
    }
}

#[derive(Debug, Default)]
struct TreeRelation {
    parent: Option<u64>,
    depends_on: Vec<u64>,
    required: bool,
}

fn parse_tree_relation(body: &str) -> TreeRelation {
    let mut relation = TreeRelation {
        required: true,
        ..TreeRelation::default()
    };
    let mut in_dependencies = false;
    let mut dependencies = HashSet::new();

    for raw in body.lines() {
        let line = raw.trim();
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("parent:") {
            relation.parent = parse_first_number(line);
        } else if lower.starts_with("depends_on:") {
            for number in parse_csv_numbers(line.split_once(':').map(|(_, value)| value).unwrap_or("")) {
                dependencies.insert(number);
            }
        } else if lower.starts_with("required:") {
            let value = line.split_once(':').map(|(_, value)| value.trim()).unwrap_or("true");
            relation.required = !value.eq_ignore_ascii_case("false");
        } else if lower.starts_with("parent issue:") || line.starts_with("父 Issue:") {
            relation.parent = parse_first_number(line);
        }

        if line.starts_with("## ") {
            in_dependencies = line == "## 依赖" || lower == "## dependencies";
            continue;
        }
        if in_dependencies {
            for number in extract_issue_numbers(line) {
                dependencies.insert(number);
            }
        }
    }

    let mut depends_on = dependencies.into_iter().collect::<Vec<_>>();
    depends_on.sort_unstable();
    relation.depends_on = depends_on;
    relation
}

fn parse_first_number(value: &str) -> Option<u64> {
    extract_issue_numbers(value)
        .into_iter()
        .next()
        .or_else(|| {
            value
                .split_once(':')
                .and_then(|(_, tail)| tail.trim().parse::<u64>().ok())
        })
}

fn parse_csv_numbers(value: &str) -> Vec<u64> {
    value
        .split(',')
        .filter_map(|part| part.trim().trim_start_matches('#').parse::<u64>().ok())
        .collect()
}

fn resolve_token() -> Option<String> {
    for key in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(token) = env::var(key) {
            let token = token.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!token.is_empty()).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_repository_without_owner() {
        assert!(GithubClient::new("burncloud").is_err());
    }

    #[test]
    fn parses_machine_tree_metadata() {
        let relation = parse_tree_relation(
            "<!-- burncloud-issue-tree\nparent: 12\ndepends_on: 9,10\nrequired: false\n-->",
        );
        assert_eq!(relation.parent, Some(12));
        assert_eq!(relation.depends_on, vec![9, 10]);
        assert!(!relation.required);
    }

    #[test]
    fn parses_dependency_section_for_legacy_issues() {
        let relation = parse_tree_relation("## 依赖\n- #21\n- depends on #22\n\n## 测试\n- #99");
        assert_eq!(relation.depends_on, vec![21, 22]);
    }
}
