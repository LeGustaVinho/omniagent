use std::{ffi::OsString, io, path::Path, time::Instant};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{
    conversations::SessionScope,
    process::TerminalOutcome,
    process_io::{CapturedProcess, Stream},
    providers::{Provider, WireMessage, capabilities::revision, runtime::bounded_write},
    runner::LaunchRequest,
};

use super::{
    CommandOutput, DESKTOP_PERMISSION_REVISION, MAX_PROBE_OUTPUT, PROBE_TIME,
    command_probe_with_limits, run_probe,
};

pub(crate) fn verify_account_binding(
    scope: &SessionScope,
    launch: impl FnOnce(LaunchRequest<'_>) -> io::Result<CapturedProcess>,
) -> Result<()> {
    let Some(expected) = scope.account_binding.as_deref() else {
        return Ok(());
    };
    if scope.permission_revision != DESKTOP_PERMISSION_REVISION {
        return Ok(());
    }
    let neutral = tempfile::tempdir()?;
    let cwd = dunce::canonicalize(neutral.path())?;
    let actual = match scope.provider {
        Provider::Codex => codex_binding(launch, &scope.executable, &cwd),
        Provider::Claude => {
            let output = command_probe_with_limits(
                launch,
                &scope.executable,
                &cwd,
                scope.provider.adapter().authentication_probe().arguments,
                PROBE_TIME,
                MAX_PROBE_OUTPUT,
            );
            output.and_then(|output| claude_binding(&output))
        }
        Provider::Antigravity => Ok(None),
    };
    ensure!(
        actual.ok().flatten().as_deref() == Some(expected),
        "The configured CLI account changed or its identity is unavailable"
    );
    Ok(())
}

pub(super) fn claude_binding(output: &CommandOutput) -> Result<Option<String>> {
    fn account(value: &Value) -> Option<(&str, &str)> {
        if value["loggedIn"] != true
            || value["authMethod"] != "claude.ai"
            || value["apiProvider"] != "firstParty"
        {
            return None;
        }
        Some((
            identity_field(&value["email"])?,
            identity_field(&value["orgId"])?,
        ))
    }
    if output.status != TerminalOutcome::Exited(0) {
        return Ok(None);
    }
    for body in [&output.stdout, &output.stderr] {
        if let Ok(value) = serde_json::from_str::<Value>(body)
            && let Some((email, org)) = account(&value)
        {
            return Ok(Some(revision(&("claude-managed", email, org))?));
        }
    }
    Ok(None)
}

pub(super) fn codex_binding(
    launch: impl FnOnce(LaunchRequest<'_>) -> io::Result<CapturedProcess>,
    executable: &Path,
    cwd: &Path,
) -> Result<Option<String>> {
    Ok(codex_identity(launch, executable, cwd)?.0)
}

pub(super) fn codex_identity(
    launch: impl FnOnce(LaunchRequest<'_>) -> io::Result<CapturedProcess>,
    executable: &Path,
    cwd: &Path,
) -> Result<(Option<String>, bool)> {
    let args = ["app-server"].map(OsString::from);
    run_probe(
        launch,
        executable,
        cwd,
        &args,
        PROBE_TIME,
        |mut process, deadline| {
            bounded_write(
                &mut process,
                vec![WireMessage::json(&json!({
                    "id": 1, "method": "initialize",
                    "params": { "clientInfo": { "name": "omniagent-account-probe", "version": env!("CARGO_PKG_VERSION") } }
                }))?],
            )?;
            let result = response(&process, deadline, 1)?;
            if result.is_null() {
                return Ok((None, false));
            }
            bounded_write(
                &mut process,
                vec![
                    WireMessage::json(&json!({ "method": "initialized" }))?,
                    WireMessage::json(
                        &json!({ "id": 2, "method": "account/read", "params": { "refreshToken": false } }),
                    )?,
                ],
            )?;
            let account = response(&process, deadline, 2)?;
            let subscription =
                account["requiresOpenaiAuth"] == true && account["account"]["type"] == "chatgpt";
            Ok((codex_response_binding(&account)?, subscription))
        },
    )
}

fn response(process: &CapturedProcess, deadline: Instant, id: u32) -> Result<Value> {
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("The CLI account probe did not complete")??;
        if frame.stream != Stream::Stdout || frame.data.len() > MAX_PROBE_OUTPUT {
            continue;
        }
        if let Ok(mut value) = serde_json::from_str::<Value>(&frame.data)
            && value["id"] == id
        {
            return Ok(value["result"].take());
        }
    }
}

fn codex_response_binding(result: &Value) -> Result<Option<String>> {
    let account = &result["account"];
    if result["requiresOpenaiAuth"] != true || account["type"] != "chatgpt" {
        return Ok(None);
    }
    if !matches!(
        account["planType"].as_str(),
        Some("free" | "go" | "plus" | "pro" | "prolite" | "promax")
    ) || identity_field(&account["workspaceName"]).is_some()
    {
        // A workspace display name is mutable and need not be unique.
        return Ok(None);
    }
    identity_field(&account["email"])
        .map(|email| revision(&("codex-chatgpt-personal", email)))
        .transpose()
}

fn identity_field(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        providers::profiles::{ConfiguredExecutable, desktop_profile},
        runner::{DirectWindowsRunner, Runner},
    };

    #[test]
    fn account_evidence_keys_change_on_identifiable_account_switches() -> Result<()> {
        let claude = |org| {
            claude_binding(&CommandOutput {
                status: TerminalOutcome::Exited(0),
                stderr: String::new(),
                stdout: json!({
                    "loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty",
                    "email": "fixture@example.invalid", "orgId": org
                })
                .to_string(),
            })
        };
        let first = claude("first")?.unwrap();
        assert!(!first.contains('@'));
        assert_ne!(Some(first), claude("second")?);
        assert_eq!(None, claude("")?);
        let codex = |email, plan, workspace| {
            codex_response_binding(&json!({
                "requiresOpenaiAuth": true,
                "account": { "type": "chatgpt", "email": email, "planType": plan, "workspaceName": workspace }
            }))
        };
        let bound = codex("first@example.invalid", "plus", None)?.unwrap();
        assert_eq!(
            Some(bound.clone()),
            codex("first@example.invalid", "pro", None)?
        );
        assert_ne!(Some(bound), codex("second@example.invalid", "plus", None)?);
        assert_eq!(
            None,
            codex(
                "first@example.invalid",
                "business",
                Some("Same display name")
            )?
        );
        assert_eq!(
            None,
            codex_response_binding(
                &json!({"requiresOpenaiAuth": true, "account": {"type": "apiKey"}})
            )?
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires local Claude Code and Codex with identifiable signed-in accounts"]
    fn desktop_profiles_report_reproducible_account_bindings() -> Result<()> {
        for provider in [Provider::Codex, Provider::Claude] {
            let profile = desktop_profile(
                ConfiguredExecutable {
                    provider,
                    executable: None,
                },
                None,
            )?;
            let scope = SessionScope {
                provider,
                profile_id: "desktop-account-probe".into(),
                account_binding: Some(
                    profile
                        .account_binding
                        .context("No account identifier was reported")?,
                ),
                checkout: tempfile::tempdir()?.path().to_owned(),
                executable: profile.executable,
                cli_version: profile.version,
                runner: DirectWindowsRunner.identity().into(),
                permission_revision: DESKTOP_PERMISSION_REVISION.into(),
                mode: crate::providers::SessionMode::ReadOnly,
                model: None,
                effort: None,
            };
            verify_account_binding(&scope, |request| DirectWindowsRunner.launch(request))?;
            let wrong_scope = SessionScope {
                account_binding: Some("different-account".into()),
                ..scope
            };
            assert!(
                verify_account_binding(&wrong_scope, |request| DirectWindowsRunner.launch(request))
                    .is_err()
            );
        }
        Ok(())
    }
}
