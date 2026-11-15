use std::{path::PathBuf, process::Command, sync::Arc};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::SessionScope,
    providers::{
        Provider, ProviderEventData, SessionMode, TurnOutcome,
        profiles::{ConfiguredExecutable, probe_cli},
        runtime::{start_managed_turn, start_managed_turn_with_native_suggestions},
        suggestions::{PromptSuggestion, SuggestionOutcome},
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
};

async fn suggest(
    provider: Provider,
    model: &str,
    enabled: bool,
) -> Result<Option<PromptSuggestion>> {
    let root = tempfile::tempdir()?;
    let checkout = root.path().join("checkout");
    std::fs::create_dir(&checkout)?;
    let checkout = dunce::canonicalize(checkout)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    let cli = probe_cli(ConfiguredExecutable {
        provider,
        executable: None,
    })?;
    let scope = SessionScope {
        provider,
        profile_id: "suggestions-fixture".into(),
        account_binding: Some("authenticated-vendor-fixture".into()),
        checkout: checkout.clone(),
        executable: PathBuf::from(cli.executable.context("missing executable")?),
        cli_version: cli.version.context("missing CLI version")?,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "suggestions-disposable-fixture".into(),
        mode: if provider == Provider::Claude {
            SessionMode::ApprovedWrite
        } else {
            SessionMode::ReadOnly
        },
        model: Some(model.into()),
        effort: None,
    };
    let store = Store::open(root.path().join("suggestions.sqlite"))?;
    let project = repository::register(&store, checkout).await?;
    let chat = store
        .create_conversation(project.checkout_id, "Suggestion fixture".into())
        .await?;
    let attempt = store.prepare_agent_attempt(
        chat.id.clone(), chat.context_id, scope,
        "We need a SQLite-backed job scheduler. Compare two ways to claim jobs atomically. Recommend one and ask which option I want to use. Do not run tools.".into(),
    ).await?;
    let message_id = attempt
        .input
        .last()
        .context("missing submitted message")?
        .message_id
        .clone();
    let run = attempt.run.id.clone();
    let worker = if enabled {
        start_managed_turn_with_native_suggestions(
            store.clone(),
            Arc::new(DirectWindowsRunner),
            attempt,
            None,
        )
    } else {
        start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), attempt, None)
    };
    assert_eq!(worker.wait().await?, RunStatus::Succeeded);
    let mut after = 0;
    let mut terminal = None;
    let mut prediction = None;
    loop {
        let page = store
            .agent_observations(chat.id.clone(), run.clone(), after)
            .await?;
        if page.is_empty() {
            break;
        }
        for row in page {
            after = row.seq;
            match row.event.data {
                ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                } => terminal = Some(row.event.location),
                ProviderEventData::PromptSuggestion { suggestion } => {
                    let source = terminal
                        .as_ref()
                        .context("suggestion emitted before turn terminal")?;
                    assert_eq!(suggestion.source.message_id, message_id);
                    assert_eq!(
                        Some(&suggestion.source.provider_session_id),
                        source.session_id.as_ref()
                    );
                    assert_eq!(suggestion.source.provider_turn_id, source.turn_id);
                    assert!(prediction.replace(suggestion).is_none());
                }
                _ => {}
            }
        }
    }
    Ok(prediction)
}

#[tokio::test]
#[ignore = "requires authenticated Claude Code Haiku 5.5 and prompt suggestions enabled for this account"]
async fn claude_optionally_records_a_suggestion_after_the_main_result() -> Result<()> {
    let suggestion = suggest(Provider::Claude, "claude-haiku-5-5", true)
        .await?
        .context("missing Claude suggestion outcome")?;
    assert!(suggestion.source.provider_result_id.is_some());
    assert!(
        matches!(suggestion.outcome, SuggestionOutcome::Available { text } if !text.is_empty())
    );
    assert!(
        suggest(Provider::Claude, "claude-haiku-5-5", false)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Codex App Server 0.169.0-alpha.11.1 with gpt-6-luna"]
async fn codex_reports_the_deployed_prediction_method_as_unavailable() -> Result<()> {
    let result = suggest(Provider::Codex, "gpt-6-luna", true)
        .await?
        .context("no Codex prediction settlement")?;
    assert!(result.source.provider_turn_id.is_some());
    assert_eq!(result.outcome, SuggestionOutcome::Unavailable);
    Ok(())
}
