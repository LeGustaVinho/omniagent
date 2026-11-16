use std::{path::Path, process::Command};

use super::*;

async fn register(store: &Store, directory: &Path) -> Result<repository::ProjectCheckout> {
    std::fs::create_dir(directory)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(directory)
            .status()?
            .success(),
        "Could not initialize the fixture repository"
    );
    repository::register(store, directory.into()).await
}

fn template() -> ConversationTemplate {
    ConversationTemplate {
        name: "Repository review".into(),
        role: JobRole::Agent,
        model: ModelSelection {
            provider: Provider::Codex,
            profile_id: "desktop-direct:codex".into(),
            model_id: "gpt-6-luna".into(),
        },
        effort: Some("high".into()),
        instructions: "Keep source metadata private and cite immutable inputs.".into(),
        tool_scope: SessionMode::ReadOnly,
        completion_contract: "Provide links to tests and review results.".into(),
        review_policy: TemplateReviewPolicy::CrossReview,
        requirements: Default::default(),
    }
}

#[tokio::test]
async fn conversation_template_revisions_are_protected_project_scoped_and_immutable() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("private.sqlite");
    let store = Store::open(database.clone())?;
    let first_project = register(&store, &temp.path().join("first")).await?;
    let second_project = register(&store, &temp.path().join("second")).await?;
    let project_id = first_project.project_id.clone();
    assert!(
        store
            .validate_conversation_template_checkout(project_id.clone(), first_project.checkout_id)
            .await
            .is_ok()
    );
    assert!(
        store
            .validate_conversation_template_checkout(project_id.clone(), second_project.checkout_id)
            .await
            .is_err()
    );
    let source = template();
    let SaveConversationTemplate::Saved { detail: initial } = store
        .save_conversation_template(project_id.clone(), None, source.clone())
        .await?
    else {
        anyhow::bail!("Expected the initial revision")
    };
    assert_eq!(initial.revision.version.ordinal, 1);
    let template_id = initial.revision.template_id.clone();
    let original_revision = initial.revision.version.revision.clone();
    let base = || {
        Some(TemplateBase {
            template_id: template_id.clone(),
            expected_revision: original_revision.clone(),
        })
    };
    let changed = ConversationTemplate {
        instructions: "And cite the immutable revision of each finding.".into(),
        review_policy: TemplateReviewPolicy::HumanApproval,
        tool_scope: SessionMode::ApprovedWrite,
        ..source.clone()
    };
    let losing_edit = ConversationTemplate {
        instructions: "Competing author's instructions.".into(),
        ..source.clone()
    };
    let (one, two) = tokio::join!(
        store.save_conversation_template(project_id.clone(), base(), changed.clone()),
        store.save_conversation_template(project_id.clone(), base(), losing_edit.clone()),
    );
    let (saved, blocked, winner) = match (one?, two?) {
        (
            SaveConversationTemplate::Saved { detail },
            SaveConversationTemplate::Conflict { current },
        ) => (detail, current, changed),
        (
            SaveConversationTemplate::Conflict { current },
            SaveConversationTemplate::Saved { detail },
        ) => (detail, current, losing_edit),
        _ => anyhow::bail!("Exactly one expected revision should advance the head"),
    };
    assert_eq!(saved.revision.template, winner);
    assert_eq!(saved.revision.version.ordinal, 2);
    assert_eq!(
        saved.revision.version.base_revision.as_deref(),
        Some(original_revision.as_str())
    );
    assert_eq!(blocked.version, saved.revision.version);
    assert_eq!(saved.history.len(), 2);
    assert!(
        store
            .list_conversation_templates(second_project.project_id.clone())
            .await?
            .is_empty()
    );
    assert!(
        store
            .conversation_template_revision(
                second_project.project_id.clone(),
                template_id.clone(),
                original_revision.clone()
            )
            .await
            .is_err()
    );
    assert!(
        store
            .save_conversation_template(second_project.project_id, base(), source.clone())
            .await
            .is_err()
    );
    let raw = store
        .execute({
            let revision = original_revision.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT payload FROM conversation_template_revisions WHERE revision = ?1",
                    [revision],
                    |row| row.get::<_, Vec<u8>>(0),
                )?)
            }
        })
        .await?;
    let plaintext = source.instructions.as_bytes();
    assert!(
        !raw.windows(plaintext.len())
            .any(|window| window == plaintext)
    );
    drop(store);
    let store = Store::open(database)?;
    let old = store
        .conversation_template_revision(project_id.clone(), template_id.clone(), original_revision)
        .await?;
    assert_eq!(old.revision.template, source);
    let heads = store
        .list_conversation_templates(project_id.clone())
        .await?;
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].version, saved.revision.version);
    let exact = store
        .conversation_template_revision(project_id, template_id, saved.revision.version.revision)
        .await?;
    assert_eq!(exact.revision.template, winner);
    Ok(())
}

#[tokio::test]
async fn conversation_template_profile_reports_the_actual_checkout_and_read_only_blocker()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("local.sqlite"))?;
    let checkout = register(&store, &temp.path().join("repository")).await?;
    let template = template();
    let scope = SessionScope {
        provider: template.model.provider,
        profile_id: template.model.profile_id,
        account_binding: None,
        checkout: checkout.checkout_root.into(),
        executable: std::env::current_exe()?,
        cli_version: "fixture".into(),
        runner: DirectWindowsRunner::IDENTITY.into(),
        permission_revision: "fixture".into(),
        mode: template.tool_scope,
        model: Some(template.model.model_id),
        effort: template.effort,
    };
    let matrix = store
        .conversation_template_profile(checkout.checkout_id, scope)
        .await?;
    assert_eq!(matrix.cli_version, "fixture");
    assert_eq!(matrix.transport, Transport::CodexAppServer);
    assert!(
        matrix
            .tool_scopes
            .iter()
            .any(|entry| entry.mode == SessionMode::ReadOnly && entry.blocker.is_none())
    );
    assert!(
        matrix
            .capabilities
            .iter()
            .any(|entry| entry.capability == Capability::WorkspaceWrite
                && entry.status == CapabilityStatus::Blocked)
    );
    assert!(matrix.capabilities.iter().any(|entry| entry.capability
        == Capability::ManualNativeCompaction
        && entry.status == CapabilityStatus::Unverified));
    Ok(())
}
