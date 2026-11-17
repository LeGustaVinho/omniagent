use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};
use serde::Serialize;

use crate::{
    process::TerminalOutcome,
    repository::{checkout_items::reveal_local_file, git_path},
    store::Store,
};

use super::{ConfiguredExecutable, Provider, command_probe, published_version};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliPrerequisite {
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPrerequisite {
    pub provider: Provider,
    pub configured_executable: Option<String>,
    pub status: CliPrerequisite,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopCliPrerequisites {
    pub git: CliPrerequisite,
    pub providers: Vec<ProviderPrerequisite>,
}

impl Store {
    pub async fn desktop_cli_prerequisites(&self) -> Result<DesktopCliPrerequisites> {
        let git = tokio::task::spawn_blocking(|| {
            inspect(
                "Git for Windows",
                git_path().and_then(dunce::canonicalize),
                vec!["--version".into()],
                git_version,
            )
        });
        let mut checks = Vec::with_capacity(Provider::ALL.len());
        for provider in Provider::ALL {
            let configured = self.configured_executable(provider).await?;
            checks.push(tokio::task::spawn_blocking(move || {
                inspect_provider(configured)
            }));
        }
        let mut providers = Vec::with_capacity(checks.len());
        for check in checks {
            providers.push(check.await?);
        }
        Ok(DesktopCliPrerequisites {
            git: git.await?,
            providers,
        })
    }

    pub async fn reveal_desktop_cli_executable(
        &self,
        provider: Option<Provider>,
        expected: PathBuf,
    ) -> Result<()> {
        let configured = match provider {
            Some(provider) => Some(self.configured_executable(provider).await?),
            None => None,
        };
        tokio::task::spawn_blocking(move || {
            let executable = match configured {
                Some(configured) => configured
                    .provider
                    .adapter()
                    .discover(configured.executable.as_deref().map(Path::new))?,
                None => dunce::canonicalize(git_path()?)?,
            };
            ensure!(
                executable == expected,
                "The executable location changed. Refresh the installation details."
            );
            reveal_local_file(&executable)
        })
        .await?
    }
}

fn inspect_provider(configured: ConfiguredExecutable) -> ProviderPrerequisite {
    let provider = configured.provider;
    let adapter = provider.adapter();
    let status = inspect(
        provider.executable(),
        adapter.discover(configured.executable.as_deref().map(Path::new)),
        adapter.version_arguments(),
        |output| published_version(provider, output),
    );
    ProviderPrerequisite {
        provider,
        configured_executable: configured.executable,
        status,
    }
}

fn inspect(
    label: &str,
    discovered: io::Result<PathBuf>,
    arguments: Vec<OsString>,
    version: impl Fn(&str) -> Option<String>,
) -> CliPrerequisite {
    let executable = match discovered {
        Ok(path) => path,
        Err(error) => {
            return CliPrerequisite {
                executable: None,
                version: None,
                issue: Some(error.to_string()),
            };
        }
    };
    // Installation discovery has no session, credential probe, or project directory.
    let detected = tempfile::tempdir()
        .ok()
        .and_then(|neutral| command_probe(&executable, neutral.path(), arguments).ok())
        .filter(|output| output.status == TerminalOutcome::Exited(0))
        .and_then(|output| version(&output.stdout).or_else(|| version(&output.stderr)));
    let issue = detected
        .is_none()
        .then(|| format!("{label} was found but did not report a recognized version"));
    CliPrerequisite {
        executable: Some(executable),
        version: detected,
        issue,
    }
}

fn git_version(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let version = line.trim().strip_prefix("git version ")?;
        (version.len() <= 80
            && version.as_bytes().first().is_some_and(u8::is_ascii_digit)
            && version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte)))
        .then(|| version.into())
    })
}
