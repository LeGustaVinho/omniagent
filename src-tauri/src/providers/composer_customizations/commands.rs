use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;

use crate::runner::{DirectWindowsRunner, Runner};

use super::{
    COMMAND_PREFIX, ComposerAvailability, ComposerCustomization, ComposerRoute, MARKER, Provider,
    SelectedComposerSkill, SessionScope, bounded_write, pinned_parts, revision, run_probe,
};
use crate::providers::{ProviderEventData, SessionIntent, SessionRequest};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ComposerCommand {
    pub(crate) name: String,
    revision: String,
}

impl ComposerCommand {
    pub(crate) fn from_advertisement(value: &Value) -> Option<Self> {
        let name = value.get("name")?.as_str()?;
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        {
            return None;
        }
        Some(Self {
            name: name.into(),
            revision: revision(value).ok()?,
        })
    }

    fn composable(&self) -> bool {
        // These controls have separately managed routes or alter the provider's
        // session history out of band from the OmniAgent transcript.
        !matches!(
            self.name.to_ascii_lowercase().as_str(),
            "model"
                | "plan"
                | "compact"
                | "goal"
                | "btw"
                | "clear"
                | "reset"
                | "fork"
                | "resume"
                | "rewind"
                | "permissions"
        )
    }

    pub(super) fn option(&self, scope: &SessionScope) -> Result<ComposerCustomization> {
        let id = revision(&(&scope.provider, "native_command", &self.name))?;
        let pin = revision(&(scope, &id, &self.revision))?;
        Ok(ComposerCustomization {
            marker: format!("{COMMAND_PREFIX}{id}:{pin}"),
            skill: SelectedComposerSkill {
                id,
                provider: scope.provider,
                name: self.name.clone(),
                path: PathBuf::new(),
                namespace: None,
                revision: self.revision.clone(),
            },
            availability: ComposerAvailability::Provider,
            route: ComposerRoute::NativeCommand,
            unavailable_reason: None,
        })
    }
}

pub(crate) fn command_parts(token: &str) -> Option<(&str, &str)> {
    pinned_parts(token.strip_prefix(COMMAND_PREFIX)?)
}

pub(crate) fn resolve_command(
    scope: &SessionScope,
    text: &str,
    advertised: Option<&[ComposerCommand]>,
) -> Result<Option<String>> {
    let mut markers = MARKER
        .find_iter(text)
        .map(|token| token.as_str())
        .filter(|token| command_parts(token).is_some());
    let Some(token) = markers.next() else {
        return Ok(None);
    };
    ensure!(
        markers.next().is_none(),
        "Select one native command per turn"
    );
    ensure!(
        scope.provider != Provider::Codex,
        "The selected command belongs to a different CLI profile"
    );
    let commands =
        advertised.context("The CLI did not advertise the selected command for this session")?;
    for command in commands.iter().filter(|command| usable(command, commands)) {
        if command.option(scope)?.marker == token {
            return Ok(Some(command.name.clone()));
        }
    }
    anyhow::bail!("The selected command or CLI profile changed. Select the command again.")
}

fn usable(command: &ComposerCommand, commands: &[ComposerCommand]) -> bool {
    command.composable()
        && commands
            .iter()
            .filter(|item| item.name == command.name)
            .count()
            == 1
}

pub(super) fn probe_commands(scope: &SessionScope) -> Result<Vec<ComposerCommand>> {
    if scope.runner != DirectWindowsRunner.identity() || !scope.executable.is_file() {
        return Ok(Vec::new());
    }
    let neutral = tempfile::tempdir()?;
    let cwd = dunce::canonicalize(neutral.path())?;
    let mut session = scope.provider.adapter().create_session(SessionRequest {
        checkout: cwd.clone(),
        intent: SessionIntent::Start,
        mode: scope.mode,
        model: scope.model.clone(),
        effort: scope.effort.clone(),
    })?;
    run_probe(
        |request| DirectWindowsRunner.launch(request),
        &scope.executable,
        &cwd,
        &session.arguments(),
        Duration::from_secs(6),
        |mut process, deadline| {
            bounded_write(&mut process, session.start()?.outbound)?;
            loop {
                let frame = process
                    .output
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))??;
                let update = session.receive(frame)?;
                ensure!(
                    !update
                        .events
                        .iter()
                        .any(|event| matches!(event.data, ProviderEventData::Error { .. })),
                    "The CLI did not advertise commands"
                );
                bounded_write(&mut process, update.outbound)?;
                if let Some(commands) = session.available_composer_commands() {
                    return Ok(commands
                        .iter()
                        .filter(|command| usable(command, commands))
                        .cloned()
                        .collect());
                }
            }
        },
    )
}
