use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    CiSnapshot, CiState, GithubCli, GithubTransport, PullRequest, RepositorySlug, RequiredCheck,
    Sha,
    ci::{commit_parents_match, same_revision},
    encoded, validate_branch, validate_sha,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteMergeProof {
    pub pull_number: u64,
    pub head_sha: String,
    pub base_sha: String,
    pub merge_sha: String,
    pub tree_sha: String,
    pub destination_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MergeOutcome {
    Pending { request_id: Option<String> },
    Verifiable(RemoteMergeProof),
    Rejected { reason: &'static str },
}

#[derive(Deserialize)]
struct ClassicProtection {
    required_status_checks: Option<ClassicStatusChecks>,
    enforce_admins: Option<AdminProtection>,
}

#[derive(Deserialize)]
struct AdminProtection {
    enabled: bool,
}

#[derive(Deserialize)]
struct ClassicStatusChecks {
    strict: bool,
    #[serde(default)]
    contexts: Vec<String>,
    #[serde(default)]
    checks: Vec<ClassicCheck>,
}

#[derive(Deserialize)]
struct ClassicCheck {
    context: String,
    app_id: Option<i64>,
}

#[derive(Deserialize)]
struct ActiveRule {
    #[serde(rename = "type")]
    rule_type: String,
    parameters: Option<Value>,
}

#[derive(Deserialize)]
struct RulesetChecks {
    strict_required_status_checks_policy: bool,
    required_status_checks: Vec<RulesetCheck>,
}

#[derive(Deserialize)]
struct RulesetCheck {
    context: String,
    integration_id: Option<u64>,
}

#[derive(Deserialize)]
struct AsyncMerge {
    status: String,
    details: AsyncMergeDetails,
}

#[derive(Deserialize)]
struct AsyncMergeDetails {
    uuid: Option<String>,
    merge_action: Option<String>,
    merge_method: Option<String>,
    expected_head_sha: Option<String>,
    #[serde(default)]
    bypass_rules: bool,
    sha: Option<String>,
}

impl<T: GithubTransport> GithubCli<T> {
    pub(crate) async fn merge_pull_request(
        &self,
        slug: &RepositorySlug,
        pull: &PullRequest,
        approved: &CiSnapshot,
    ) -> Result<MergeOutcome> {
        let approved_tree = approved.verify_for(pull)?;
        if let Some(proof) = self.verify_merge(slug, pull, approved).await? {
            return Ok(MergeOutcome::Verifiable(proof));
        }
        self.verify_protection(slug, &pull.base_branch, &approved.required_checks)
            .await?;
        ensure!(
            self.commit_in_history(slug, &pull.base_sha, &pull.head_sha)
                .await?,
            "The GitHub pull request head does not contain the approved destination revision"
        );
        let fresh = self
            .inspect_ci(slug, pull, &approved.required_checks)
            .await?;
        ensure!(
            fresh.state == CiState::Passed
                && fresh.tested_sha == approved.tested_sha
                && fresh.tested_tree.as_deref() == Some(approved_tree)
                && fresh.checks == approved.checks,
            "The passing GitHub CI evidence changed before merge"
        );
        let latest = self.pull_request(slug, pull.number).await?;
        ensure!(
            latest.open
                && !latest.draft
                && same_revision(&latest, pull)
                && self.branch_head(slug, &pull.base_branch).await? == pull.base_sha,
            "The GitHub pull request head or destination changed before merge"
        );
        let endpoint = slug.endpoint(&format!("/pulls/{}/merge-async", pull.number))?;
        let response = self
            .transport
            .request(
                "PUT",
                &endpoint,
                Some(json!({
                    "sha": pull.head_sha,
                    "merge_method": "merge",
                    "merge_action": "direct_merge",
                    "bypass_rules": false,
                })),
            )
            .await?;
        if response.status == 400 {
            return Ok(MergeOutcome::Rejected {
                reason: "The GitHub pull request was not ready for the protected merge",
            });
        }
        let conflicting = response.status == 409;
        let result: AsyncMerge = if conflicting {
            serde_json::from_value(response.body)
                .context("GitHub reported a conflicting merge request")?
        } else {
            response.decode()?
        };
        let outcome = self.merge_outcome(slug, pull, approved, result).await?;
        ensure!(
            !conflicting
                || matches!(
                    outcome,
                    MergeOutcome::Pending {
                        request_id: Some(_)
                    }
                ),
            "GitHub reported a conflicting merge request"
        );
        Ok(outcome)
    }

    pub(crate) async fn poll_merge(
        &self,
        slug: &RepositorySlug,
        pull: &PullRequest,
        approved: &CiSnapshot,
        request_id: &str,
    ) -> Result<MergeOutcome> {
        approved.verify_for(pull)?;
        validate_request_id(request_id)?;
        let endpoint =
            slug.endpoint(&format!("/pulls/{}/merge-async/{request_id}", pull.number))?;
        let response = self.transport.request("GET", &endpoint, None).await?;
        if response.status == 404 {
            let proof = self.verify_merge(slug, pull, approved).await?
                .context("GitHub no longer exposes the merge request and the approved merge could not be verified")?;
            return Ok(MergeOutcome::Verifiable(proof));
        }
        let result: AsyncMerge = response.decode()?;
        ensure!(
            result
                .details
                .uuid
                .as_deref()
                .is_none_or(|uuid| uuid == request_id),
            "GitHub returned a different merge request identifier"
        );
        let outcome = self.merge_outcome(slug, pull, approved, result).await?;
        match outcome {
            MergeOutcome::Pending { request_id: None } => Ok(MergeOutcome::Pending {
                request_id: Some(request_id.into()),
            }),
            outcome => Ok(outcome),
        }
    }

    async fn merge_outcome(
        &self,
        slug: &RepositorySlug,
        pull: &PullRequest,
        approved: &CiSnapshot,
        result: AsyncMerge,
    ) -> Result<MergeOutcome> {
        let details = result.details;
        match result.status.as_str() {
            "pending" => {
                let uuid = details
                    .uuid
                    .context("GitHub did not supply the asynchronous merge request identifier")?;
                validate_request_id(&uuid)?;
                ensure!(
                    details.expected_head_sha.as_deref() == Some(&pull.head_sha)
                        && details.merge_method.as_deref() == Some("merge")
                        && details.merge_action.as_deref() == Some("direct_merge")
                        && !details.bypass_rules,
                    "The GitHub merge request does not match the authorized head and protected merge method"
                );
                Ok(MergeOutcome::Pending {
                    request_id: Some(uuid),
                })
            }
            "merged" => {
                let sha = details
                    .sha
                    .context("GitHub did not supply the resulting merge SHA")?;
                validate_sha(&sha)?;
                match self.verify_merge(slug, pull, approved).await? {
                    Some(proof) => {
                        ensure!(
                            proof.merge_sha == sha,
                            "GitHub reported two different merge commits for the pull request"
                        );
                        Ok(MergeOutcome::Verifiable(proof))
                    }
                    None => Ok(MergeOutcome::Pending { request_id: None }),
                }
            }
            "enqueued" => Ok(MergeOutcome::Rejected {
                reason: "GitHub queued this pull request and has not performed the authorized direct merge",
            }),
            "failed" => Ok(MergeOutcome::Rejected {
                reason: "GitHub rejected the protected merge request",
            }),
            _ => anyhow::bail!("GitHub returned an unknown merge request status"),
        }
    }

    pub(crate) async fn verify_merge(
        &self,
        slug: &RepositorySlug,
        expected: &PullRequest,
        approved: &CiSnapshot,
    ) -> Result<Option<RemoteMergeProof>> {
        let tested_tree = approved.verify_for(expected)?;
        let current = self.pull_request(slug, expected.number).await?;
        ensure!(
            current.repository_id == expected.repository_id
                && current.id == expected.id
                && current.head_branch == expected.head_branch
                && current.head_sha == expected.head_sha
                && current.base_branch == expected.base_branch,
            "The GitHub pull request identity, refs or head changed before merge verification"
        );
        if !current.merged {
            ensure!(
                current.open && !current.draft && same_revision(&current, expected),
                "The open GitHub pull request no longer matches its approved revision"
            );
            return Ok(None);
        }
        let merge_sha = current
            .merge_commit_sha
            .context("GitHub reports a merge without a commit SHA")?;
        let merge = self.commit(slug, &merge_sha).await?;
        ensure!(
            commit_parents_match(&merge, expected) && merge.tree.sha == tested_tree,
            "The GitHub merge commit does not contain the approved parents and tested tree"
        );
        let destination_sha = self.branch_head(slug, &expected.base_branch).await?;
        ensure!(
            self.commit_in_history(slug, &merge_sha, &destination_sha)
                .await?,
            "The destination branch does not contain the verified GitHub merge commit"
        );
        Ok(Some(RemoteMergeProof {
            pull_number: current.number,
            head_sha: expected.head_sha.clone(),
            base_sha: expected.base_sha.clone(),
            merge_sha,
            tree_sha: merge.tree.sha,
            destination_sha,
        }))
    }

    async fn commit_in_history(
        &self,
        slug: &RepositorySlug,
        ancestor: &str,
        tip: &str,
    ) -> Result<bool> {
        #[derive(Deserialize)]
        struct Comparison {
            status: String,
            merge_base_commit: Sha,
        }

        validate_sha(ancestor)?;
        validate_sha(tip)?;
        if ancestor == tip {
            return Ok(true);
        }
        let endpoint = slug.endpoint(&format!("/compare/{ancestor}...{tip}?per_page=1"))?;
        let comparison: Comparison = self.get(&endpoint).await?;
        Ok(comparison.status == "ahead" && comparison.merge_base_commit.sha == ancestor)
    }

    async fn verify_protection(
        &self,
        slug: &RepositorySlug,
        branch: &str,
        approved: &[RequiredCheck],
    ) -> Result<()> {
        validate_branch(branch)?;
        let branch = encoded(branch);
        let endpoint = slug.endpoint(&format!("/rules/branches/{branch}"))?;
        let mut enforced = Vec::new();
        for page in self.pages(&endpoint).await? {
            for rule in serde_json::from_value::<Vec<ActiveRule>>(page)? {
                ensure!(
                    rule.rule_type != "merge_queue",
                    "The configured GitHub destination requires a merge queue"
                );
                if rule.rule_type != "required_status_checks" {
                    continue;
                }
                let config: RulesetChecks =
                    serde_json::from_value(rule.parameters.unwrap_or_default())?;
                if config.strict_required_status_checks_policy {
                    enforced.extend(config.required_status_checks.into_iter().map(|check| {
                        RequiredCheck {
                            context: check.context,
                            app_id: check.integration_id,
                        }
                    }));
                }
            }
        }
        if !covers(&enforced, approved) {
            let endpoint = slug.endpoint(&format!("/branches/{branch}/protection"))?;
            let response = self.transport.request("GET", &endpoint, None).await?;
            if response.status == 200 {
                let classic: ClassicProtection = response.decode()?;
                if let Some(checks) = classic
                    .required_status_checks
                    .filter(|checks| checks.strict)
                    && classic
                        .enforce_admins
                        .is_some_and(|protection| protection.enabled)
                {
                    let mut classic_checks: Vec<_> = checks
                        .checks
                        .into_iter()
                        .map(|check| RequiredCheck {
                            context: check.context,
                            app_id: check
                                .app_id
                                .and_then(|app| u64::try_from(app).ok())
                                .filter(|&app| app != 0),
                        })
                        .collect();
                    for context in checks.contexts {
                        if !classic_checks.iter().any(|check| check.context == context) {
                            classic_checks.push(RequiredCheck {
                                context,
                                app_id: None,
                            });
                        }
                    }
                    enforced.extend(classic_checks);
                }
            }
        }
        ensure!(
            covers(&enforced, approved),
            "Configure strict required status checks for the approved contexts and sources on the GitHub destination, with bypass disabled"
        );
        Ok(())
    }
}

fn covers(enforced: &[RequiredCheck], approved: &[RequiredCheck]) -> bool {
    approved.iter().all(|check| {
        enforced.iter().any(|rule| {
            check.context == rule.context && check.app_id.is_none_or(|app| rule.app_id == Some(app))
        })
    })
}

fn validate_request_id(uuid: &str) -> Result<()> {
    ensure!(
        uuid.len() == 36
            && uuid
                .bytes()
                .enumerate()
                .all(|(index, byte)| if [8, 13, 18, 23].contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }),
        "The GitHub asynchronous merge identifier is invalid"
    );
    Ok(())
}
