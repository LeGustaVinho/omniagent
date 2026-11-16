use crate::{
    conversations::{AttemptPlan, SessionScope},
    dispatch::{
        ContextSnapshot, ConversationSelection, Delivery, NativeHistory, SelectedMessageSpan,
    },
    jobs::{AdmissionEnvironment, JobRole},
    mcp_bridge::{RunBridge, invoke_test_mcp_tool},
    model_catalog::ModelSelection,
    providers::{Provider, SessionMode, questions::QuestionRoute},
    repository::ProjectCheckout,
    runs::RunStatus,
    workflows::{
        BuiltinNode, NodeConfiguration, NodeInstance, WorkflowGraph,
        authoring::package::{
            PackageArtifactBody, PackageDestination, PackageMember, WorkflowPackage,
        },
        role_templates::{ConversationTemplate, SaveConversationTemplate, TemplateReviewPolicy},
        tests::approved_delivery_repository,
    },
};

use super::*;

#[tokio::test]
async fn proposals_apply_only_scoped_grants_and_review_mixed_fields_atomically() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("settings.sqlite");
    let store = Store::open(path.clone())?;
    let repo = approved_delivery_repository(&store, root.path()).await?;
    let project = repo.project_id.clone();
    let SaveConversationTemplate::Saved { detail } = store
        .save_conversation_template(
            project.clone(),
            None,
            ConversationTemplate {
                name: "Canonical role definition".into(),
                role: JobRole::Agent,
                model: ModelSelection {
                    provider: Provider::Codex,
                    profile_id: "profile".into(),
                    model_id: "model".into(),
                },
                effort: None,
                instructions: "Read the selected inputs.".into(),
                tool_scope: SessionMode::ReadOnly,
                completion_contract: "Summarize findings.".into(),
                review_policy: TemplateReviewPolicy::Independent,
                requirements: Default::default(),
            },
        )
        .await?
    else {
        anyhow::bail!("The fixture template was not saved");
    };
    let template = detail.revision.template_id;
    let role_revision = detail.revision.version.revision;
    let timeout = |expected_revision, minutes| SettingsChange::LiveQuestionTimeout {
        expected_revision,
        minutes,
    };
    let label = |expected_revision, value: &str| SettingsChange::TemplateDisplayLabel {
        template_id: template.clone(),
        template_revision: role_revision.clone(),
        expected_revision,
        label: Some(value.into()),
    };
    let patch = |request_id: &str, changes| SettingsPatch {
        request_id: request_id.into(),
        changes,
    };
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            WorkflowPackage {
                name: "Unpublished pipeline draft".into(),
                members: vec![PackageMember {
                    key: "pipeline".into(),
                    name: "Test pipeline".into(),
                    destination: PackageDestination::Project,
                    base: None,
                    body: PackageArtifactBody::Pipeline {
                        graph: WorkflowGraph {
                            name: "Test pipeline".into(),
                            nodes: vec![NodeInstance {
                                id: "start".into(),
                                kind: BuiltinNode::Start,
                                configuration: NodeConfiguration::None,
                            }],
                            wires: vec![],
                        },
                    },
                    references: Default::default(),
                }],
            },
        )
        .await?;

    let policy = store
        .set_settings_patch_allowlist(
            project.clone(),
            0,
            BTreeSet::from([SettingsAutoGrant {
                field: SettingsField::LiveQuestionTimeout,
                checkout_id: Some(repo.checkout_id.clone()),
                template_id: None,
            }]),
        )
        .await?;
    assert_eq!(policy.revision, 1);
    let plan = create_attempt(&store, &repo).await?;
    let bridge = RunBridge::start(&store, plan.run.id.clone()).await?;
    let run_scope = store
        .execute({
            let run = plan.run.id.clone();
            move |db| read_active_scope(db, &run)?.context("The fixture attempt is unavailable")
        })
        .await?;
    let automatic = patch("automatic", vec![timeout(0, 18)]);
    assert!(
        store
            .propose_settings_patch(run_scope.clone(), automatic.clone())
            .await
            .is_err()
    );
    let dispatch = submit_dispatch(&store, &plan).await?;
    let catalog =
        invoke_test_mcp_tool(&bridge, "read_editable_settings", serde_json::json!({})).await?;
    let catalog: SettingsPatchCatalog = serde_json::from_value(
        catalog["result"]["structuredContent"].clone(),
    )
    .with_context(|| format!("The MCP helper did not return the scoped catalog: {catalog}"))?;
    assert_eq!(
        catalog.live_question_timeout,
        RevisionedSetting {
            revision: 0,
            value: 10
        }
    );
    assert_eq!(catalog.templates[0].label.template_id, template);
    assert_eq!(catalog.templates[0].label.template_revision, role_revision);
    let applied = invoke_test_mcp_tool(
        &bridge,
        "propose_settings_patch",
        serde_json::to_value(&automatic)?,
    )
    .await?;
    let applied: SettingsProposal = serde_json::from_value(
        applied["result"]["structuredContent"].clone(),
    )
    .with_context(|| format!("The MCP helper did not apply the scoped timeout grant: {applied}"))?;
    assert_eq!(applied.actor.run_id, plan.run.id);
    assert_eq!(applied.actor.dispatch_id, dispatch);
    assert_eq!(applied.allowlist_revision, policy.revision);
    let SettingsProposalDecision::Applied {
        source,
        allowlist_revision,
        revisions,
        ..
    } = &applied.decision
    else {
        anyhow::bail!("The scoped timeout grant did not apply");
    };
    assert_eq!(*source, SettingsDecisionSource::UserAllowlist);
    assert_eq!(*allowlist_revision, 1);
    assert_eq!(revisions[0].before_revision, 0);
    assert_eq!(revisions[0].after_revision, 1);
    let bypass = invoke_test_mcp_tool(&bridge, "propose_settings_patch", serde_json::json!({
        "requestId":"do-not-publish",
        "changes":[{"field":"publish_workflow_package","draftId":draft.id,"expectedRevision":draft.revision}]
    })).await?;
    assert!(bypass["error"].is_object(), "{bypass}");
    let direct = invoke_test_mcp_tool(
        &bridge,
        "publish_workflow_package",
        serde_json::json!({
            "draftId":draft.id,"expectedRevision":draft.revision
        }),
    )
    .await?;
    assert!(direct["error"].is_object(), "{direct}");
    assert_eq!(
        store
            .propose_settings_patch(run_scope.clone(), automatic)
            .await?,
        applied
    );
    assert!(
        store
            .propose_settings_patch(run_scope.clone(), patch("automatic", vec![timeout(1, 19)]))
            .await
            .is_err()
    );
    assert_eq!(
        store
            .settings_patch_question_timeout(project.clone())
            .await?
            .value,
        18
    );

    let stale = store
        .propose_settings_patch(
            run_scope.clone(),
            patch(
                "mixed-stale",
                vec![timeout(1, 20), label(0, "Proposed alias")],
            ),
        )
        .await?;
    assert_eq!(stale.decision, SettingsProposalDecision::Pending);
    assert_eq!(
        store
            .settings_patch_question_timeout(project.clone())
            .await?
            .value,
        18
    );
    assert_eq!(
        store
            .template_display_label(project.clone(), template.clone())
            .await?
            .display_label
            .value,
        None
    );
    assert!(
        store
            .review_settings_proposal(
                "different-project".into(),
                stale.id.clone(),
                stale.review_revision.clone(),
                true
            )
            .await
            .is_err()
    );
    assert!(
        store
            .review_settings_proposal(
                project.clone(),
                stale.id.clone(),
                "different-revision".into(),
                true
            )
            .await
            .is_err()
    );
    let edited = store
        .set_template_display_label(
            project.clone(),
            template.clone(),
            role_revision.clone(),
            0,
            Some("Operator alias".into()),
        )
        .await?;
    assert_eq!(edited.display_label.revision, 1);
    assert!(
        store
            .review_settings_proposal(project.clone(), stale.id, stale.review_revision, true)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .settings_patch_question_timeout(project.clone())
            .await?,
        RevisionedSetting {
            value: 18,
            revision: 1
        }
    );

    let pending = store
        .propose_settings_patch(
            run_scope.clone(),
            patch(
                "mixed-current",
                vec![timeout(1, 25), label(1, "Reviewed alias")],
            ),
        )
        .await?;
    assert_eq!(pending.decision, SettingsProposalDecision::Pending);
    store
        .finish_agent_attempt(plan.run.id, RunStatus::Succeeded, None)
        .await?;
    assert!(
        store
            .propose_settings_patch(run_scope, patch("late", vec![timeout(1, 50)]))
            .await
            .is_err()
    );
    drop(store);

    let store = Store::open(path)?;
    let confirmed = store
        .review_settings_proposal(project.clone(), pending.id, pending.review_revision, true)
        .await?;
    let SettingsProposalDecision::Applied {
        source, revisions, ..
    } = confirmed.decision
    else {
        anyhow::bail!("The confirmed patch did not apply");
    };
    assert_eq!(source, SettingsDecisionSource::DesktopUser);
    assert_eq!(revisions.len(), 2);
    assert!(
        revisions
            .iter()
            .all(|revision| revision.before_revision == 1 && revision.after_revision == 2)
    );
    assert_eq!(
        store
            .settings_patch_question_timeout(project.clone())
            .await?
            .value,
        25
    );
    let renamed = store
        .template_display_label(project.clone(), template)
        .await?;
    assert_eq!(renamed.template_revision, role_revision);
    assert_eq!(
        renamed.display_label.value.as_deref(),
        Some("Reviewed alias")
    );
    assert!(
        store
            .workflow_package(project.clone(), draft.id)
            .await?
            .publication
            .is_none()
    );
    store
        .set_project_question_timeout(project.clone(), 27)
        .await?;
    assert_eq!(
        store
            .settings_patch_question_timeout(project.clone())
            .await?,
        RevisionedSetting {
            value: 27,
            revision: 3
        }
    );
    assert_eq!(store.settings_proposals(project, 0).await?.len(), 3);
    Ok(())
}

#[test]
fn the_patch_wire_type_has_no_publish_policy_or_permission_controls() {
    for field in [
        "publish_workflow_package",
        "execution_grant",
        "allowlist",
        "release_rule",
        "updater_trust",
    ] {
        let patch = serde_json::json!({"requestId":"rejected", "changes":[{"field":field,"expectedRevision":0}]});
        assert!(
            serde_json::from_value::<SettingsPatch>(patch).is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
async fn a_read_only_summary_cannot_use_a_settings_grant_for_its_checkout() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = Store::open(root.path().join("summary-settings.sqlite"))?;
    let repo = approved_delivery_repository(&store, root.path()).await?;
    let project = repo.project_id.clone();
    store
        .set_settings_patch_allowlist(
            project.clone(),
            0,
            BTreeSet::from([SettingsAutoGrant {
                field: SettingsField::LiveQuestionTimeout,
                checkout_id: Some(repo.checkout_id.clone()),
                template_id: None,
            }]),
        )
        .await?;
    let source = create_attempt(&store, &repo).await?;
    submit_dispatch(&store, &source).await?;
    store
        .finish_agent_attempt(source.run.id, RunStatus::Succeeded, None)
        .await?;
    store
        .enqueue_conversation_summary(
            ConversationSelection {
                conversation_id: source.conversation_id,
                context_id: source.context_id,
                spans: vec![SelectedMessageSpan {
                    revision_id: source.input[0].revision_id.clone(),
                    byte_range: None,
                }],
            },
            source.scope.clone(),
        )
        .await?;
    let summary = store
        .claim_next_job(AdmissionEnvironment {
            runner: source.scope.runner,
            bridge_available: true,
        })
        .await?
        .context("The summary job did not reach the scheduler")?;
    let bridge = RunBridge::start(&store, summary.run.id.clone()).await?;
    submit_dispatch(&store, &summary).await?;
    let response = invoke_test_mcp_tool(
        &bridge,
        "propose_settings_patch",
        serde_json::json!({
            "requestId":"summary-attempt",
            "changes":[{"field":"live_question_timeout","expectedRevision":0,"minutes":25}]
        }),
    )
    .await?;
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("does not have settings authority")),
        "{response}"
    );
    assert_eq!(
        store
            .settings_patch_question_timeout(project.clone())
            .await?
            .revision,
        0
    );
    assert!(store.settings_proposals(project, 0).await?.is_empty());
    Ok(())
}

async fn create_attempt(store: &Store, repo: &ProjectCheckout) -> Result<AttemptPlan> {
    let checkout = std::path::PathBuf::from(&repo.checkout_root);
    let executable = checkout.join("codex.exe");
    std::fs::write(&executable, b"settings fixture")?;
    let chat = store
        .create_conversation(repo.checkout_id.clone(), "Settings fixture".into())
        .await?;
    store
        .prepare_agent_attempt(
            chat.id,
            chat.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "profile".into(),
                account_binding: None,
                executable,
                checkout,
                cli_version: "fixture".into(),
                runner: "settings-fixture".into(),
                permission_revision: "read-only".into(),
                mode: SessionMode::ReadOnly,
                model: Some("model".into()),
                effort: None,
            },
            "Read eligible settings".into(),
        )
        .await
}

async fn submit_dispatch(store: &Store, plan: &AttemptPlan) -> Result<String> {
    store.mark_agent_running(plan.run.id.clone()).await?;
    let snapshot = ContextSnapshot {
        version: 13,
        context_id: plan.context_id.clone(),
        scope: plan.scope.clone(),
        session_id: None,
        job_policy_revision: None,
        role_context: None,
        base_commit: None,
        submitted_messages: plan.input.clone(),
        conversation_references: Vec::new(),
        native_history: NativeHistory::Fresh,
        capabilities: None,
        filesystem_skills: Vec::new(),
        reported_skills: None,
        instructions: None,
        scoped_launch: None,
        requested_skill: None,
        plugin_inventory: None,
        attachments: Vec::new(),
        task_context: None,
        repository_markdown: None,
        question_route: QuestionRoute::Native,
        answered_question: None,
    };
    let dispatch = store
        .prepare_dispatch(
            plan.run.id.clone(),
            snapshot,
            vec![serde_json::to_vec(&plan.input[0].text)?],
        )
        .await?;
    store
        .settle_dispatch(dispatch.clone(), Delivery::Submitted)
        .await?;
    Ok(dispatch)
}
