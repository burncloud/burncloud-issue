use std::{env, process::Command, time::Duration};

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};

use crate::models::{DuplicateCandidate, IssueDraft};

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
}

impl GithubClient {
    pub fn new(repository: impl Into<String>) -> Result<Self> {
        let repository = repository.into();
        if repository.split('/').count() != 2 {
            return Err(anyhow!("仓库必须使用 owner/name 格式: {repository}"));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("burncloud-issue/0.1")
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

    pub fn search_duplicates(&self, title: &str) -> Result<Vec<DuplicateCandidate>> {
        if title.trim().is_empty() {
            return Ok(Vec::new());
        }
        let query = format!("repo:{} is:issue in:title {}", self.repository, title.trim());
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
        });
        let response = self.auth(request).send().context("create GitHub issue")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(anyhow!("创建 GitHub Issue 失败 {status}: {body}"));
        }
        response.json().context("decode created GitHub issue")
    }

    fn auth(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some(token) = &self.token {
            request.bearer_auth(token)
        } else {
            request
        }
    }
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
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
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
}
