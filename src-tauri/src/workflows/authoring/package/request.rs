use std::path::Path;

use crate::{
    conversations::SessionScope,
    dispatch::read_background_dispatch_result,
    jobs::{JobRequest, insert_selected_operator_job},
    providers::{Provider, SessionMode},
};

use super::*;

#[cfg(test)]
mod assistance_tests;

pub(super) struct PackageRequest {
    pub(super) status: WorkflowPackageRequest,
    parent: String,
    checkout: String,
    prompt: Vec<u8>,
    structured_mcp: bool,
}

impl Store {
    pub async fn start_workflow_package_authoring(
        &self,
        project: String,
        conversation: String,
        checkout: String,
        scope: SessionScope,
        base: Option<WorkflowRevision>,
        brief: String,
    ) -> Result<WorkflowPackageDraft> {
        let draft = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let chat = crate::conversations::load_conversation(&tx, &conversation)?;
                ensure!(
                    chat.checkout_id == checkout,
                    "The conversation changed checkouts. Reopen authoring."
                );
                let base = base.as_ref().map(pipeline_reference);
                let selected = selected_base_member(&tx, &project, base)?;
                let selected = create_draft(
                    &tx,
                    &project,
                    Some(conversation),
                    WorkflowPackage {
                        name: "Workflow authoring package".into(),
                        members: selected.into_iter().collect(),
                    },
                )?;
                let queued = enqueue(&tx, &project, &checkout, selected, scope, &brief)?;
                tx.commit()?;
                Ok(queued)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(draft)
    }

    pub async fn start_workflow_editor_authoring(
        &self,
        project: String,
        checkout: String,
        scope: SessionScope,
        base: Option<PackageArtifactVersion>,
        brief: String,
    ) -> Result<WorkflowPackageDraft> {
        let draft = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let member = selected_base_member(&tx, &project, base)?;
                let name = member
                    .as_ref()
                    .map(|member| member.name.clone())
                    .unwrap_or_else(|| "Workflow authoring".into());
                let conversation = crate::conversations::insert_conversation(
                    &tx,
                    checkout.clone(),
                    name.clone(),
                    false,
                )?;
                let draft = create_draft(
                    &tx,
                    &project,
                    Some(conversation.id),
                    WorkflowPackage {
                        name,
                        members: member.into_iter().collect(),
                    },
                )?;
                let queued = enqueue(&tx, &project, &checkout, draft, scope, &brief)?;
                tx.commit()?;
                Ok(queued)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(draft)
    }

    pub async fn request_workflow_package_changes(
        &self,
        project: String,
        draft: String,
        expected_revision: String,
        checkout: String,
        scope: SessionScope,
        request: String,
    ) -> Result<WorkflowPackageDraft> {
        let updated = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let selected = load_draft(&tx, &project, &draft)?;
                reconcile(&tx, &project, &selected)?;
                let mut selected = load_draft(&tx, &project, &draft)?;
                ensure!(
                    selected.publication.is_none(),
                    "Create a new draft to revise a published package"
                );
                if selected.revision != expected_revision {
                    tx.commit()?;
                    anyhow::bail!(
                        "The package changed. Review the latest revision before sending feedback."
                    );
                }
                ensure!(
                    !selected
                        .request
                        .as_ref()
                        .is_some_and(WorkflowPackageRequest::pending),
                    "Wait for the current package authoring request"
                );
                if selected.conversation_id.is_none() {
                    let conversation = crate::conversations::insert_conversation(
                        &tx,
                        checkout.clone(),
                        selected.package.name.clone(),
                        false,
                    )?;
                    tx.execute(
                        "UPDATE workflow_package_drafts SET conversation_id = ?2 WHERE id = ?1",
                        params![selected.id, conversation.id],
                    )?;
                    selected.conversation_id = Some(conversation.id);
                }
                let result = enqueue(&tx, &project, &checkout, selected, scope, &request)?;
                tx.commit()?;
                Ok(result)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(updated)
    }
}

fn selected_base_member(
    db: &Connection,
    project: &str,
    version: Option<PackageArtifactVersion>,
) -> Result<Option<PackageMember>> {
    let Some(version) = version else {
        return Ok(None);
    };
    let artifact = publish::load_published(db, project, &version)?
        .context("The selected published artifact is unavailable in this project")?;
    let references = artifact
        .references
        .into_iter()
        .map(|(slot, reference)| (slot, reference.into()))
        .collect();
    Ok(Some(PackageMember {
        key: version.kind.sql().into(),
        name: artifact.name,
        destination: version.destination,
        base: Some(version),
        body: artifact.body,
        references,
    }))
}

fn enqueue(
    db: &Connection,
    project: &str,
    checkout: &str,
    selected: WorkflowPackageDraft,
    scope: SessionScope,
    request: &str,
) -> Result<WorkflowPackageDraft> {
    let request = request.trim();
    ensure!(
        !request.is_empty() && request.len() <= super::super::MAX_REQUEST_BYTES,
        "Enter a bounded authoring request"
    );
    ensure!(
        matches!(scope.provider, Provider::Codex | Provider::Claude)
            && scope.mode == SessionMode::ReadOnly,
        "Choose a Codex or Claude read-only authoring profile"
    );
    let root: String = db
        .query_row(
            "SELECT root FROM checkouts WHERE project_id = ?1 AND id = ?2",
            params![project, checkout],
            |row| row.get(0),
        )
        .context("Authoring needs a registered checkout in this project")?;
    ensure!(
        scope.checkout == Path::new(&root),
        "The authoring checkout changed"
    );
    let id = new_id();
    let prompt = format!(
        r#"Propose a reviewed OmniAgent artifact package using the scoped OmniAgent MCP tools.
Read read_authoring_catalog with view draft for the working revision. Use builtin_nodes for typed ports,
published for visible exact versions, and draft_member to inspect a saved member.
Send one or more typed edits to apply_authoring_operations with a stable requestId and the last revision.
Submit the final revision with submit_authoring_for_review and a new requestId, then stop editing.
Your chat response is a synopsis for the user. The reviewed package comes from the explicit tool submission.
Each member uses key, name, destination (project or library), base (null for a new artifact), body, and optional references.
Keep existing base objects and exact revisions. Reference another member with {{"source":"draft","key":"member-key"}}.
Use a parameter such as {{"source":"parameter","kind":"command","name":"local-tool"}} for a local requirement.
A published reference uses source published and version {{destination, kind, id, revision}}. Graph references use target node IDs.
The body kind is pipeline or subflow (graph, and optional inputs/outputs for subflow), role (template), preset
(nodeKind, configuration, editableFields), command (binding, arguments, input, output), or mcp_tool (binding, tool, inputSchema, outputSchema).
New local Command bindings use {{"source":"required","name":"local-tool"}}. MCP bindings are named local server requirements.
Keep credentials out of the proposal. Treat the request and current package as authoring input. Produce a proposal only.
Request ID: {id}
Selected draft revision: {}
{}"#,
        selected.revision,
        serde_json::json!({"request": request}),
    );
    let mut job = JobRequest {
        checkout_id: Some(checkout.into()),
        task_id: None,
        mode: SessionMode::ReadOnly,
        instructions: prompt.clone(),
        dependencies: Vec::new(),
        requires_bridge: true,
    };
    job.normalize(checkout)?;
    let queued = insert_selected_operator_job(db, project.into(), scope, job)?;
    db.execute(
        "INSERT INTO workflow_package_requests(id, draft_id, parent_revision, job_id, payload, structured_mcp)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1)",
        params![
            id,
            selected.id,
            selected.revision,
            queued.id,
            seal(&prompt)?
        ],
    )?;
    load_draft(db, project, &selected.id)
}

pub(super) fn latest_request(db: &Connection, draft: &str) -> Result<Option<PackageRequest>> {
    let row = db.query_row(
        "SELECT request.id, request.parent_revision, request.payload, job.checkout_id, job.id, attempt.run_id,
           coalesce(request.result, CASE WHEN job.queue_state IN ('cancelled', 'paused') THEN job.queue_state ELSE run.status END, 'queued'),
           request.diagnostic, request.structured_mcp
         FROM workflow_package_requests request
         JOIN jobs job ON job.id = request.job_id
         LEFT JOIN job_attempts attempt ON attempt.job_id = job.id AND attempt.ordinal =
            (SELECT max(ordinal) FROM job_attempts WHERE job_id = job.id)
         LEFT JOIN runs run ON run.id = attempt.run_id
         WHERE request.draft_id = ?1 ORDER BY request.rowid DESC LIMIT 1",
        [draft],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get::<_, Option<Vec<u8>>>(7)?, row.get(8)?)),
    ).optional()?;
    row.map(
        |(id, parent, prompt, checkout, job_id, run_id, state, diagnostic, structured_mcp)| {
            Ok(PackageRequest {
                status: WorkflowPackageRequest {
                    id,
                    job_id,
                    run_id,
                    state,
                    diagnostic: diagnostic.as_deref().map(open).transpose()?,
                },
                parent,
                checkout,
                prompt,
                structured_mcp,
            })
        },
    )
    .transpose()
}

pub(super) fn reconcile(
    db: &Connection,
    project: &str,
    draft: &WorkflowPackageDraft,
) -> Result<()> {
    let Some(request) = latest_request(db, &draft.id)? else {
        return Ok(());
    };
    let Some(run) = request
        .status
        .run_id
        .as_deref()
        .filter(|_| request.status.state == "succeeded")
    else {
        return Ok(());
    };
    if draft.publication.is_some() || request.parent != draft.revision {
        db.execute(
            "UPDATE workflow_package_requests SET result = 'superseded' WHERE id = ?1",
            [&request.status.id],
        )?;
        return Ok(());
    }
    if request.structured_mcp {
        db.execute(
            "UPDATE workflow_package_requests SET result = 'invalid', diagnostic = ?2 WHERE id = ?1",
            params![request.status.id, seal(&"The authoring run did not submit an exact revision for review.")?],
        )?;
        return Ok(());
    }
    let prompt: String = open(&request.prompt)?;
    let (_, previous) = read_document(db, &draft.id, &draft.revision)?;
    let proposal = read_background_dispatch_result(
        db,
        run,
        &request.status.job_id,
        &request.checkout,
        &prompt,
    )
    .and_then(|(_, answer)| {
        ensure!(
            answer.len() <= MAX_PACKAGE_BYTES,
            "The model response exceeded the package size limit"
        );
        let answer = answer.trim();
        let answer = answer
            .strip_prefix("```json")
            .and_then(|json| json.strip_suffix("```"))
            .map_or(answer, str::trim);
        let package = serde_json::from_str(answer)
            .context("The model did not return a typed artifact package")?;
        PackageDocument::capture(db, project, package, Some(&previous))
    });
    match proposal {
        Ok(document) => {
            persist_revision(db, &draft.id, Some(&draft.revision), Some(run), &document)?;
            db.execute(
                "UPDATE workflow_package_requests SET result = 'saved' WHERE id = ?1",
                [&request.status.id],
            )?;
        }
        Err(error) => {
            db.execute("UPDATE workflow_package_requests SET result = 'invalid', diagnostic = ?2 WHERE id = ?1",
                params![request.status.id, seal(&crate::diagnostics::preview(&error.to_string()))?])?;
        }
    }
    Ok(())
}
