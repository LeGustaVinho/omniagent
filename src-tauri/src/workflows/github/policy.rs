use std::{collections::BTreeSet, path::PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    protection,
    repository::{
        github::{GithubCli, RepositorySlug, RequiredCheck, validate_branch},
        registered_checkout_head,
    },
    store::Store,
    workflows::digest,
};

use super::GithubDeliveryClient;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubSettings {
    pub account_id: u64,
    pub repository_id: u64,
    pub owner: String,
    pub repository: String,
    pub remote: String,
    pub destination_branch: String,
    pub required_checks: Vec<RequiredCheck>,
    #[serde(default)]
    pub allow_automatic_merge: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_risk_paths: Option<Vec<String>>,
}

impl GithubSettings {
    pub(super) fn slug(&self) -> RepositorySlug {
        RepositorySlug {
            owner: self.owner.clone(),
            name: self.repository.clone(),
        }
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.account_id > 0 && self.repository_id > 0,
            "Pin the GitHub account and repository IDs"
        );
        let component = |value: &str| {
            !value.is_empty()
                && value.len() <= 100
                && !value.starts_with('-')
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
                && value != "."
                && value != ".."
        };
        self.slug().validate()?;
        ensure!(
            component(&self.remote),
            "GitHub needs a configured remote name"
        );
        validate_branch(&self.destination_branch)?;
        let mut checks = BTreeSet::new();
        ensure!(
            (1..=64).contains(&self.required_checks.len())
                && self
                    .required_checks
                    .iter()
                    .all(|check| !check.context.trim().is_empty()
                        && check.context == check.context.trim()
                        && check.context.len() <= 240
                        && !check.context.chars().any(char::is_control)
                        && check.app_id.is_none_or(|id| id > 0)
                        && checks.insert((&check.context, check.app_id))),
            "Configure distinct required GitHub checks"
        );
        if let Some(paths) = &self.high_risk_paths {
            let mut seen = BTreeSet::new();
            ensure!(
                paths.len() <= 128
                    && paths
                        .iter()
                        .all(|path| valid_path_rule(path) && seen.insert(path)),
                "High-risk paths need distinct relative file, directory-prefix or extension rules"
            );
        }
        Ok(())
    }

    pub(super) fn requires_human(&self, paths: &[String], review_high_risk: bool) -> bool {
        review_high_risk
            || self.high_risk_paths.as_ref().is_none_or(|rules| {
                rules.is_empty()
                    || paths.iter().any(|path| {
                        rules.iter().any(|rule| {
                            if let Some(suffix) = rule.strip_prefix('*') {
                                path.to_lowercase().ends_with(&suffix.to_lowercase())
                            } else if rule.ends_with('/') {
                                path.to_lowercase().starts_with(&rule.to_lowercase())
                            } else {
                                path.to_lowercase() == rule.to_lowercase()
                            }
                        })
                    })
            })
    }
}

fn valid_path_rule(rule: &str) -> bool {
    let value = if let Some(suffix) = rule.strip_prefix("*.") {
        if suffix.contains('/') {
            return false;
        }
        suffix
    } else {
        rule.strip_suffix('/').unwrap_or(rule)
    };
    !value.is_empty()
        && value.len() <= 512
        && !value.contains(|character: char| {
            character.is_control() || matches!(character, ':' | '\\' | '*' | '?')
        })
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
}

pub(super) fn load_settings(
    db: &Connection,
    project: &str,
) -> Result<Option<(String, GithubSettings)>> {
    let row = db
        .query_row(
            "SELECT revision, payload FROM workflow_github_project_settings WHERE project_id = ?1",
            [project],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    row.map(|(revision, payload)| {
        Ok((
            revision,
            serde_json::from_slice(&protection::unprotect(&payload)?)?,
        ))
    })
    .transpose()
}

impl Store {
    pub(crate) async fn resolve_workflow_github_base(
        &self,
        project: String,
        checkout: String,
        destination: String,
    ) -> Result<Option<String>> {
        let Some(base) = self
            .prepare_workflow_github_base(&project, &checkout, &destination)
            .await?
        else {
            return Ok(None);
        };
        self.fetch_workflow_github_base(
            project,
            checkout,
            destination,
            base,
            &GithubCli::discover()?,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn resolve_workflow_github_base_with_client(
        &self,
        project: String,
        checkout: String,
        destination: String,
        gh: &impl GithubDeliveryClient,
    ) -> Result<Option<String>> {
        let Some(base) = self
            .prepare_workflow_github_base(&project, &checkout, &destination)
            .await?
        else {
            return Ok(None);
        };
        self.fetch_workflow_github_base(project, checkout, destination, base, gh)
            .await
    }

    async fn prepare_workflow_github_base(
        &self,
        project: &str,
        checkout: &str,
        destination: &str,
    ) -> Result<Option<(String, GithubSettings, PathBuf)>> {
        let scope = (
            project.to_owned(),
            checkout.to_owned(),
            destination.to_owned(),
        );
        self.execute(move |db| {
            let (project, checkout, destination) = scope;
            let Some((revision, settings)) = load_settings(db, &project)? else {
                return Ok(None);
            };
            ensure!(
                destination == settings.destination_branch,
                "The delivery and GitHub destinations differ"
            );
            Ok(Some((
                revision,
                settings,
                checkout_path(db, &project, &checkout)?,
            )))
        })
        .await
    }

    async fn fetch_workflow_github_base(
        &self,
        project: String,
        checkout: String,
        destination: String,
        (revision, settings, path): (String, GithubSettings, PathBuf),
        gh: &impl GithubDeliveryClient,
    ) -> Result<Option<String>> {
        authorize(gh, &settings, &path).await?;
        let slug = settings.slug();
        let observed = gh.branch_head(&slug, &destination).await?;
        gh.fetch_branch(&slug, &destination, &observed, &path)
            .await?;
        ensure!(
            gh.branch_head(&slug, &destination).await? == observed,
            "The GitHub destination moved while fetching the delivery base"
        );
        self.execute(move |db| {
            let _ = checkout_path(db, &project, &checkout)?;
            require_settings(db, &project, &revision)?;
            Ok(Some(observed))
        })
        .await
    }

    pub async fn workflow_github_settings(
        &self,
        project: String,
    ) -> Result<Option<GithubSettings>> {
        self.execute(move |db| Ok(load_settings(db, &project)?.map(|(_, settings)| settings)))
            .await
    }

    pub async fn set_workflow_github_settings(
        &self,
        project: String,
        settings: GithubSettings,
    ) -> Result<String> {
        settings.validate()?;
        let bytes = serde_json::to_vec(&settings)?;
        let revision = digest(&bytes);
        let payload = protection::protect(&bytes)?;
        self.execute(move |db| {
            db.execute(
                "INSERT INTO workflow_github_project_settings VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id) DO UPDATE SET revision = excluded.revision, payload = excluded.payload",
                params![project, revision, payload],
            )?;
            Ok(revision)
        }).await
    }
}

pub(super) fn checkout_path(db: &Connection, project: &str, checkout: &str) -> Result<PathBuf> {
    registered_checkout_head(db, project, checkout)?;
    Ok(db
        .query_row(
            "SELECT root FROM checkouts WHERE project_id = ?1 AND id = ?2",
            params![project, checkout],
            |row| row.get::<_, String>(0),
        )?
        .into())
}

pub(super) fn require_settings(
    db: &Connection,
    project: &str,
    revision: &str,
) -> Result<GithubSettings> {
    let (active, settings) =
        load_settings(db, project)?.context("Configure the project's GitHub delivery policy")?;
    ensure!(active == revision, "The GitHub delivery policy changed");
    Ok(settings)
}

pub(super) async fn authorize(
    gh: &impl GithubDeliveryClient,
    settings: &GithubSettings,
    path: &std::path::Path,
) -> Result<()> {
    let slug = settings.slug();
    gh.validate_remote(&slug, &settings.remote, path).await?;
    ensure!(
        gh.account().await?.id == settings.account_id,
        "The active GitHub account differs from the project setting"
    );
    ensure!(
        gh.repository(&slug).await?.id == settings.repository_id,
        "The GitHub repository identity changed"
    );
    Ok(())
}

pub(crate) fn verified_destination(
    db: &Connection,
    project: &str,
    branch: &str,
) -> Result<Option<String>> {
    let Some((_, settings)) = load_settings(db, project)? else {
        return Ok(None);
    };
    if settings.destination_branch != branch {
        return Ok(None);
    }
    Ok(db.query_row(
        "SELECT observed_tip FROM workflow_github_destinations WHERE project_id = ?1 AND repository_id = ?2 AND branch = ?3",
        params![project, settings.repository_id.to_string(), branch], |row| row.get(0),
    ).optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_high_risk_policy_requires_a_decision_for_missing_rules_and_escalations() -> Result<()>
    {
        let mut settings = GithubSettings {
            account_id: 1,
            repository_id: 2,
            owner: "example".into(),
            repository: "fixture".into(),
            remote: "origin".into(),
            destination_branch: "main".into(),
            required_checks: vec![RequiredCheck {
                context: "build".into(),
                app_id: Some(12),
            }],
            allow_automatic_merge: true,
            high_risk_paths: None,
        };
        settings.validate()?;
        assert!(settings.requires_human(&["ordinary.rs".into()], false));
        settings.high_risk_paths = Some(Vec::new());
        assert!(settings.requires_human(&["ordinary.rs".into()], false));
        settings.high_risk_paths =
            Some(vec![".github/".into(), "Cargo.lock".into(), "*.pem".into()]);
        settings.validate()?;
        assert!(!settings.requires_human(&["ordinary.rs".into()], false));
        assert!(settings.requires_human(&["ordinary.rs".into()], true));
        for path in [".github/workflows/ci.yml", "cargo.lock", "certs/test.PEM"] {
            assert!(settings.requires_human(&[path.into()], false));
        }
        Ok(())
    }
}
