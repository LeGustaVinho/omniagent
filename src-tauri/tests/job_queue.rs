use std::{fs, path::Path, process::Command, time::Duration};

use anyhow::{Context, Result, ensure};
use omniagent::{
    attachments::{AttachmentDelivery, AttachmentSource},
    conversations::{AttemptPlan, Conversation, SessionScope},
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, AttemptSubagentRoute, InstructionDelivery, Job, JobContext,
        JobNoticeKind, JobOrigin, JobPermission, JobRequest, JobRequestConflict, JobRequirements,
        JobRole, JobRule, JobWorkspace, PipelineDispatch, PipelineNodeKind, ProjectSubagentPolicy,
        QueueHistoryPolicy, QueueSettings, QueueState, RecoveryAction, SubagentAuditField,
        SubagentAuditManagedReason, SubagentAuditPolicy, SubagentAuditPolicySource, SubagentAuditSource,
        SubagentCounts, SubagentLimitMode,
    },
    providers::{Capability, Provider, SessionIntent, SessionMode},
    repository,
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
};

#[path = "support/schema.rs"]
mod schema;

fn remove_recent_schema(db: &rusqlite::Connection) -> Result<()> {
    schema::restore_question_view_before_structured_planning(db)?;
    db.execute_batch(
        "DROP TABLE history_deleted_side_payloads;
         DROP TABLE history_deleted_candidate_diffs;
         DROP TABLE history_deleted_attempt_diffs;
         DROP TABLE history_deleted_dispatch_payloads;
         DROP TABLE history_deleted_artifacts;
         DROP TABLE history_deleted_revisions;
         DROP TABLE conversation_context_compactions;
         DROP TABLE conversation_compaction_requests;
         DROP TABLE project_conversation_compaction_settings;
         DROP TABLE conversation_fork_checkout_previews;
         DROP TABLE conversation_fork_checkout_policies;
         DROP TABLE project_fork_checkout_policies;
         DROP TABLE conversation_attachment_drafts;
         DROP TABLE pipeline_work_event_origins;
         DROP TABLE project_work_events;
         DROP TABLE message_conversation_references;
         DROP TABLE conversation_reference_drafts;
         DROP TABLE conversation_reference_previews;
         DROP TABLE conversation_history_journal;
         DROP TABLE workflow_destination_drift_updates;
         DROP TABLE project_destination_drift_policies;
         DROP TABLE task_delivery_milestone_pins;
         DROP TABLE project_delivery_milestone_policies;
         DROP TABLE delivery_merge_previews;
         DROP TABLE workflow_merge_intents;
         DROP TABLE workflow_verified_merges;
         DROP TABLE conversation_continuation_jobs;
         DROP TABLE conversation_continuation_previews;
         DROP TABLE conversation_continuation_policies;
         DROP TABLE project_continuation_policies;
         DROP TRIGGER conversation_library_create;
         DROP TRIGGER conversation_library_edit;
         DROP TRIGGER conversation_library_attempt;
         DROP TRIGGER conversation_library_run_state;
         DROP TABLE conversation_library_activity;
         DROP TABLE markdown_rename_previews;
         DROP TABLE checkout_note_links_indexed;
         DROP TABLE repository_note_link_resolutions;
         DROP TABLE repository_note_index;
         DROP TABLE repository_note_fts;
         DROP TABLE project_description_drafts;
         DROP TABLE history_revoked_sessions;
         DROP TABLE removed_dispatch_attachments;
         DROP TABLE removed_history_attachments;
         DROP TRIGGER remember_attachment_revision_copy;
         DROP VIEW attachment_revision_copy_candidates;
         DROP TABLE attachment_revision_copy_sources;
         DROP TABLE side_output_fixture_payloads;
         DROP TABLE generated_image_evidence_payloads;
         DROP TABLE goal_criterion_evidence;
         ALTER TABLE job_attempt_policies DROP COLUMN evidence;
         DROP TABLE attempt_subagent_audit_routing;
         DROP TABLE task_delivery_events;
         DROP TABLE verified_delivery_commits;
         DROP TABLE delivery_commit_intents;
         DROP TABLE project_delivery_git_policies;
         DROP TRIGGER workflow_job_must_match_pin;
         DROP TRIGGER workflow_plan_must_be_published;
         DROP TABLE workflow_job_inputs;
         DROP TABLE workflow_job_admissions;
         DROP TABLE workflow_delivery_units;
         DROP TABLE workflow_release_bindings;
         DROP TABLE workflow_delivery_heads;
         DROP TABLE workflow_approval_decisions;
         DROP TABLE workflow_host_completions;
         DROP TABLE workflow_artifacts;
         DROP TABLE workflow_node_pins;
         DROP TABLE workflow_run_pins;
         DROP TABLE workflow_revisions;
         DROP TABLE candidate_acceptances;
         DROP TABLE candidate_gate_results;
         DROP TABLE candidate_verifications;
         DROP TABLE active_unit_candidates;
         DROP TABLE selected_candidate_revisions;
         DROP TABLE candidate_gate_policies;
         DROP TABLE conversation_artifact_sources;
         DROP TABLE conversation_artifact_revisions;
         DROP TABLE conversation_summary_requests;
         DROP TABLE dispatch_lineage_messages;
         DROP TABLE dispatch_lineage;
         DROP TABLE message_revision_copy_sources;
         DROP TABLE task_branch_strategies;
         DROP TABLE project_branch_strategies;
         ALTER TABLE pending_worktree_changes DROP COLUMN discard_starting_head;
         DROP TABLE checklist_progress_events;
         DROP TABLE job_checklist_modes;
         DROP TABLE task_checklist_modes;
         DROP TABLE project_checklist_modes;
         DROP TABLE job_role_contexts;
         DROP TABLE project_context_review_events;
         DROP TABLE project_context_revisions;
         DROP TABLE project_context_records;
         DROP TABLE job_repository_markdown;
         DROP TABLE repository_markdown_revisions;
         DROP TABLE project_description_revisions;
         DROP VIEW task_runs;
         DROP TABLE job_task_run_scopes;
         DROP TABLE project_checklist_item_revisions;
         DROP TABLE project_checklist_items;
         DROP TABLE project_task_prerequisites;
         DROP TABLE project_task_details;
         DROP TABLE project_checklists;
         DROP TABLE project_task_memberships;
         DROP TABLE project_epics;
         DROP TABLE side_validation_evidence;
         DROP TRIGGER discard_suggestion_on_draft_insert;
         DROP TRIGGER discard_suggestion_on_draft_edit;
         DROP TRIGGER discard_suggestion_on_context_change;
         DROP TRIGGER retire_discarded_suggestion;
         DROP TABLE composer_suggestions_settings;
         DROP TABLE job_side_responses;
         DROP TABLE job_side_requests;",
    )?;
    Ok(())
}

async fn origin(store: &Store, path: &Path) -> Result<AttemptPlan> {
    origin_with_mode(store, path, SessionMode::ReadOnly).await
}

async fn origin_with_mode(store: &Store, path: &Path, mode: SessionMode) -> Result<AttemptPlan> {
    fs::create_dir(path)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(path)
            .status()?
            .success()
    );
    let project = repository::register(store, path.into()).await?;
    let chat = store
        .create_conversation(project.checkout_id, "Parent".into())
        .await?;
    let attempt = store
        .prepare_agent_attempt(
            chat.id,
            chat.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "test".into(),
                account_binding: None,
                checkout: dunce::canonicalize(path)?,
                executable: std::env::current_exe()?,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner.identity().into(),
                permission_revision: "read-only".into(),
                mode,
                model: None,
                effort: None,
            },
            "Plan the fixture".into(),
        )
        .await?;
    store.mark_agent_running(attempt.run.id.clone()).await?;
    Ok(attempt)
}

fn request() -> JobRequest {
    JobRequest {
        checkout_id: None,
        task_id: None,
        mode: SessionMode::Plan,
        instructions: "Use the confidential fixture canary ☕".into(),
        dependencies: Vec::new(),
        requires_bridge: false,
    }
}

#[tokio::test]
async fn selected_models_use_the_same_agent_admission_and_delegation_scope() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("queue.sqlite"))?;
    let parent = origin(&store, &temp.path().join("project")).await?;
    let project = parent.run.project_id.clone();
    let checkout = parent.run.checkout_id.clone();
    let mut scope = parent.scope.clone();
    scope.provider = Provider::Claude;
    scope.profile_id = "selected-claude-profile".into();
    scope.model = Some("selected-claude-model".into());
    scope.mode = SessionMode::Plan;
    let parent_run = Some(parent.run.id.clone());

    let raised = SessionScope {
        mode: SessionMode::ApprovedWrite,
        ..scope.clone()
    };
    let error = store
        .enqueue_selected_agent_job(
            project.clone(),
            checkout.clone(),
            parent_run.clone(),
            raised,
            "Escalate past parent".into(),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot delegate repository writes")
    );
    assert!(
        store
            .enqueue_selected_agent_job(
                "another-project".into(),
                checkout.clone(),
                parent_run.clone(),
                scope.clone(),
                "Cross-project child".into()
            )
            .await
            .is_err()
    );

    let child = store
        .enqueue_selected_agent_job(
            project.clone(),
            checkout.clone(),
            parent_run,
            scope.clone(),
            "Managed child prompt".into(),
        )
        .await?;
    assert_eq!(child.operator_requested, Some(true));
    let standalone = store
        .enqueue_selected_agent_job(
            project.clone(),
            checkout,
            None,
            scope.clone(),
            "Standalone job prompt".into(),
        )
        .await?;
    let env = environment(false);
    let pending = store
        .inspect_job(project.clone(), child.id.clone(), env.clone())
        .await?
        .context("Missing child")?;
    let approval = pending
        .blockers
        .iter()
        .find_map(|blocker| match blocker {
            AdmissionBlocker::Approval { approval_id } => Some(approval_id.clone()),
            _ => None,
        })
        .context("The selected child bypassed agent approval")?;
    let plan = store
        .claim_next_job(env.clone())
        .await?
        .context("Missing standalone job")?;
    assert!(plan.scope == scope);
    assert_eq!(plan.input.last().unwrap().text, "Standalone job prompt");
    assert_eq!(
        store
            .job(project.clone(), standalone.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .run_id,
        plan.run.id
    );
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    store
        .finish_agent_attempt(plan.run.id, RunStatus::Cancelled, None)
        .await?;
    store.decide_job_approval(project, approval, true).await?;
    let delegated = store
        .claim_next_job(env)
        .await?
        .context("Missing approved child")?;
    assert!(delegated.scope == scope);
    assert_eq!(delegated.input.last().unwrap().text, "Managed child prompt");
    assert_eq!(
        store
            .conversation_subagent_quota(parent.conversation_id)
            .await?
            .managed
            .active,
        1
    );
    Ok(())
}

#[tokio::test]
async fn queue_rejects_an_unsupported_os_sandbox_and_preserves_cli_only_work() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("queue.sqlite"))?;
    let initial = origin(&store, &temp.path().join("project")).await?;
    let chat = store.conversation(initial.conversation_id.clone()).await?;
    store
        .finish_agent_attempt(initial.run.id.clone(), RunStatus::Cancelled, None)
        .await?;
    store
        .set_project_job_rules(
            initial.run.project_id.clone(),
            vec![],
            vec![JobRule {
                role: JobRole::Operator,
                operation: SessionMode::ReadOnly,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: JobRequirements {
                    capabilities: vec![Capability::OsCommandSandbox],
                    ..Default::default()
                },
            }],
        )
        .await?;
    let queue = |chat: Conversation, mode: SessionMode| {
        let (store, mut scope) = (store.clone(), initial.scope.clone());
        scope.provider = Provider::Claude;
        scope.mode = mode;
        async move {
            let prompt = "Test Windows profile admission";
            store
                .save_conversation_draft(chat.id.clone(), prompt.into())
                .await?;
            store
                .enqueue_conversation_turn(
                    JobContext {
                        conversation_id: chat.id,
                        context_id: chat.context_id,
                    },
                    scope,
                    prompt.into(),
                )
                .await
        }
    };
    let rejected = queue(chat, SessionMode::ReadOnly).await?;
    let other_chat = store
        .create_conversation(
            initial.run.checkout_id,
            "Planning with CLI permissions".into(),
        )
        .await?;
    let admitted = queue(other_chat.clone(), SessionMode::Plan).await?;
    let env = environment(false);
    let inspection = store
        .inspect_job(rejected.project_id, rejected.id, env.clone())
        .await?
        .context("Missing sandbox inspection")?;
    assert!(inspection.blockers.iter().any(|blocker| matches!(blocker, AdmissionBlocker::Policy { reason } if reason.contains("OS command sandbox"))));
    let plan = store
        .claim_next_job(env.clone())
        .await?
        .context("Missing supported job")?;
    assert_eq!(plan.conversation_id, other_chat.id);
    assert_eq!(plan.scope.mode, SessionMode::Plan);
    assert_eq!(plan.run.project_id, admitted.project_id);
    assert!(store.claim_next_job(env).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn project_job_rules_match_the_role_operation_and_workspace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("queue.sqlite"))?;
    let parent = origin(&store, &temp.path().join("project")).await?;
    let other = origin(&store, &temp.path().join("other")).await?;
    let project = parent.run.project_id.clone();
    let tagged = store
        .create_project_task(project.clone(), "Rule scope".into())
        .await?;
    let task_allow = JobRule {
        role: JobRole::Agent,
        operation: SessionMode::Plan,
        workspace: JobWorkspace::Task {
            task_id: tagged.id.clone(),
        },
        permission: JobPermission::Allow,
        requirements: Default::default(),
    };
    let checkout_review = JobRule {
        role: JobRole::Agent,
        operation: SessionMode::Plan,
        workspace: JobWorkspace::Checkout {
            checkout_id: parent.run.checkout_id.clone(),
        },
        permission: JobPermission::Review,
        requirements: Default::default(),
    };
    let deny = JobRule {
        role: JobRole::Agent,
        operation: SessionMode::ReadOnly,
        workspace: JobWorkspace::Project,
        permission: JobPermission::Deny,
        requirements: Default::default(),
    };
    let rules = vec![task_allow.clone(), deny.clone()];
    assert!(
        store
            .set_project_job_rules(
                project.clone(),
                vec![],
                vec![JobRule {
                    workspace: JobWorkspace::Checkout {
                        checkout_id: other.run.checkout_id
                    },
                    ..deny.clone()
                }]
            )
            .await
            .is_err()
    );
    store
        .set_project_job_rules(project.clone(), vec![], rules.clone())
        .await?;
    let allowed = store
        .submit_agent_job(
            parent.run.id.clone(),
            "allowed".into(),
            JobRequest {
                task_id: Some(tagged.id.clone()),
                ..request()
            },
        )
        .await?;
    let review = store
        .submit_agent_job(parent.run.id.clone(), "review".into(), request())
        .await?;
    let denied = store
        .submit_agent_job(
            parent.run.id,
            "denied".into(),
            JobRequest {
                mode: SessionMode::ReadOnly,
                task_id: Some(tagged.id),
                ..request()
            },
        )
        .await?;
    let inspect =
        |job: &Job| store.inspect_job(project.clone(), job.id.clone(), environment(false));
    assert!(inspect(&allowed).await?.unwrap().blockers.is_empty());
    let approval_id = match &inspect(&review).await?.unwrap().blockers[..] {
        [AdmissionBlocker::Approval { approval_id }] => approval_id.clone(),
        blockers => anyhow::bail!("Unexpected review blockers {blockers:?}"),
    };
    assert!(matches!(
        &inspect(&denied).await?.unwrap().blockers[..],
        [AdmissionBlocker::Policy { .. }]
    ));
    assert!(
        store
            .project_job_rules(other.run.project_id)
            .await?
            .is_empty()
    );
    store
        .decide_job_approval(project.clone(), approval_id.clone(), true)
        .await?;
    let changed = vec![task_allow, checkout_review, deny];
    store
        .set_project_job_rules(project.clone(), rules, changed.clone())
        .await?;
    let pending = inspect(&review).await?.unwrap().blockers;
    assert!(
        matches!(&pending[..], [AdmissionBlocker::Approval { approval_id: new }] if new != &approval_id)
    );
    assert!(
        store
            .decide_job_approval(project.clone(), approval_id, true)
            .await
            .is_err()
    );
    let plan = store
        .claim_next_job(environment(false))
        .await?
        .context("Allowed job should start automatically")?;
    assert_eq!(
        store
            .job(allowed.project_id, allowed.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .run_id,
        plan.run.id
    );
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let admitted = store.run_orchestration(
        project.clone(), plan.run.id.clone(), Some(plan.conversation_id.clone()),
    ).await?.context("missing admitted rule evidence")?;
    let admitted = serde_json::to_value(admitted)?;
    assert_eq!(admitted["policy"]["evidence"]["permission"], "allow");
    assert_eq!(admitted["policy"]["evidence"]["source"]["workspace"]["type"], "task");
    let mut replacement = changed.clone();
    replacement[0].permission = JobPermission::Deny;
    store.set_project_job_rules(project.clone(), changed, replacement).await?;
    let historical = store.run_orchestration(
        project.clone(), plan.run.id.clone(), Some(plan.conversation_id),
    ).await?.context("the saved rule disappeared after a policy change")?;
    assert_eq!(serde_json::to_value(historical)?["policy"], admitted["policy"]);
    assert!(store.run_orchestration(project, plan.run.id, Some("unrelated".into())).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn queue_controls_review_and_interrupt_only_the_selected_project_attempt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("queue.sqlite"))?;
    let parent = origin(&store, &temp.path().join("project")).await?;
    let other = origin(&store, &temp.path().join("other")).await?;
    let job = store
        .submit_agent_job(parent.run.id, "queue-control".into(), request())
        .await?;
    let env = environment(false);
    let inspection = store
        .inspect_job(job.project_id.clone(), job.id.clone(), env.clone())
        .await?
        .context("queued job missing")?;
    let [AdmissionBlocker::Approval { approval_id }] = inspection.blockers.as_slice() else {
        anyhow::bail!("queued job did not surface its approval")
    };
    let review = store
        .pending_job_approval_request(job.project_id.clone(), approval_id.clone())
        .await?;
    assert_eq!(review.instructions, request().instructions);
    assert!(
        store
            .pending_job_approval_request(other.run.project_id.clone(), approval_id.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .decide_job_approval(other.run.project_id.clone(), approval_id.clone(), true)
            .await
            .is_err()
    );
    store
        .decide_job_approval(job.project_id.clone(), approval_id.clone(), true)
        .await?;
    assert!(
        store
            .pending_job_approval_request(job.project_id.clone(), approval_id.clone())
            .await
            .is_err()
    );

    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 7,
        })
        .await?;
    assert_eq!(
        store.set_job_queue_paused(true).await?,
        QueueSettings {
            paused: true,
            capacity: 7
        }
    );
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    store
        .set_pending_job_state(job.project_id.clone(), job.id.clone(), QueueState::Paused)
        .await?;
    assert_eq!(
        store.set_job_queue_paused(false).await?,
        QueueSettings {
            paused: false,
            capacity: 7
        }
    );
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    store
        .set_pending_job_state(job.project_id.clone(), job.id.clone(), QueueState::Queued)
        .await?;
    let plan = store
        .claim_next_job(env.clone())
        .await?
        .context("resumed job was not admitted")?;
    assert_ne!(plan.conversation_id, parent.conversation_id);
    let run = plan.run.id;
    store.mark_agent_running(run.clone()).await?;
    assert!(
        store
            .request_project_job_interrupt(other.run.project_id, job.id.clone(), run.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .request_project_job_interrupt(
                job.project_id.clone(),
                "another-job".into(),
                run.clone()
            )
            .await
            .is_err()
    );
    store
        .request_project_job_interrupt(job.project_id.clone(), job.id.clone(), run.clone())
        .await?;
    let current = store
        .inspect_job(job.project_id.clone(), job.id.clone(), env)
        .await?
        .context("admitted job missing")?
        .job
        .latest_attempt
        .context("attempt missing")?;
    assert_eq!(
        current.conversation_id.as_ref(),
        Some(&plan.conversation_id)
    );
    assert_eq!(current.run_id, run);
    assert!(current.cancel_requested);
    store
        .finish_agent_attempt(run.clone(), RunStatus::Cancelled, None)
        .await?;
    assert!(
        store
            .request_project_job_interrupt(job.project_id, job.id, run)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn native_job_notices_survive_reopen_and_skip_obsolete_approvals() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite");
    let store = Store::open(database.clone())?;
    let parent = origin(&store, &temp.path().join("repo")).await?;
    let mut notices = store.job_notices();
    let job = store
        .submit_agent_job(parent.run.id.clone(), "review".into(), request())
        .await?;
    assert_eq!(
        store
            .submit_agent_job(parent.run.id.clone(), "review".into(), request())
            .await?,
        job
    );
    let approval = tokio::time::timeout(Duration::from_secs(3), notices.next()).await??;
    assert_eq!(approval.kind, JobNoticeKind::JobApproval);
    assert_eq!(approval.project_id, job.project_id);
    assert_eq!(approval.job_id, job.id);
    assert!(approval.run_id.is_none());
    drop(notices);
    drop(store);

    let reopened = Store::open(database)?;
    let mut notices = reopened.job_notices();
    let delivered = tokio::time::timeout(Duration::from_secs(3), notices.next()).await??;
    assert_eq!(delivered, approval);
    notices.acknowledge(&delivered).await?;
    let obsolete = reopened
        .submit_agent_job(parent.run.id, "obsolete".into(), request())
        .await?;
    reopened
        .set_pending_job_state(obsolete.project_id, obsolete.id, QueueState::Cancelled)
        .await?;
    approve(&reopened, &job).await?;
    let plan = reopened
        .claim_next_job(environment(false))
        .await?
        .context("approved job was not admitted")?;
    reopened.mark_agent_running(plan.run.id.clone()).await?;
    reopened
        .finish_agent_attempt(plan.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    let completed = tokio::time::timeout(Duration::from_secs(3), notices.next()).await??;
    assert_eq!(completed.kind, JobNoticeKind::Succeeded);
    assert_eq!(completed.job_id, job.id);
    assert_eq!(completed.run_id, Some(plan.run.id));
    notices.acknowledge(&completed).await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), notices.next())
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_replays_resolve_to_the_same_persisted_job_and_conflict_on_changed_input()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("jobs.sqlite3");
    let primary = Store::open(database.clone())?;
    let secondary = Store::open(database.clone())?;
    let parent = origin(&primary, &temp.path().join("parent")).await?;
    let run = parent.run.id;
    let (first, replay) = tokio::join!(
        primary.submit_agent_job(run.clone(), "stable-request".into(), request()),
        secondary.submit_agent_job(run.clone(), "stable-request".into(), request()),
    );
    let job = first?;
    assert_eq!(job, replay?);
    assert_eq!(
        job.origin,
        JobOrigin::Agent {
            run_id: run.clone(),
            request_id: "stable-request".into()
        }
    );
    let context = job.context.as_ref().unwrap();
    assert_eq!(context.conversation_id, parent.conversation_id);
    assert_eq!(context.context_id, parent.context_id);
    assert_eq!(job.checkout_id, parent.run.checkout_id);
    assert!(job.latest_attempt.is_none());

    let error = primary
        .submit_agent_job(
            run.clone(),
            "stable-request".into(),
            JobRequest {
                instructions: "Different instructions".into(),
                ..request()
            },
        )
        .await
        .unwrap_err();
    assert!(error.is::<JobRequestConflict>());
    primary
        .set_pending_job_state(job.project_id.clone(), job.id.clone(), QueueState::Paused)
        .await?;
    primary
        .finish_agent_attempt(run.clone(), RunStatus::Cancelled, None)
        .await?;
    drop(primary);
    drop(secondary);

    let reopened = Store::open(database.clone())?;
    let saved = reopened
        .submit_agent_job(
            run.clone(),
            "stable-request".into(),
            JobRequest {
                checkout_id: Some(job.checkout_id.clone()),
                ..request()
            },
        )
        .await?;
    assert_eq!(saved.id, job.id);
    assert_eq!(saved.queue_state, QueueState::Paused);
    assert!(
        reopened
            .submit_agent_job(run, "new-request".into(), request())
            .await
            .is_err()
    );
    let raw = rusqlite::Connection::open(database)?;
    let (rows, blob): (i64, Vec<u8>) =
        raw.query_row("SELECT count(*), request FROM jobs", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    assert_eq!(rows, 1);
    assert!(
        !blob
            .windows(b"confidential fixture canary".len())
            .any(|window| window == b"confidential fixture canary")
    );
    Ok(())
}

#[tokio::test]
async fn job_trust_is_scoped_to_the_origin_project_and_writes_need_a_managed_worktree() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("jobs.sqlite3"))?;
    let first = origin(&store, &temp.path().join("first")).await?.run;
    let second = origin(&store, &temp.path().join("second")).await?.run;
    let task = store
        .create_project_task(first.project_id.clone(), "Investigate".into())
        .await?;
    let one = store
        .submit_agent_job(
            first.id.clone(),
            "same".into(),
            JobRequest {
                task_id: Some(task.id.clone()),
                ..request()
            },
        )
        .await?;
    let two = store
        .submit_agent_job(second.id.clone(), "same".into(), request())
        .await?;
    assert_ne!(one.id, two.id);
    assert!(
        store
            .job(first.project_id.clone(), two.id.clone())
            .await?
            .is_none()
    );
    for denied in [
        JobRequest {
            checkout_id: Some(second.checkout_id),
            ..request()
        },
        JobRequest {
            dependencies: vec![two.id],
            ..request()
        },
        JobRequest {
            mode: SessionMode::ApprovedWrite,
            ..request()
        },
        JobRequest {
            task_id: Some(task.id.clone()),
            mode: SessionMode::ApprovedWrite,
            ..request()
        },
    ] {
        assert!(
            store
                .submit_agent_job(first.id.clone(), "reusable".into(), denied)
                .await
                .is_err()
        );
    }
    assert!(
        store
            .submit_agent_job(
                second.id,
                "cross-task".into(),
                JobRequest {
                    task_id: Some(task.id),
                    ..request()
                }
            )
            .await
            .is_err()
    );
    let dependent = store
        .submit_agent_job(
            first.id.clone(),
            "reusable".into(),
            JobRequest {
                dependencies: vec![one.id.clone(), one.id.clone()],
                ..request()
            },
        )
        .await?;
    let normalized_replay = store
        .submit_agent_job(
            first.id,
            "reusable".into(),
            JobRequest {
                dependencies: vec![one.id],
                ..request()
            },
        )
        .await?;
    assert_eq!(dependent.id, normalized_replay.id);
    assert_eq!(dependent.queue_state, QueueState::Queued);
    Ok(())
}

fn environment(bridge_available: bool) -> AdmissionEnvironment {
    AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available,
    }
}

async fn approve(store: &Store, job: &Job) -> Result<()> {
    let pending = store
        .inspect_job(job.project_id.clone(), job.id.clone(), environment(false))
        .await?
        .context("missing job")?;
    let approval = pending
        .blockers
        .into_iter()
        .find_map(|blocker| match blocker {
            AdmissionBlocker::Approval { approval_id } => Some(approval_id),
            _ => None,
        })
        .context("missing linked approval")?;
    store
        .decide_job_approval(job.project_id.clone(), approval, true)
        .await
}

#[tokio::test]
async fn managed_quota_separates_forks_and_shares_capacity_with_nested_children() -> Result<()> {
    use omniagent::{
        dispatch::EventSource,
        providers::{ProviderEvent, ProviderEventData, ProviderLocation, TurnOutcome},
    };
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("quota.sqlite");
    let store = Store::open(path.clone())?;
    let seed = origin(&store, &temporary.path().join("project")).await?;
    store
        .record_agent_observation(
            seed.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    session_id: Some("completed-original".into()),
                    ..Default::default()
                },
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(
            seed.run.id,
            RunStatus::Succeeded,
            Some("Recorded fork boundary".into()),
        )
        .await?;
    let history = store
        .conversation_history(seed.conversation_id.clone())
        .await?;
    assert!(
        store
            .fork_conversation(
                seed.conversation_id.clone(),
                seed.context_id,
                None,
                "Stale head".into()
            )
            .await
            .is_err()
    );
    let prefix = store
        .fork_conversation(
            seed.conversation_id.clone(),
            history.context_id.clone(),
            Some(history.messages[0].revision_id.clone()),
            "Fork from prompt".into(),
        )
        .await?;
    let prefix_history = store.conversation_history(prefix.id).await?;
    assert_eq!(prefix_history.messages.len(), 1);
    assert_eq!(prefix_history.messages[0].text, history.messages[0].text);
    let second_chat = store
        .fork_conversation(
            seed.conversation_id.clone(),
            history.context_id.clone(),
            None,
            "Independent fork".into(),
        )
        .await?;
    assert_eq!(second_chat.checkout_id, seed.run.checkout_id);
    let fork_history = store.conversation_history(second_chat.id.clone()).await?;
    assert_eq!(fork_history.messages.len(), history.messages.len());
    for (original, copied) in history.messages.iter().zip(&fork_history.messages) {
        assert_eq!(
            (&original.role, &original.text),
            (&copied.role, &copied.text)
        );
        assert_ne!(original.revision_id, copied.revision_id);
    }
    let lineage: (String, String, String, String) = rusqlite::Connection::open(&path)?.query_row(
        "SELECT source_conversation_id, source_context_id, source_revision_id, source_session_id
         FROM conversation_forks WHERE conversation_id = ?1",
        [&second_chat.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(
        lineage,
        (
            seed.conversation_id.clone(),
            history.context_id.clone(),
            history.messages.last().unwrap().revision_id.clone(),
            seed.agent_session_id,
        )
    );
    let parent = store
        .prepare_agent_attempt(
            seed.conversation_id,
            history.context_id,
            seed.scope,
            "Original after the fork".into(),
        )
        .await?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    let project = &parent.run.project_id;
    let root = &parent.conversation_id;
    let second = store
        .prepare_agent_attempt(
            second_chat.id.clone(),
            second_chat.context_id,
            parent.scope.clone(),
            "Second conversation".into(),
        )
        .await?;
    assert!(matches!(second.intent, SessionIntent::Start));
    assert!(second.previous_run_id.is_none());
    assert!(
        second
            .input
            .iter()
            .any(|message| message.text == "Recorded fork boundary")
    );
    store.mark_agent_running(second.run.id.clone()).await?;
    store.set_job_queue_capacity(4).await?;
    store
        .set_project_subagent_policy(
            project.clone(),
            ProjectSubagentPolicy {
                mode: SubagentLimitMode::Managed,
                maximum: 1,
            },
        )
        .await?;
    let quota = store
        .set_conversation_subagent_quota(root.clone(), None, 8, None)
        .await?;
    assert_eq!((quota.requested, quota.authorized), (8, 1));
    let fork_quota = store
        .set_conversation_subagent_quota(second_chat.id.clone(), None, 2, None)
        .await?;
    assert_eq!((fork_quota.requested, fork_quota.authorized), (2, 1));

    let concurrent = Store::open(path.clone())?;
    let fork_connection = Store::open(path.clone())?;
    let (one, two, independent) = tokio::try_join!(
        store.submit_agent_job(parent.run.id.clone(), "first".into(), request()),
        concurrent.submit_agent_job(parent.run.id, "same-chat".into(), request()),
        fork_connection.submit_agent_job(second.run.id.clone(), "first".into(), request()),
    )?;
    assert_ne!(one.id, independent.id);
    for job in [&one, &two, &independent] {
        approve(&store, job).await?;
    }

    let (a, b) = tokio::join!(
        store.claim_next_job(environment(false)),
        concurrent.claim_next_job(environment(false)),
    );
    let first_claim = a?.context("First claim lost")?;
    let second_claim = b?.context("Independent conversation should have a slot")?;
    let one = store.job(project.clone(), one.id).await?.unwrap();
    let two = store.job(project.clone(), two.id).await?.unwrap();
    let (current, waiting) = match (one.latest_attempt, two.latest_attempt) {
        (Some(current), None) => (current, two.id),
        (None, Some(current)) => (current, one.id),
        _ => anyhow::bail!("Exactly one child should acquire the shared original quota"),
    };
    let other_run = store
        .job(project.clone(), independent.id.clone())
        .await?
        .unwrap()
        .latest_attempt
        .unwrap();
    assert!([&first_claim.run.id, &second_claim.run.id].contains(&&current.run_id));
    assert!([&first_claim.run.id, &second_claim.run.id].contains(&&other_run.run_id));
    assert!(
        store
            .job(project.clone(), waiting.clone())
            .await?
            .unwrap()
            .latest_attempt
            .is_none()
    );
    let counts = store
        .conversation_subagent_quota(root.clone())
        .await?
        .managed;
    assert_eq!(
        counts,
        SubagentCounts {
            active: 1,
            queued: 1,
            historical: 0
        }
    );
    assert_eq!(
        store
            .conversation_subagent_quota(second_chat.id.clone())
            .await?
            .managed
            .active,
        1
    );

    store.mark_agent_running(current.run_id.clone()).await?;
    store.mark_agent_running(other_run.run_id.clone()).await?;
    let grandchild = store
        .submit_agent_job(current.run_id.clone(), "nested".into(), request())
        .await?;
    approve(&store, &grandchild).await?;
    let blocked = store
        .inspect_job(project.clone(), grandchild.id.clone(), environment(false))
        .await?
        .unwrap();
    assert!(blocked.blockers.contains(&AdmissionBlocker::SubagentQuota {
        conversation_id: root.clone(),
        active: 1,
        limit: 1,
    }));
    store
        .set_project_subagent_policy(
            project.clone(),
            ProjectSubagentPolicy {
                mode: SubagentLimitMode::Managed,
                maximum: 0,
            },
        )
        .await?;
    assert_eq!(
        store
            .conversation_subagent_quota(root.clone())
            .await?
            .managed
            .active,
        1
    );
    let new_fork_job = store
        .submit_agent_job(second.run.id, "after-project-cap".into(), request())
        .await?;
    approve(&store, &new_fork_job).await?;
    let blocked = store
        .inspect_job(project.clone(), new_fork_job.id, environment(false))
        .await?
        .unwrap();
    assert_eq!(
        blocked.blockers,
        [AdmissionBlocker::SubagentQuota {
            conversation_id: second_chat.id,
            active: 1,
            limit: 0,
        }]
    );
    assert_eq!(
        store
            .job(project.clone(), independent.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .status,
        RunStatus::Running
    );
    store
        .finish_agent_attempt(current.run_id, RunStatus::Cancelled, None)
        .await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    assert_eq!(
        store
            .conversation_subagent_quota(root.clone())
            .await?
            .managed,
        SubagentCounts {
            active: 0,
            queued: 2,
            historical: 1,
        }
    );
    store
        .set_pending_job_state(project.clone(), waiting, QueueState::Paused)
        .await?;
    store
        .set_project_subagent_policy(
            project.clone(),
            ProjectSubagentPolicy {
                mode: SubagentLimitMode::Managed,
                maximum: 1,
            },
        )
        .await?;
    let nested = store
        .claim_next_job(environment(false))
        .await?
        .context("Released slot was not reused")?;
    assert_eq!(
        store
            .job(project.clone(), grandchild.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .run_id,
        nested.run.id
    );

    let strict = store
        .set_conversation_subagent_quota(root.clone(), Some(SubagentLimitMode::Strict), 8, None)
        .await?;
    assert_eq!(strict.authorized, 1);
    let restarted = Store::open(path)?;
    let saved = restarted.conversation_subagent_quota(root.clone()).await?;
    assert_eq!(saved.mode_override, Some(SubagentLimitMode::Strict));
    assert_eq!(saved.managed.active, 1);
    Ok(())
}

#[tokio::test]
async fn strict_quota_reserves_native_sessions_and_managed_children_in_one_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("strict.sqlite"))?;
    let initial = origin(&store, &temp.path().join("project")).await?;
    let chat = initial.conversation_id;
    let project = initial.run.project_id;
    let scope = initial.scope;
    let strict = Some(SubagentLimitMode::Strict);
    let pending = store
        .set_conversation_subagent_quota(chat.clone(), strict, 3, Some(1))
        .await?;
    assert!(pending.configuration_pending);
    store
        .finish_agent_attempt(initial.run.id, RunStatus::Cancelled, None)
        .await?;
    let current = store.conversation(chat.clone()).await?;
    let root = store
        .prepare_agent_attempt(
            chat.clone(),
            current.context_id,
            scope.clone(),
            "Capped session".into(),
        )
        .await?;
    store.mark_agent_running(root.run.id.clone()).await?;

    for (id, prompt) in [("first", "First fixture"), ("waiting", "Waiting fixture")] {
        let job = store
            .submit_agent_job(
                root.run.id.clone(),
                id.into(),
                JobRequest {
                    instructions: prompt.into(),
                    ..request()
                },
            )
            .await?;
        approve(&store, &job).await?;
    }
    let child = store
        .claim_next_job(environment(false))
        .await?
        .context("Native cap left one managed slot")?;
    let snapshot = store.conversation_subagent_quota(chat.clone()).await?;
    assert_eq!(snapshot.managed.active, 1);
    assert_eq!(snapshot.native_sessions.len(), 2);
    assert!(!snapshot.configuration_pending);
    for session in [&root.agent_session_id, &child.agent_session_id] {
        let reserved = snapshot
            .native_sessions
            .iter()
            .find(|native| &native.session_id == session)
            .unwrap();
        assert_eq!(reserved.allowance, 1);
        assert!(!reserved.verified);
    }
    assert!(store.claim_next_job(environment(false)).await?.is_none());

    let pending = store
        .set_conversation_subagent_quota(chat.clone(), strict, 2, Some(0))
        .await?;
    assert!(pending.configuration_pending);
    assert_eq!(
        pending
            .native_sessions
            .iter()
            .map(|s| s.allowance)
            .sum::<u32>(),
        2
    );
    store
        .finish_agent_attempt(child.run.id, RunStatus::Cancelled, None)
        .await?;
    store
        .finish_agent_attempt(root.run.id, RunStatus::Cancelled, None)
        .await?;

    let next = store
        .claim_next_job(environment(false))
        .await?
        .context("Retired reservation was not returned")?;
    assert_eq!(next.input.last().unwrap().text, "Waiting fixture");
    let updated = store.conversation_subagent_quota(chat.clone()).await?;
    assert!(!updated.configuration_pending);
    assert_eq!(updated.native_sessions.len(), 1);
    assert_eq!(updated.native_sessions[0].allowance, 0);
    let alternate = store
        .create_conversation(next.run.checkout_id, "Unsupported native provider".into())
        .await?;
    store
        .set_conversation_subagent_quota(alternate.id.clone(), strict, 2, Some(0))
        .await?;
    let prompt = "This test job must be blocked";
    store
        .save_conversation_draft(alternate.id.clone(), prompt.into())
        .await?;
    let blocked = store
        .enqueue_conversation_turn(
            JobContext {
                conversation_id: alternate.id,
                context_id: alternate.context_id,
            },
            SessionScope {
                provider: Provider::Antigravity,
                ..scope
            },
            prompt.into(),
        )
        .await?;
    let inspection = store
        .inspect_job(project, blocked.id, environment(false))
        .await?
        .unwrap();
    assert!(inspection.blockers.iter().any(|item| matches!(item,
        AdmissionBlocker::Policy { reason } if reason.contains("verified native delegation controls")
    )));
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn subagent_audit_overrides_are_independent_and_a_pipeline_stage_can_require_more()
-> Result<()> {
    use omniagent::{
        dispatch::EventSource,
        providers::{
            ChildDetails, ProviderEvent, ProviderEventData, ProviderLocation,
            questions::QuestionRoute,
        },
    };

    let temp = tempfile::tempdir()?;
    let database = temp.path().join("subagent-audit.sqlite");
    let store = Store::open(database.clone())?;
    let initial = origin(&store, &temp.path().join("project")).await?;
    let chat = initial.conversation_id;
    let project = initial.run.project_id;
    let scope = initial.scope;
    let checkout = initial.run.checkout_id;
    store
        .finish_agent_attempt(initial.run.id, RunStatus::Cancelled, None)
        .await?;
    let quota = store.conversation_subagent_quota(chat.clone()).await?;
    assert_eq!(
        store.project_subagent_audit(project.clone()).await?,
        SubagentAuditPolicy::Available
    );
    store
        .set_project_subagent_audit(project.clone(), SubagentAuditPolicy::Complete)
        .await?;
    store
        .set_conversation_subagent_audit(chat.clone(), Some(SubagentAuditPolicy::Available))
        .await?;
    let reopened = Store::open(database)?;
    let persisted = reopened.conversation_subagent_audit(chat.clone()).await?;
    assert_eq!(persisted.project_default, SubagentAuditPolicy::Complete);
    assert_eq!(
        persisted.conversation_override,
        Some(SubagentAuditPolicy::Available)
    );
    assert_eq!(
        reopened.conversation_subagent_quota(chat.clone()).await?,
        quota
    );

    let context = store.conversation(chat.clone()).await?;
    let unsupported = SessionScope {
        provider: Provider::Antigravity,
        ..scope.clone()
    };
    let inherited = store
        .create_conversation(checkout.clone(), "Inherited audit requirement".into())
        .await?;
    assert!(
        store
            .prepare_agent_attempt(
                inherited.id,
                inherited.context_id,
                unsupported.clone(),
                "Project audit applies here".into()
            )
            .await
            .is_err()
    );
    let baseline = store
        .prepare_agent_attempt(
            chat.clone(),
            context.context_id,
            unsupported.clone(),
            "Available evidence".into(),
        )
        .await?;
    let audit = reopened
        .attempt_subagent_audit(chat.clone(), baseline.run.id.clone())
        .await?
        .context("missing admission audit")?;
    assert_eq!(audit.effective_policy, SubagentAuditPolicy::Available);
    assert_eq!(audit.policy_source, Some(SubagentAuditPolicySource::ConversationOverride));
    assert_eq!(audit.route, AttemptSubagentRoute::Available);
    assert!(audit.required_fields.is_empty());
    store.mark_agent_running(baseline.run.id.clone()).await?;
    let observe_partial = |run: String| {
        store.record_agent_observation(
            run,
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    session_id: Some("fixture-session".into()),
                    ..Default::default()
                },
                data: ProviderEventData::ChildActivity {
                    child_id: Some("child".into()),
                    status: Some("completed".into()),
                    details: Some(ChildDetails {
                        prompt: Some("Child input".into()),
                        ..Default::default()
                    }),
                },
            },
        )
    };
    observe_partial(baseline.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            baseline.run.id.clone(),
            RunStatus::Succeeded,
            Some("Root response".into()),
        )
        .await?;
    assert_eq!(
        store
            .latest_conversation_run(chat.clone())
            .await?
            .unwrap()
            .status,
        RunStatus::Succeeded
    );
    let reports = store
        .agent_observations(chat.clone(), baseline.run.id, 0)
        .await?;
    assert!(reports.iter().any(|event| matches!(
        &event.event.data, ProviderEventData::SubagentAuditGap { report }
        if report.source == SubagentAuditSource::Native
            && report.missing == [SubagentAuditField::ContextProvenance, SubagentAuditField::Response]
    )));

    let pipeline = "audit-pipeline".to_owned();
    store
        .register_job_pipeline(project.clone(), pipeline.clone())
        .await?;
    store
        .register_job_pipeline_node_with_audit(
            project.clone(),
            pipeline.clone(),
            "audited".into(),
            PipelineNodeKind::Execution,
            SubagentAuditPolicy::Complete,
        )
        .await?;
    store
        .set_job_pipeline_plan(project.clone(), pipeline.clone(), None, "plan-a".into())
        .await?;
    let binding = PipelineDispatch {
        pipeline_run_id: pipeline,
        node_instance_id: "audited".into(),
        plan_revision: Some("plan-a".into()),
        release_id: None,
    };
    let context = JobContext {
        conversation_id: chat.clone(),
        context_id: store.conversation(chat.clone()).await?.context_id,
    };
    let request = || JobRequest {
        checkout_id: Some(checkout.clone()),
        mode: SessionMode::ReadOnly,
        ..request()
    };
    let blocked = store
        .submit_pinned_pipeline_agent_job(
            binding.clone(),
            unsupported,
            request(),
            context.clone(),
            QuestionRoute::Native,
        )
        .await?;
    let blocked = store
        .inspect_job(project.clone(), blocked.id, environment(false))
        .await?
        .unwrap();
    assert!(matches!(blocked.blockers.as_slice(),
        [AdmissionBlocker::SubagentAudit { missing }] if missing.len() == 4
    ));
    let stage = store
        .submit_pinned_pipeline_agent_job(binding, scope, request(), context, QuestionRoute::Native)
        .await?;
    let parent = store
        .claim_next_job(environment(false))
        .await?
        .context("Audit did not select managed-only fallback")?;
    let audit = reopened
        .attempt_subagent_audit(chat.clone(), parent.run.id.clone())
        .await?
        .context("missing workflow audit")?;
    assert_eq!(audit.effective_policy, SubagentAuditPolicy::Complete);
    assert_eq!(audit.policy_source, Some(SubagentAuditPolicySource::Workflow));
    assert_eq!(audit.required_fields.len(), 4);
    assert_eq!(audit.route, AttemptSubagentRoute::Managed {
        reason: Some(SubagentAuditManagedReason::NativeEvidenceUnavailable),
    });
    let quota = store.conversation_subagent_quota(chat.clone()).await?;
    assert_eq!(quota.native_sessions.len(), 1);
    assert_eq!(quota.native_sessions[0].allowance, 0);
    assert!(!quota.native_sessions[0].strict_total);
    assert!(!quota.configuration_pending);
    store.mark_agent_running(parent.run.id.clone()).await?;
    let delegated = store
        .submit_agent_job(parent.run.id.clone(), "owned-child".into(), request())
        .await?;
    approve(&store, &delegated).await?;
    let child = store
        .claim_next_job(environment(false))
        .await?
        .context("Audit-only reservation blocked managed delegation")?;
    let audit = reopened
        .attempt_subagent_audit(child.conversation_id.clone(), child.run.id.clone())
        .await?
        .context("missing child audit")?;
    assert_eq!(audit.policy_conversation_id, chat);
    assert_eq!(audit.policy_source, Some(SubagentAuditPolicySource::OriginatingAttempt));
    assert!(reopened.attempt_subagent_audit(chat.clone(), child.run.id.clone()).await?.is_none());
    store.mark_agent_running(child.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            child.run.id,
            RunStatus::Succeeded,
            Some("Missing exact dispatch".into()),
        )
        .await?;
    let failed_child = store
        .job(project.clone(), delegated.id)
        .await?
        .unwrap()
        .latest_attempt
        .unwrap();
    assert_eq!(failed_child.status, RunStatus::Failed);

    observe_partial(parent.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            parent.run.id.clone(),
            RunStatus::Succeeded,
            Some("Unexpected native delegation".into()),
        )
        .await?;
    assert_eq!(
        store
            .job(project, stage.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .status,
        RunStatus::Failed
    );
    assert!(store.agent_observations(chat.clone(), parent.run.id.clone(), 0).await?.iter().any(|event| matches!(
        &event.event.data, ProviderEventData::SubagentAuditGap { report } if report.native_delegation_detected
    )));
    store
        .set_conversation_subagent_audit(chat.clone(), Some(SubagentAuditPolicy::Complete))
        .await?;
    let historical = reopened
        .attempt_subagent_audit(chat, parent.run.id)
        .await?
        .context("historical audit disappeared after editing the conversation policy")?;
    assert_eq!(historical.policy_source, Some(SubagentAuditPolicySource::Workflow));
    Ok(())
}

#[tokio::test]
async fn native_subagent_observations_are_scoped_deduplicated_and_backfilled() -> Result<()> {
    use omniagent::{
        dispatch::EventSource,
        providers::{ProviderEvent, ProviderEventData},
    };

    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("old-evidence.sqlite");
    let store = Store::open(path.clone())?;
    let parent = origin(&store, &temporary.path().join("project")).await?;
    let child = store
        .submit_agent_job(parent.run.id.clone(), "legacy-child".into(), request())
        .await?;
    approve(&store, &child).await?;
    let nested = store
        .claim_next_job(environment(false))
        .await?
        .context("Missing child")?;
    store.mark_agent_running(nested.run.id.clone()).await?;
    for (run, id, status) in [
        (&parent.run.id, Some("same-child-id"), Some("pendingInit")),
        (&parent.run.id, Some("same-child-id"), Some("running")),
        (&parent.run.id, Some("same-child-id"), Some("completed")),
        (&parent.run.id, Some("same-child-id"), None),
        (&nested.run.id, Some("same-child-id"), Some("running")),
        (&nested.run.id, Some("queued-child"), Some("pendingInit")),
        (
            &nested.run.id,
            Some("missing-native-state"),
            Some("notFound"),
        ),
        (&nested.run.id, None, Some("task_started")),
    ] {
        store
            .record_agent_observation(
                run.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: Default::default(),
                    data: ProviderEventData::ChildActivity {
                        child_id: id.map(str::to_owned),
                        status: status.map(str::to_owned),
                        details: None,
                    },
                },
            )
            .await?;
    }
    let root = parent.conversation_id;
    let snapshot = store.conversation_subagent_quota(root.clone()).await?;
    assert_eq!(snapshot.native.len(), 1);
    let native = &snapshot.native[0];
    assert_eq!(native.provider, Provider::Codex);
    assert_eq!(
        native.reported,
        SubagentCounts {
            active: 1,
            queued: 1,
            historical: 1
        }
    );
    assert_eq!((native.unknown_state, native.unidentified_events), (1, 1));
    let nested_scope = store
        .conversation_subagent_quota(nested.conversation_id)
        .await?;
    assert!(nested_scope.native.is_empty());
    assert_eq!(snapshot.managed.active, 1);
    drop(store);

    // Recreate the previous schema with history intact and verify the upgrade.
    let db = rusqlite::Connection::open(&path)?;
    remove_recent_schema(&db)?;
    db.execute_batch(
        "DROP TABLE attempt_subagent_audit;
         DROP TABLE native_subagent_audit_candidates;
         DROP TABLE conversation_forks;
         DROP VIEW unconfirmed_agent_stops;
         DROP TABLE unconfirmed_managed_goal_stops;
         DROP TABLE goal_writer_attempts;
         DROP TABLE goal_writer_recoveries;
         DROP TABLE goal_revision_controls;
         DROP TABLE late_goal_turn_reports;
         DROP TABLE native_goal_resume_checkpoints;
         DROP TABLE native_goal_context_receipts;
         DROP TABLE native_goal_executors;
         DROP TABLE native_goal_qualifications;
         DROP TABLE native_goal_controls;
         DROP TABLE native_goal_turns;
         DROP TABLE native_goal_observations;
         DROP TABLE goal_turn_reports;
         DROP TABLE goal_checkpoints;
         DROP TABLE goal_continuations;
         DROP TABLE active_goals;
         DROP TABLE goal_revisions;
         DROP TABLE goals;
         DROP VIEW borrowed_instruction_deliveries;
         DROP TABLE instruction_delivery_attempts;
         ALTER TABLE conversations DROP COLUMN history_generation;
         ALTER TABLE conversation_contexts DROP COLUMN history_generation;
         DROP INDEX jobs_by_conversation_sequence;
         DROP TABLE queued_conversation_instructions;
         DROP VIEW deliverable_job_notices;
         DROP VIEW ready_question_continuations;
         DROP VIEW open_agent_questions;
         DROP VIEW unconfirmed_question_stops;
         DROP TRIGGER notice_structured_question;
         DROP TRIGGER notice_job_outcome;
         DROP INDEX agent_questions_by_run;
         DROP INDEX question_continuations_by_run;
         DROP TABLE recorded_question_answers;
         DROP TABLE native_question_waits;
         DROP TABLE project_question_timeouts;
         DROP TABLE project_subagent_audit;
         DROP TABLE conversation_subagent_audit;
         ALTER TABLE job_pipeline_nodes DROP COLUMN subagent_audit;
         DROP VIEW managed_subagent_states;
         DROP TABLE native_subagent_reservations;
         DROP INDEX agent_attempts_by_subagent_conversation;
         ALTER TABLE agent_attempts DROP COLUMN subagent_conversation_id;
         ALTER TABLE agent_sessions DROP COLUMN native_subagent_limit;
         DROP TABLE native_subagent_observations;
         DROP TABLE job_delegation_owners;
         DROP TABLE project_subagent_policies;
         DROP TABLE conversation_subagent_quotas;
         PRAGMA user_version = 33;",
    )?;
    drop(db);
    let upgraded = Store::open(path)?;
    assert_eq!(upgraded.conversation_subagent_quota(root).await?, snapshot);
    Ok(())
}

#[tokio::test]
async fn history_switch_applies_project_defaults_and_overrides_atomically() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("queue.sqlite");
    let store = Store::open(database.clone())?;
    let first = origin(&store, &temporary.path().join("project")).await?;
    let chat = first.conversation_id;
    let project = first.run.project_id;

    assert!(
        store
            .rewind_conversation_history(chat.clone(), first.context_id, None)
            .await
            .is_err()
    );
    store
        .finish_agent_attempt(
            first.run.id,
            RunStatus::Succeeded,
            Some("Original answer".into()),
        )
        .await?;

    for policy in [
        QueueHistoryPolicy::Review,
        QueueHistoryPolicy::Recorded,
        QueueHistoryPolicy::Cancel,
    ] {
        let history = store.conversation_history(chat.clone()).await?;
        let seed = if history.messages.is_empty() {
            let turn = store
                .prepare_agent_attempt(
                    chat.clone(),
                    history.context_id,
                    first.scope.clone(),
                    "New visible input".into(),
                )
                .await?;
            store.mark_agent_running(turn.run.id.clone()).await?;
            store
                .finish_agent_attempt(
                    turn.run.id,
                    RunStatus::Succeeded,
                    Some("Visible answer".into()),
                )
                .await?;
            store.conversation_history(chat.clone()).await?
        } else {
            history
        };
        match policy {
            QueueHistoryPolicy::Review => {
                assert_eq!(
                    store.project_queue_history_policy(project.clone()).await?,
                    policy
                );
            }
            QueueHistoryPolicy::Recorded => {
                store
                    .set_project_queue_history_policy(project.clone(), policy)
                    .await?;
            }
            QueueHistoryPolicy::Cancel => {
                let settings = store
                    .set_conversation_queue_history_policy(chat.clone(), Some(policy))
                    .await?;
                assert_eq!(settings.project_default, QueueHistoryPolicy::Recorded);
                assert_eq!(settings.conversation_override, Some(policy));
            }
        }
        store
            .save_conversation_draft(chat.clone(), "Frozen job instruction".into())
            .await?;
        let job = store
            .enqueue_conversation_turn(
                JobContext {
                    conversation_id: chat.clone(),
                    context_id: seed.context_id.clone(),
                },
                first.scope.clone(),
                "Frozen job instruction".into(),
            )
            .await?;

        assert!(
            store
                .rewind_conversation_history(chat.clone(), "old-base".into(), None)
                .await
                .is_err()
        );
        assert_eq!(
            store
                .job(project.clone(), job.id.clone())
                .await?
                .unwrap()
                .queue_state,
            QueueState::Queued
        );

        let through =
            (policy == QueueHistoryPolicy::Recorded).then(|| seed.messages[0].revision_id.clone());
        let active = store
            .rewind_conversation_history(chat.clone(), seed.context_id.clone(), through.clone())
            .await?;
        let reopened = Store::open(database.clone())?;
        assert_ne!(active, seed.context_id);
        let visible = reopened.conversation_history(chat.clone()).await?;
        assert_eq!(visible.context_id, active);
        assert_eq!(visible.messages.len(), usize::from(through.is_some()));
        let latest = reopened
            .job(project.clone(), job.id.clone())
            .await?
            .context("Job disappeared")?;
        match policy {
            QueueHistoryPolicy::Review => {
                assert_eq!(latest.queue_state, QueueState::Paused);
                assert!(reopened.claim_next_job(environment(false)).await?.is_none());
                reopened
                    .set_pending_job_state(project.clone(), job.id, QueueState::Queued)
                    .await?;
            }
            QueueHistoryPolicy::Recorded => assert_eq!(latest.queue_state, QueueState::Queued),
            QueueHistoryPolicy::Cancel => {
                assert_eq!(latest.queue_state, QueueState::Cancelled);
                assert!(reopened.claim_next_job(environment(false)).await?.is_none());
                continue;
            }
        }
        let attempt = reopened
            .claim_next_job(environment(false))
            .await?
            .context("Retained job did not start")?;
        assert!(matches!(attempt.intent, SessionIntent::Start));
        assert!(attempt.input.starts_with(&seed.messages));
        assert_eq!(attempt.input.last().unwrap().text, "Frozen job instruction");
        assert_eq!(
            reopened.conversation(chat.clone()).await?.context_id,
            active
        );
        reopened.mark_agent_running(attempt.run.id.clone()).await?;
        reopened
            .finish_agent_attempt(
                attempt.run.id,
                RunStatus::Succeeded,
                Some("Retained job output".into()),
            )
            .await?;
        assert_eq!(
            reopened.conversation_messages(chat.clone()).await?,
            visible.messages
        );
    }
    Ok(())
}

#[tokio::test]
async fn history_switch_archives_old_questions_and_keeps_the_pinned_stage_wait() -> Result<()> {
    use omniagent::{
        dispatch::EventSource,
        providers::{
            ProviderEvent, ProviderEventData, RequestId,
            questions::{
                QuestionAction, QuestionField, QuestionForm, QuestionInput, QuestionOrigin,
                QuestionReply, QuestionRoute,
            },
        },
    };

    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("queue.sqlite");
    let store = Store::open(database.clone())?;
    let first = origin(&store, &temporary.path().join("project")).await?;
    let chat = first.conversation_id;
    let project = first.run.project_id;
    store
        .finish_agent_attempt(
            first.run.id,
            RunStatus::Succeeded,
            Some("Source answer".into()),
        )
        .await?;

    let scope = first.scope;
    let record_question = |attempt: &AttemptPlan, multiple| {
        let origin = QuestionOrigin {
            run_id: attempt.run.id.clone(),
            request_id: RequestId::Text(format!("question:{}", attempt.run.turn_id)),
        };
        let schema = serde_json::json!({"type":"object","properties":{"summary":{"type":"boolean"}},"required":["summary"]});
        let message = "Include a summary?";
        let form = QuestionForm {
            message: Some(message.into()),
            fields: vec![QuestionField {
                id: "summary".into(),
                label: "summary".into(),
                header: None,
                description: None,
                secret: false,
                required: true,
                default: None,
                input: QuestionInput::Boolean,
            }],
            actions: vec![
                QuestionAction::Accept,
                QuestionAction::Decline,
                QuestionAction::Cancel,
            ],
        };
        let event = ProviderEvent {
            location: Default::default(),
            data: ProviderEventData::Question {
                request_id: origin.request_id.clone(),
                form: serde_json::json!({"message": message, "schema": schema}),
                normalized: Some(form),
                route: QuestionRoute::StructuredTurn,
            },
        };
        let store = &store;
        async move {
            store.mark_agent_running(origin.run_id.clone()).await?;
            store
                .record_agent_observation(
                    origin.run_id.clone(),
                    EventSource::Application,
                    event.clone(),
                )
                .await?;
            if multiple {
                for id in [42, 43] {
                    let mut other = event.clone();
                    if let ProviderEventData::Question { request_id, .. } = &mut other.data {
                        *request_id = RequestId::Number(id);
                    }
                    store
                        .record_agent_observation(
                            origin.run_id.clone(),
                            EventSource::Application,
                            other,
                        )
                        .await?;
                }
            }
            store
                .finish_agent_attempt(
                    origin.run_id.clone(),
                    RunStatus::Succeeded,
                    Some(message.into()),
                )
                .await?;
            Ok::<_, anyhow::Error>(origin)
        }
    };
    let current = store.conversation(chat.clone()).await?;
    let abandoned = store
        .prepare_structured_planning_attempt(
            chat.clone(),
            current.context_id,
            scope.clone(),
            "Old planning branch".into(),
            Vec::new(),
        )
        .await?;
    let old_question = record_question(&abandoned, false).await?;
    let recorded = store.conversation_history(chat.clone()).await?;
    let pipeline = "history-pipeline".to_owned();
    store
        .register_job_pipeline(project.clone(), pipeline.clone())
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            pipeline.clone(),
            "stage".into(),
            PipelineNodeKind::Execution,
        )
        .await?;
    store
        .set_job_pipeline_plan(project.clone(), pipeline.clone(), None, "plan-v1".into())
        .await?;
    store
        .set_project_job_rules(
            project.clone(),
            vec![],
            vec![JobRule {
                role: JobRole::Pipeline,
                operation: SessionMode::ReadOnly,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Review,
                requirements: Default::default(),
            }],
        )
        .await?;

    let stage = store
        .submit_pinned_pipeline_agent_job(
            PipelineDispatch {
                pipeline_run_id: pipeline,
                node_instance_id: "stage".into(),
                plan_revision: Some("plan-v1".into()),
                release_id: None,
            },
            scope.clone(),
            JobRequest {
                checkout_id: Some(first.run.checkout_id),
                mode: SessionMode::ReadOnly,
                ..request()
            },
            JobContext {
                conversation_id: chat.clone(),
                context_id: recorded.context_id.clone(),
            },
            QuestionRoute::StructuredTurn,
        )
        .await?;
    store
        .set_project_queue_history_policy(project.clone(), QueueHistoryPolicy::Cancel)
        .await?;
    let clear = store
        .rewind_conversation_history(chat.clone(), recorded.context_id.clone(), None)
        .await?;
    assert_eq!(
        store
            .job(project.clone(), stage.id.clone())
            .await?
            .unwrap()
            .queue_state,
        QueueState::Queued
    );
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    approve(&store, &stage).await?;
    let execution = store
        .claim_next_job(environment(false))
        .await?
        .context("Approved stage stayed blocked")?;
    assert!(execution.input.starts_with(&recorded.messages));
    assert!(matches!(execution.intent, SessionIntent::Start));
    let pinned_question = record_question(&execution, true).await?;

    let clear_again = store
        .rewind_conversation_history(chat.clone(), clear, None)
        .await?;
    let reply = QuestionReply {
        action: QuestionAction::Accept,
        values: [("summary".into(), serde_json::json!(true))]
            .into_iter()
            .collect(),
    };
    let archived_boundary = recorded
        .messages
        .first()
        .context("missing retained question prompt")?;
    let fork = store
        .fork_retained_conversation(
            chat.clone(),
            clear_again.clone(),
            recorded.context_id.clone(),
            archived_boundary.revision_id.clone(),
            "Fork from the archived question".into(),
        )
        .await?;
    let fork_history = store.conversation_history(fork.id.clone()).await?;
    assert_eq!(fork_history.messages.len(), 1);
    assert_eq!(fork_history.messages[0].text, archived_boundary.text);
    assert!(
        store
            .prepare_structured_planning_reply(
                fork.id,
                fork.context_id,
                scope.clone(),
                old_question.clone(),
                reply.clone(),
            )
            .await
            .is_err()
    );
    let inspected = serde_json::to_value(store.inspect_conversation_history(chat.clone()).await?)?;
    assert_eq!(
        inspected["retainedQuestions"],
        serde_json::json!([
            { "runId": pinned_question.run_id, "requestId": pinned_question.request_id },
            { "runId": pinned_question.run_id, "requestId": 42 },
            { "runId": pinned_question.run_id, "requestId": 43 },
        ])
    );
    assert!(
        store
            .prepare_structured_planning_reply(
                chat.clone(),
                clear_again.clone(),
                scope.clone(),
                old_question.clone(),
                reply.clone(),
            )
            .await
            .is_err()
    );
    assert!(
        store
            .agent_observations(chat.clone(), old_question.run_id.clone(), 0)
            .await?
            .iter()
            .any(|saved| matches!(&saved.event.data, ProviderEventData::Question { .. }))
    );
    let delivery: String = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?
    .query_row(
        "SELECT delivery FROM agent_questions WHERE run_id = ?1",
        [&old_question.run_id],
        |row| row.get(0),
    )?;
    assert_eq!(delivery, "closed");

    let mut notices = store.job_notices();
    let mut found = false;
    for _ in 0..2 {
        if let Ok(notice) = tokio::time::timeout(Duration::from_millis(500), notices.next()).await {
            let notice = notice?;
            found |= notice.kind == JobNoticeKind::Question
                && notice.run_id.as_ref() == Some(&execution.run.id);
            notices.acknowledge(&notice).await?;
        }
    }
    assert!(
        found,
        "The originating question lost its notification after the clear"
    );

    let second_question = QuestionOrigin {
        run_id: pinned_question.run_id.clone(),
        request_id: RequestId::Number(42),
    };
    let invalid = QuestionReply {
        action: QuestionAction::Accept,
        values: [("summary".into(), serde_json::json!("true"))]
            .into_iter()
            .collect(),
    };
    assert!(
        store
            .answer_saved_question(chat.clone(), pinned_question.clone(), invalid)
            .await
            .is_err()
    );
    assert!(
        store
            .answer_saved_question(chat.clone(), old_question.clone(), reply.clone())
            .await
            .is_err()
    );
    store
        .answer_saved_question(chat.clone(), pinned_question.clone(), reply.clone())
        .await?;
    store
        .answer_saved_question(chat.clone(), pinned_question.clone(), reply.clone())
        .await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let other_reply = QuestionReply {
        action: QuestionAction::Accept,
        values: [("summary".into(), serde_json::json!(false))]
            .into_iter()
            .collect(),
    };
    assert!(
        store
            .answer_saved_question(chat.clone(), pinned_question.clone(), other_reply.clone())
            .await
            .is_err()
    );
    store
        .answer_saved_question(chat.clone(), second_question, other_reply)
        .await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    store
        .answer_saved_question(
            chat.clone(),
            QuestionOrigin {
                run_id: pinned_question.run_id.clone(),
                request_id: RequestId::Number(43),
            },
            QuestionReply {
                action: QuestionAction::Cancel,
                values: Default::default(),
            },
        )
        .await?;
    store.set_job_queue_paused(true).await?;
    let reopened = Store::open(database.clone())?;
    reopened.recover_agent_attempts_after_restart().await?;
    let inspected = reopened
        .inspect_job(project.clone(), stage.id.clone(), environment(false))
        .await?
        .unwrap();
    assert_eq!(inspected.queued_question, Some(true));
    assert!(reopened.claim_next_job(environment(false)).await?.is_none());
    reopened.set_job_queue_paused(false).await?;
    let (first, second) = tokio::join!(
        store.claim_next_job(environment(false)),
        reopened.claim_next_job(environment(false))
    );
    let (first, second) = (first?, second?);
    assert_ne!(
        first.is_some(),
        second.is_some(),
        "exactly one linked turn should be claimed"
    );
    let followup = first
        .or(second)
        .context("The recorded answers did not continue the job")?;
    assert!(matches!(followup.intent, SessionIntent::Start));
    assert_eq!(followup.previous_run_id.as_ref(), Some(&execution.run.id));
    assert!(followup.input.starts_with(&execution.input));
    let sent = &followup.input.last().unwrap().text;
    assert!(sent.contains("\"summary\":true") && sent.contains("\"summary\":false"));
    assert!(sent.contains("\"action\":\"cancel\""));
    let linked: u32 = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?
    .query_row(
        "SELECT count(*) FROM structured_question_continuations WHERE continuation_run_id = ?1",
        [&followup.run.id],
        |row| row.get(0),
    )?;
    assert_eq!(linked, 2);
    store.mark_agent_running(followup.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            followup.run.id,
            RunStatus::Succeeded,
            Some("Saved answer".into()),
        )
        .await?;
    assert!(
        store
            .answer_saved_question(chat.clone(), pinned_question, reply)
            .await
            .is_err()
    );
    assert_eq!(
        store.conversation(chat.clone()).await?.context_id,
        clear_again
    );
    assert!(store.conversation_messages(chat).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn queued_conversation_draft_and_snapshot_survive_reopen_without_creating_a_run() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    let parent = origin(&store, &temp.path().join("project")).await?;
    store
        .finish_agent_attempt(parent.run.id, RunStatus::Cancelled, None)
        .await?;
    let chat = store
        .create_conversation(parent.run.checkout_id, "Durable queue".into())
        .await?;
    let job_context = JobContext {
        conversation_id: chat.id.clone(),
        context_id: chat.context_id.clone(),
    };
    store
        .save_conversation_draft(chat.id.clone(), "Saved exact prompt ☕".into())
        .await?;
    assert!(
        store
            .enqueue_conversation_turn(
                job_context.clone(),
                parent.scope.clone(),
                "Raced draft".into()
            )
            .await
            .is_err()
    );
    assert_eq!(
        store.conversation_draft(chat.id.clone()).await?,
        "Saved exact prompt ☕"
    );
    let settings = QueueSettings {
        paused: true,
        capacity: 2,
    };
    store.set_job_queue_settings(settings).await?;
    let queued = store
        .enqueue_conversation_turn(job_context, parent.scope, "Saved exact prompt ☕".into())
        .await?;
    assert!(store.conversation_draft(chat.id.clone()).await?.is_empty());
    assert!(
        store
            .latest_conversation_run(chat.id.clone())
            .await?
            .is_none()
    );
    assert_eq!(
        store.conversation(chat.id.clone()).await?.context_id,
        chat.context_id
    );
    drop(store);
    // A pending conversation job from the previous schema already owns its prompt and scope.
    let db = rusqlite::Connection::open(&database)?;
    remove_recent_schema(&db)?;
    db.execute_batch(
        "DROP TABLE conversation_forks;
         DROP VIEW unconfirmed_agent_stops;
         DROP TABLE unconfirmed_managed_goal_stops;
         DROP TABLE goal_writer_attempts;
         DROP TABLE goal_writer_recoveries;
         DROP TABLE goal_revision_controls;
         DROP TABLE late_goal_turn_reports;
         DROP TABLE native_goal_resume_checkpoints;
         DROP TABLE native_goal_context_receipts;
         DROP TABLE native_goal_executors;
         DROP TABLE native_goal_qualifications;
         DROP TABLE native_goal_controls;
         DROP TABLE native_goal_turns;
         DROP TABLE native_goal_observations;
         DROP TABLE goal_turn_reports;
         DROP TABLE goal_checkpoints;
         DROP TABLE goal_continuations;
         DROP TABLE active_goals;
         DROP TABLE goal_revisions;
         DROP TABLE goals;
         DROP VIEW borrowed_instruction_deliveries;
         DROP TABLE instruction_delivery_attempts;
         ALTER TABLE conversations DROP COLUMN history_generation;
         ALTER TABLE conversation_contexts DROP COLUMN history_generation;
         DROP TABLE queued_conversation_instructions;
         DROP INDEX jobs_by_conversation_sequence;
         PRAGMA user_version = 38;",
    )?;
    drop(db);
    let store = Store::open(database)?;
    assert_eq!(store.job_queue_settings().await?, settings);
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let current = store
        .latest_conversation_job(chat.id.clone(), environment(false))
        .await?
        .context("pending job missing")?;
    assert_eq!(current.job, queued);
    assert_eq!(current.blockers, [AdmissionBlocker::QueuePaused]);
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            ..settings
        })
        .await?;
    let plan = store
        .claim_next_job(environment(false))
        .await?
        .context("did not claim saved job")?;
    assert_eq!(plan.conversation_id, chat.id);
    assert_eq!(
        plan.input.last().context("missing prompt")?.text,
        "Saved exact prompt ☕"
    );
    assert_eq!(
        store
            .latest_conversation_run(chat.id.clone())
            .await?
            .context("missing run")?,
        plan.run
    );
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    store
        .finish_agent_attempt(plan.run.id.clone(), RunStatus::Cancelled, None)
        .await?;
    assert_eq!(
        store
            .job(queued.project_id, queued.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .status,
        RunStatus::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn queued_instruction_accepts_an_appended_reply_but_preserves_explicit_history_boundaries()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let store = Store::open(root.path().join("queue.sqlite"))?;
    let running = origin(&store, &root.path().join("repository")).await?;
    let conversation = store.conversation(running.conversation_id.clone()).await?;
    let context_before_reply = JobContext {
        conversation_id: conversation.id.clone(),
        context_id: conversation.context_id,
    };
    store
        .finish_agent_attempt(
            running.run.id,
            RunStatus::Succeeded,
            Some("Background reply".into()),
        )
        .await?;
    store
        .save_conversation_draft(conversation.id.clone(), "Still in the same history".into())
        .await?;
    let queued = store
        .enqueue_conversation_turn(
            context_before_reply.clone(),
            running.scope.clone(),
            "Still in the same history".into(),
        )
        .await?;
    assert_eq!(queued.context, Some(context_before_reply));
    store
        .set_pending_job_state(queued.project_id, queued.id, QueueState::Cancelled)
        .await?;

    let current = store.conversation(conversation.id.clone()).await?;
    let initial_empty = store
        .rewind_conversation_history(conversation.id.clone(), current.context_id, None)
        .await?;
    let cleared = store
        .rewind_conversation_history(conversation.id.clone(), initial_empty.clone(), None)
        .await?;
    assert_ne!(initial_empty, cleared);
    store
        .save_conversation_draft(
            conversation.id.clone(),
            "Draft from the earlier empty context".into(),
        )
        .await?;
    let stale = store
        .enqueue_conversation_turn(
            JobContext {
                conversation_id: conversation.id.clone(),
                context_id: initial_empty,
            },
            running.scope,
            "Draft from the earlier empty context".into(),
        )
        .await;
    assert!(stale.is_err());
    assert_eq!(
        store.conversation_draft(conversation.id).await?,
        "Draft from the earlier empty context"
    );
    Ok(())
}

#[tokio::test]
async fn pending_instructions_keep_order_and_claim_one_revision_after_restart() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    store.set_job_queue_capacity(1).await?;
    let active = origin(&store, &temp.path().join("project")).await?;
    let conversation = store.conversation(active.conversation_id.clone()).await?;
    let context = JobContext {
        conversation_id: conversation.id.clone(),
        context_id: conversation.context_id.clone(),
    };
    let attachment = "queue-reference.md";
    fs::write(
        active.scope.checkout.join(attachment),
        "Keep this reference with the instruction",
    )?;
    let preview = store
        .preview_attachment(
            conversation.checkout_id.clone(),
            AttachmentSource::Checkout {
                relative_path: attachment.into(),
            },
            AttachmentDelivery::Reference,
        )
        .await?;
    store
        .save_conversation_draft(conversation.id.clone(), "Original instruction".into())
        .await?;
    let first = store
        .enqueue_conversation_turn_with_attachments(
            context.clone(),
            active.scope.clone(),
            "Original instruction".into(),
            vec![preview],
        )
        .await?;
    let enqueue = |text: &'static str| {
        let store = &store;
        let context = context.clone();
        let scope = active.scope.clone();
        async move {
            store
                .save_conversation_draft(context.conversation_id.clone(), text.into())
                .await?;
            store
                .enqueue_conversation_turn(context, scope, text.into())
                .await
        }
    };
    let removed = enqueue("Remove me").await?;
    let last = enqueue("Last in order").await?;
    let pending =
        || store.pending_conversation_instructions(conversation.id.clone(), environment(false));
    let pending = pending().await?;
    assert_eq!(
        pending.iter().map(|entry| &entry.id).collect::<Vec<_>>(),
        [&first.id, &removed.id, &last.id]
    );
    assert_eq!(pending[0].attachments, [attachment]);
    assert_eq!(pending[0].context_id, context.context_id);
    assert_eq!(pending[0].provider, active.scope.provider);
    assert_eq!(pending[0].checkout_id, active.run.checkout_id);
    assert!(
        pending
            .iter()
            .all(|entry| entry.delivery == InstructionDelivery::Queued && entry.editable)
    );
    store
        .revise_pending_instruction(
            conversation.id.clone(),
            first.id.clone(),
            pending[0].revision_id.clone(),
            "Revised while the turn runs".into(),
            None,
        )
        .await?;
    assert!(
        store
            .revise_pending_instruction(
                conversation.id.clone(),
                first.id.clone(),
                pending[0].revision_id.clone(),
                "Stale edit".into(),
                None
            )
            .await
            .is_err()
    );
    store
        .remove_pending_instruction(
            conversation.id.clone(),
            removed.id.clone(),
            pending[1].revision_id.clone(),
        )
        .await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    store
        .finish_agent_attempt(
            active.run.id,
            RunStatus::Succeeded,
            Some("Result of the active turn".into()),
        )
        .await?;
    store
        .set_pending_job_state(
            first.project_id.clone(),
            first.id.clone(),
            QueueState::Paused,
        )
        .await?;
    drop(store);

    let store = Store::open(database.clone())?;
    store.recover_agent_attempts_after_restart().await?;
    let pending = store
        .pending_conversation_instructions(conversation.id.clone(), environment(false))
        .await?;
    assert_eq!(pending[0].delivery, InstructionDelivery::Paused);
    assert_eq!(pending[0].id, first.id);
    assert_eq!(pending[0].text, "Revised while the turn runs");
    assert_eq!(pending[0].attachments, [attachment]);
    assert_eq!(pending[0].context_id, context.context_id);
    assert_eq!(pending[1].id, last.id);
    assert!(
        pending[1]
            .blockers
            .contains(&AdmissionBlocker::InstructionOrder {
                job_id: first.id.clone()
            })
    );
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    store
        .set_pending_job_state(first.project_id, first.id.clone(), QueueState::Queued)
        .await?;
    let other = Store::open(database.clone())?;
    let (claim, edit) = tokio::join!(
        store.claim_next_job(environment(false)),
        other.revise_pending_instruction(
            conversation.id.clone(),
            first.id.clone(),
            pending[0].revision_id.clone(),
            "Revision at the claim boundary".into(),
            None
        ),
    );
    let claim = claim?.context("The first instruction was not claimed")?;
    let accepted = claim.input.last().context("No accepted instruction")?;
    assert_eq!(
        accepted.text,
        if edit.is_ok() {
            "Revision at the claim boundary"
        } else {
            "Revised while the turn runs"
        }
    );
    if edit.is_err() {
        assert_eq!(accepted.revision_id, pending[0].revision_id);
    }
    assert_eq!(
        claim
            .input
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        [
            "Plan the fixture",
            "Result of the active turn",
            accepted.text.as_str()
        ]
    );
    let list = store
        .pending_conversation_instructions(conversation.id.clone(), environment(false))
        .await?;
    assert_eq!(list[0].delivery, InstructionDelivery::Delivering);
    assert_eq!(list[0].run_id.as_ref(), Some(&claim.run.id));
    assert_eq!(list[0].revision_id, accepted.revision_id);
    assert_eq!(list[0].attachments, [attachment]);
    assert!(
        store
            .remove_pending_instruction(
                conversation.id.clone(),
                first.id,
                accepted.revision_id.clone()
            )
            .await
            .is_err()
    );
    assert!(other.claim_next_job(environment(false)).await?.is_none());
    store.mark_agent_running(claim.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            claim.run.id,
            RunStatus::Succeeded,
            Some("First instruction finished".into()),
        )
        .await?;
    let next = store
        .claim_next_job(environment(false))
        .await?
        .context("The next instruction didn't follow")?;
    assert_eq!(next.input.last().unwrap().text, "Last in order");
    assert!(
        next.input
            .iter()
            .any(|message| message.text == "First instruction finished")
    );
    assert!(next.input.iter().all(|message| message.text != "Remove me"));
    store.mark_agent_running(next.run.id.clone()).await?;
    store
        .finish_agent_attempt(next.run.id, RunStatus::Succeeded, None)
        .await?;
    Ok(())
}

#[tokio::test]
async fn inspector_and_claim_agree_on_approval_dependencies_bridge_and_capacity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    let parallel = Store::open(database)?;
    let parent = origin(&store, &temp.path().join("project")).await?;
    let task = store
        .create_project_task(parent.run.project_id.clone(), "Queue task".into())
        .await?;
    let bridge_job = store
        .submit_agent_job(
            parent.run.id.clone(),
            "needs-bridge".into(),
            JobRequest {
                requires_bridge: true,
                ..request()
            },
        )
        .await?;
    let portable = store
        .submit_agent_job(
            parent.run.id.clone(),
            "portable".into(),
            JobRequest {
                task_id: Some(task.id.clone()),
                ..request()
            },
        )
        .await?;
    let dependent = store
        .submit_agent_job(
            parent.run.id,
            "depends-on-portable".into(),
            JobRequest {
                dependencies: vec![portable.id.clone()],
                ..request()
            },
        )
        .await?;
    let read =
        |job: &Job| store.inspect_job(job.project_id.clone(), job.id.clone(), environment(false));
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    for job in [&bridge_job, &portable, &dependent] {
        approve(&store, job).await?;
    }
    assert_eq!(
        read(&bridge_job).await?.unwrap().blockers,
        [AdmissionBlocker::MissingCapability {
            capability: Capability::RunScopedMcp
        }]
    );
    assert!(read(&portable).await?.unwrap().blockers.is_empty());
    assert_eq!(
        read(&dependent).await?.unwrap().blockers,
        [AdmissionBlocker::Dependency {
            job_id: portable.id.clone(),
            status: None,
            queue_state: QueueState::Queued
        }]
    );
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    let (first, second) = tokio::join!(
        store.claim_next_job(environment(false)),
        parallel.claim_next_job(environment(false))
    );
    let claimed = [first?, second?].into_iter().flatten().collect::<Vec<_>>();
    assert_eq!(claimed.len(), 1);
    let plan = &claimed[0];
    assert_ne!(plan.conversation_id, parent.conversation_id);
    assert_eq!(
        read(&portable)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        plan.run.id
    );
    assert!(
        read(&dependent)
            .await?
            .unwrap()
            .blockers
            .contains(&AdmissionBlocker::Capacity {
                active: 1,
                limit: 1
            })
    );
    store.mark_agent_running(plan.run.id.clone()).await?;
    let grandchild = store
        .submit_agent_job(plan.run.id.clone(), "nested".into(), request())
        .await?;
    assert_eq!(grandchild.task_id, Some(task.id));
    assert_eq!(
        grandchild.origin,
        JobOrigin::Agent {
            run_id: plan.run.id.clone(),
            request_id: "nested".into()
        }
    );
    assert!(
        store
            .request_job_interrupt(parent.conversation_id, plan.run.id.clone())
            .await
            .is_err()
    );
    store
        .request_job_interrupt(plan.conversation_id.clone(), plan.run.id.clone())
        .await?;
    let stopping = read(&portable).await?.unwrap().job.latest_attempt.unwrap();
    assert!(stopping.cancel_requested);
    assert_eq!(stopping.status, RunStatus::Running);
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    // The provider can complete successfully before the cancellation takes effect.
    store
        .finish_agent_attempt(plan.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    assert!(
        store
            .finish_agent_attempt(plan.run.id.clone(), RunStatus::Cancelled, None)
            .await
            .is_err()
    );
    let complete = read(&portable).await?.unwrap().job.latest_attempt.unwrap();
    assert!(!complete.cancel_requested);
    assert_eq!(complete.status, RunStatus::Succeeded);
    assert!(read(&dependent).await?.unwrap().blockers.is_empty());
    let dependency_plan = store
        .claim_next_job(environment(false))
        .await?
        .context("dependent became eligible")?;
    assert_eq!(
        read(&dependent)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        dependency_plan.run.id
    );
    store
        .finish_agent_attempt(dependency_plan.run.id, RunStatus::Cancelled, None)
        .await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let bridge_plan = store
        .claim_next_job(environment(true))
        .await?
        .context("bridge job remained blocked")?;
    assert_eq!(
        read(&bridge_job)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        bridge_plan.run.id
    );
    store
        .finish_agent_attempt(bridge_plan.run.id, RunStatus::Cancelled, None)
        .await?;
    Ok(())
}

#[tokio::test]
async fn an_occupied_managed_worktree_and_a_declined_approval_block_the_same_jobs_in_both_paths()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    let root = temp.path().join("project");
    let parent = origin_with_mode(&store, &root, SessionMode::ApprovedWrite).await?;
    let git = |args: &[&str]| -> Result<String> {
        let output = Command::new("git.exe")
            .args(args)
            .current_dir(&root)
            .output()?;
        ensure!(
            output.status.success(),
            "fixture Git operation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().into())
    };
    git(&[
        "-c",
        "user.name=OmniAgent Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=.git/no-fixture-hooks",
        "commit",
        "--allow-empty",
        "--quiet",
        "-m",
        "Base",
    ])?;
    let worktree = temp.path().join("managed");
    let branch = "codex/queue-writer-fixture";
    git(&[
        "-c",
        "core.hooksPath=.git/no-fixture-hooks",
        "worktree",
        "add",
        "--quiet",
        "-b",
        branch,
        worktree.to_str().unwrap(),
    ])?;
    let worktree = repository::register(&store, worktree).await?;
    let task = store
        .create_project_task(worktree.project_id.clone(), "Fixture writer".into())
        .await?;
    // Register the real linked checkout as managed for this admission fixture.
    rusqlite::Connection::open(&database)?.execute(
        "INSERT INTO managed_task_worktrees VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            worktree.project_id,
            task.id,
            worktree.checkout_id,
            branch,
            git(&["rev-parse", "HEAD"])?
        ],
    )?;
    let writer = || JobRequest {
        checkout_id: Some(worktree.checkout_id.clone()),
        task_id: Some(task.id.clone()),
        mode: SessionMode::ApprovedWrite,
        ..request()
    };
    let first = store
        .submit_agent_job(parent.run.id.clone(), "first-writer".into(), writer())
        .await?;
    let second = store
        .submit_agent_job(parent.run.id.clone(), "second-writer".into(), writer())
        .await?;
    let declined = store
        .submit_agent_job(parent.run.id, "declined-writer".into(), writer())
        .await?;
    let inspect =
        |job: &Job| store.inspect_job(job.project_id.clone(), job.id.clone(), environment(false));
    let approval = inspect(&declined)
        .await?
        .unwrap()
        .blockers
        .into_iter()
        .find_map(|blocker| match blocker {
            AdmissionBlocker::Approval { approval_id } => Some(approval_id),
            _ => None,
        })
        .context("missing writer approval")?;
    store
        .decide_job_approval(declined.project_id.clone(), approval, false)
        .await?;
    for job in [&first, &second] {
        approve(&store, job).await?;
    }
    let claimed = store
        .claim_next_job(environment(false))
        .await?
        .context("first writer did not claim")?;
    let snapshot = store
        .inspect_project_queue(worktree.project_id.clone(), environment(false))
        .await?;
    assert_eq!(snapshot.active, 1);
    assert_eq!(snapshot.settings.capacity, 3);
    assert_eq!(snapshot.jobs.len(), 3);
    for (entry, job) in snapshot.jobs.iter().zip([&first, &second, &declined]) {
        assert_eq!(entry, &inspect(job).await?.unwrap());
    }
    assert_eq!(
        inspect(&first)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        claimed.run.id
    );
    assert_eq!(
        inspect(&second).await?.unwrap().blockers,
        [AdmissionBlocker::Writer {
            checkout_id: worktree.checkout_id.clone(),
            run_id: claimed.run.id.clone(),
            needs_review: false,
        }]
    );
    assert!(
        inspect(&declined)
            .await?
            .unwrap()
            .blockers
            .iter()
            .any(|blocker| matches!(blocker, AdmissionBlocker::Policy { .. }))
    );
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    store
        .finish_agent_attempt(claimed.run.id, RunStatus::Cancelled, None)
        .await?;
    assert!(inspect(&second).await?.unwrap().blockers.is_empty());
    let next = store
        .claim_next_job(environment(false))
        .await?
        .context("writer did not yield slot")?;
    assert_eq!(
        inspect(&second)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        next.run.id
    );
    store.mark_agent_running(next.run.id.clone()).await?;
    store
        .finish_agent_attempt(next.run.id.clone(), RunStatus::Cancelled, None)
        .await?;
    assert!(inspect(&second).await?.unwrap().recovery_actions.is_empty());
    assert!(
        store
            .queue_job_recovery(
                second.project_id,
                second.id,
                next.run.id.clone(),
                RecoveryAction::Retry
            )
            .await
            .is_err()
    );
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    assert_eq!(
        inspect(&declined).await?.unwrap().blockers,
        [
            AdmissionBlocker::Policy {
                reason: "An operator declined this job"
            },
            AdmissionBlocker::Writer {
                checkout_id: worktree.checkout_id,
                run_id: next.run.id,
                needs_review: true,
            },
        ]
    );
    Ok(())
}

async fn conversation_job(
    store: &Store,
    checkout: &Path,
) -> Result<(Conversation, Job, AttemptPlan)> {
    let parent = origin(store, checkout).await?;
    let chat = store
        .create_conversation(parent.run.checkout_id, "Recoverable job".into())
        .await?;
    store
        .save_conversation_draft(chat.id.clone(), request().instructions.clone())
        .await?;
    let job = store
        .enqueue_conversation_turn(
            JobContext {
                conversation_id: chat.id.clone(),
                context_id: chat.context_id.clone(),
            },
            parent.scope,
            request().instructions,
        )
        .await?;
    let plan = store
        .claim_next_job(environment(false))
        .await?
        .context("missing first attempt")?;
    Ok((chat, job, plan))
}

#[tokio::test]
async fn recovery_refuses_a_checkout_whose_git_identity_changed_after_failure() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("queue.sqlite3"))?;
    let (_, job, attempt) = conversation_job(&store, &temp.path().join("project")).await?;
    let run_id = attempt.run.id;
    store
        .finish_agent_attempt(run_id.clone(), RunStatus::Failed, None)
        .await?;
    let failure = store
        .job_failure(job.project_id.clone(), run_id.clone())
        .await?
        .context("failure should survive without logs or an artifact")?;
    assert!(!failure.logs_available && !failure.saved_diff_available);
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet", "--separate-git-dir"])
            .arg(temp.path().join("relocated-git"))
            .current_dir(&attempt.scope.checkout)
            .status()?
            .success(),
        "fixture could not relocate Git metadata"
    );
    assert!(
        store
            .queue_job_recovery(
                job.project_id.clone(),
                job.id.clone(),
                run_id.clone(),
                RecoveryAction::Retry
            )
            .await
            .is_err()
    );
    assert!(
        store
            .job(job.project_id.clone(), job.id)
            .await?
            .unwrap()
            .queued_recovery
            .is_none()
    );
    assert_eq!(
        store.job_failure(job.project_id, run_id).await?,
        Some(failure)
    );
    Ok(())
}

#[tokio::test]
async fn crash_after_claim_requeues_unlaunched_jobs_but_honors_a_pending_cancel() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    let (chat, job, untouched) = conversation_job(&store, &temp.path().join("ready")).await?;
    let (other, cancelled_job, cancelled) =
        conversation_job(&store, &temp.path().join("cancelled")).await?;
    store
        .request_job_interrupt(other.id, cancelled.run.id.clone())
        .await?;
    let reopened = Store::open(database)?;
    reopened.recover_agent_attempts_after_restart().await?;
    reopened.recover_agent_attempts_after_restart().await?;
    let no_retry = reopened
        .job(cancelled_job.project_id, cancelled_job.id)
        .await?
        .unwrap();
    assert!(no_retry.queued_recovery.is_none());
    let queued = reopened.job(job.project_id, job.id).await?.unwrap();
    assert_eq!(queued.queued_recovery, Some(RecoveryAction::Retry));
    assert_eq!(
        queued.latest_attempt.unwrap().status,
        RunStatus::Interrupted
    );
    let next = reopened
        .claim_next_job(environment(false))
        .await?
        .context("never-launched work was lost")?;
    assert_eq!(next.conversation_id, chat.id);
    assert_eq!(next.previous_run_id, Some(untouched.run.id));
    assert_eq!(next.context_id, untouched.context_id);
    assert_eq!(next.input, untouched.input);
    assert!(reopened.claim_next_job(environment(false)).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn restart_keeps_inflight_and_queued_recovery_distinct_then_links_only_one_retry()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("queue.sqlite3");
    let store = Store::open(database.clone())?;
    let (conversation, job, first) = conversation_job(&store, &temp.path().join("project")).await?;
    store.mark_agent_running(first.run.id.clone()).await?;
    drop(store);

    let store = Store::open(database.clone())?;
    store.recover_agent_attempts_after_restart().await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let interrupted = store
        .inspect_job(job.project_id.clone(), job.id.clone(), environment(false))
        .await?
        .unwrap();
    assert_eq!(
        interrupted.job.latest_attempt.unwrap().status,
        RunStatus::Interrupted
    );
    assert_eq!(interrupted.recovery_actions, [RecoveryAction::Retry]);
    store
        .set_job_queue_settings(QueueSettings {
            paused: true,
            capacity: 1,
        })
        .await?;
    let (one, two) = tokio::join!(
        store.queue_job_recovery(
            job.project_id.clone(),
            job.id.clone(),
            first.run.id.clone(),
            RecoveryAction::Retry
        ),
        store.queue_job_recovery(
            job.project_id.clone(),
            job.id.clone(),
            first.run.id.clone(),
            RecoveryAction::Retry
        ),
    );
    assert_eq!(one?, two?);
    drop(store);

    let reopened = Store::open(database.clone())?;
    reopened.recover_agent_attempts_after_restart().await?;
    let saved = reopened
        .inspect_job(job.project_id.clone(), job.id.clone(), environment(false))
        .await?
        .unwrap();
    assert_eq!(saved.job.queued_recovery, Some(RecoveryAction::Retry));
    assert_eq!(saved.blockers, [AdmissionBlocker::QueuePaused]);
    assert!(reopened.claim_next_job(environment(false)).await?.is_none());
    reopened
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    let competing_connection = Store::open(database.clone())?;
    let (one, two) = tokio::join!(
        reopened.claim_next_job(environment(false)),
        competing_connection.claim_next_job(environment(false))
    );
    let claims = [one?, two?].into_iter().flatten().collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    let retry = &claims[0];
    assert_eq!(retry.conversation_id, conversation.id);
    assert_eq!(
        retry.previous_run_id.as_deref(),
        Some(first.run.id.as_str())
    );
    assert_eq!(retry.input, first.input);
    assert_eq!(retry.context_id, first.context_id);
    assert!(matches!(retry.intent, SessionIntent::Start));
    assert_ne!(retry.agent_session_id, first.agent_session_id);
    let record = reopened.job(job.project_id, job.id).await?.unwrap();
    assert!(record.queued_recovery.is_none());
    let latest = record.latest_attempt.unwrap();
    assert_eq!(latest.ordinal, 2);
    assert_eq!(
        latest.previous_run_id.as_deref(),
        Some(first.run.id.as_str())
    );
    assert_eq!(latest.recovery, Some(RecoveryAction::Retry));
    let db = rusqlite::Connection::open(database)?;
    let events: i64 = db.query_row("SELECT count(*) FROM run_events WHERE turn_id = ?1 AND json_extract(payload, '$.status') = 'interrupted'", [&first.run.turn_id], |row| row.get(0))?;
    assert_eq!(events, 1);
    reopened
        .finish_agent_attempt(retry.run.id.clone(), RunStatus::Cancelled, None)
        .await?;
    Ok(())
}

#[tokio::test]
async fn resume_uses_the_recorded_partial_result_and_blocks_when_the_context_changes() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("queue.sqlite3"))?;
    let (conversation, job, first) = conversation_job(&store, &temp.path().join("project")).await?;
    store.mark_agent_running(first.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            first.run.id.clone(),
            RunStatus::Failed,
            Some("Retained partial answer".into()),
        )
        .await?;
    let checkpoint = store
        .conversation(conversation.id.clone())
        .await?
        .context_id;
    store
        .queue_job_recovery(
            job.project_id.clone(),
            job.id.clone(),
            first.run.id.clone(),
            RecoveryAction::Resume,
        )
        .await?;
    let resumed = store
        .claim_next_job(environment(false))
        .await?
        .context("missing resume attempt")?;
    assert!(matches!(resumed.intent, SessionIntent::Start));
    assert_eq!(resumed.previous_run_id, Some(first.run.id));
    assert_eq!(resumed.input[0], first.input[0]);
    assert_eq!(resumed.input[1].text, "Retained partial answer");
    assert!(
        resumed.input[2]
            .text
            .starts_with("Continue the interrupted task")
    );
    store.mark_agent_running(resumed.run.id.clone()).await?;
    store
        .finish_agent_attempt(resumed.run.id.clone(), RunStatus::Failed, None)
        .await?;
    store
        .queue_job_recovery(
            job.project_id.clone(),
            job.id.clone(),
            resumed.run.id,
            RecoveryAction::Retry,
        )
        .await?;
    store
        .replace_conversation_message(
            conversation.id,
            resumed.context_id.clone(),
            first.input[0].message_id.clone(),
            "Edited after queueing".into(),
        )
        .await?;
    assert!(store.claim_next_job(environment(false)).await?.is_none());
    let blocked = store
        .inspect_job(job.project_id.clone(), job.id.clone(), environment(false))
        .await?
        .unwrap();
    assert_eq!(
        blocked.blockers,
        [AdmissionBlocker::ContextChanged {
            context_id: resumed.context_id
        }]
    );
    store
        .set_pending_job_state(job.project_id, job.id, QueueState::Cancelled)
        .await?;
    let db = rusqlite::Connection::open(temp.path().join("queue.sqlite3"))?;
    let saved: String = db.query_row(
        "SELECT result_context_id FROM agent_attempts WHERE run_id = ?1",
        [resumed.previous_run_id.unwrap()],
        |row| row.get(0),
    )?;
    assert_eq!(saved, checkpoint);
    Ok(())
}
