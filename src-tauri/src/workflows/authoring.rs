use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{
    conversations::{SessionScope, load_conversation},
    dispatch::read_background_dispatch_result,
    jobs::{JobRequest, insert_selected_operator_job},
    protection,
    providers::{Provider, SessionMode},
    store::Store,
};

use super::{
    WorkflowDefinition, WorkflowGraph, WorkflowRevision, builtin_registry, compile,
    runtime::load_definition,
};

const MAX_REQUEST_BYTES: usize = 8 * 1024;
const MAX_GRAPH_BYTES: usize = 48 * 1024;

#[cfg(test)]
mod tests;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthoringBase {
    pub version: WorkflowRevision,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthoringDraft {
    pub id: String,
    conversation_id: String,
    base: Option<WorkflowRevision>,
    head: Option<DraftRevision>,
    request: RequestStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DraftRevision {
    id: String,
    run_id: Option<String>,
    graph: WorkflowGraph,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestStatus {
    job_id: String,
    run_id: Option<String>,
    state: String,
    diagnostic: Option<String>,
}

struct DraftScope {
    project: String,
    checkout: String,
    conversation: String,
    workflow: String,
    base: Option<String>,
    head: Option<String>,
}

struct PendingRequest {
    id: String,
    parent: Option<String>,
    payload: Vec<u8>,
    status: RequestStatus,
}

impl Store {
    pub async fn workflow_authoring_bases(&self, project: String) -> Result<Vec<AuthoringBase>> {
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT definition FROM workflow_revisions WHERE project_id = ?1
                 ORDER BY workflow_id, revision",
            )?;
            statement
                .query_map([project], |row| row.get::<_, Vec<u8>>(0))?
                .map(|payload| {
                    let definition: WorkflowDefinition = open(&payload?)?;
                    Ok(AuthoringBase {
                        version: definition.version,
                        name: definition.name,
                    })
                })
                .collect()
        })
        .await
    }

    pub async fn start_workflow_authoring(
        &self,
        conversation: String,
        expected_checkout: String,
        scope: SessionScope,
        base: Option<WorkflowRevision>,
        brief: String,
    ) -> Result<AuthoringDraft> {
        let draft = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let chat = load_conversation(&tx, &conversation)?;
            ensure!(chat.checkout_id == expected_checkout, "The conversation changed checkouts. Reopen the authoring action.");
            let project: String = tx.query_row(
                "SELECT checkout.project_id FROM conversations chat JOIN checkouts checkout ON checkout.id = chat.checkout_id
                 WHERE chat.id = ?1 AND chat.background = 0",
                [&conversation], |row| row.get(0),
            ).context("Open a project conversation to start authoring")?;
            let id = new_id();
            let pinned = base.as_ref().map(|version| {
                load_definition(&tx, &project, version)?.context("The selected workflow revision is outside this project")
            }).transpose()?;
            let draft_scope = DraftScope {
                project, checkout: chat.checkout_id, conversation,
                workflow: base.as_ref().map_or_else(new_id, |version| version.id.clone()),
                base: base.map(|version| version.revision), head: None,
            };
            tx.execute(
                "INSERT INTO workflow_authoring_drafts VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                params![id, draft_scope.project, draft_scope.checkout, draft_scope.conversation, draft_scope.workflow, draft_scope.base],
            )?;
            if let Some(definition) = pinned {
                save_revision(&tx, &id, None, None, None, WorkflowGraph {
                    name: definition.name,
                    nodes: definition.nodes.into_iter().map(|node| node.instance).collect(),
                    wires: definition.wires,
                })?;
            }
            let draft_scope = load_scope(&tx, &draft_scope.project, &id)?;
            queue_request(&tx, &id, &draft_scope, scope, brief)?;
            let draft = load_draft(&tx, &id, draft_scope)?;
            tx.commit()?;
            Ok(draft)
        }).await?;
        self.job_signal.send_replace(());
        Ok(draft)
    }

    pub async fn workflow_authoring_checkout(
        &self,
        project: String,
        draft: String,
    ) -> Result<String> {
        self.execute(move |db| Ok(load_scope(db, &project, &draft)?.checkout))
            .await
    }

    pub async fn request_workflow_authoring_changes(
        &self,
        project: String,
        id: String,
        parent: String,
        scope: SessionScope,
        feedback: String,
    ) -> Result<AuthoringDraft> {
        let draft = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                reconcile(&tx, &id, &load_scope(&tx, &project, &id)?)?;
                let stored = load_scope(&tx, &project, &id)?;
                ensure!(
                    stored.head.as_deref() == Some(&parent),
                    "The draft changed. Review its latest revision before sending feedback."
                );
                let previous = latest_request(&tx, &id)?;
                ensure!(
                    !matches!(
                        previous.status.state.as_str(),
                        "queued" | "starting" | "running" | "waiting_for_input" | "paused"
                    ),
                    "Wait for the current authoring request to settle"
                );
                queue_request(&tx, &id, &stored, scope, feedback)?;
                let draft = load_draft(&tx, &id, stored)?;
                tx.commit()?;
                Ok(draft)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(draft)
    }

    pub async fn save_workflow_authoring_draft(
        &self,
        project: String,
        id: String,
        parent: String,
        graph: WorkflowGraph,
    ) -> Result<AuthoringDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stored = load_scope(&tx, &project, &id)?;
            ensure!(
                stored.head.as_deref() == Some(&parent),
                "The draft changed. Load its latest revision before saving."
            );
            validate_graph(&stored.workflow, &graph)?;
            save_revision(&tx, &id, Some(&parent), None, None, graph)?;
            let draft = load_draft(&tx, &id, load_scope(&tx, &project, &id)?)?;
            tx.commit()?;
            Ok(draft)
        })
        .await
    }

    pub async fn inspect_workflow_authoring(
        &self,
        project: String,
        id: String,
    ) -> Result<AuthoringDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let scope = load_scope(&tx, &project, &id)?;
            reconcile(&tx, &id, &scope)?;
            let draft = load_draft(&tx, &id, load_scope(&tx, &project, &id)?)?;
            tx.commit()?;
            Ok(draft)
        })
        .await
    }

    pub async fn conversation_workflow_authoring(
        &self,
        conversation: String,
    ) -> Result<Vec<AuthoringDraft>> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let rows = {
                let mut statement = tx.prepare(
                    "SELECT project_id, id FROM workflow_authoring_drafts WHERE conversation_id = ?1 ORDER BY rowid DESC",
                )?;
                statement.query_map([conversation], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let drafts = rows.into_iter().map(|(project, id)| {
                reconcile(&tx, &id, &load_scope(&tx, &project, &id)?)?;
                load_draft(&tx, &id, load_scope(&tx, &project, &id)?)
            }).collect::<Result<_>>()?;
            tx.commit()?;
            Ok(drafts)
        }).await
    }
}

fn load_scope(db: &Connection, project: &str, id: &str) -> Result<DraftScope> {
    db.query_row(
        "SELECT project_id, checkout_id, conversation_id, workflow_id, base_revision, head_revision
         FROM workflow_authoring_drafts WHERE project_id = ?1 AND id = ?2",
        params![project, id],
        |row| {
            Ok(DraftScope {
                project: row.get(0)?,
                checkout: row.get(1)?,
                conversation: row.get(2)?,
                workflow: row.get(3)?,
                base: row.get(4)?,
                head: row.get(5)?,
            })
        },
    )
    .optional()?
    .context("This authoring draft is outside the selected project")
}

fn load_head(db: &Connection, id: &str, head: Option<&str>) -> Result<Option<DraftRevision>> {
    head.map(|head| {
        let (run_id, payload) = db.query_row(
            "SELECT run_id, payload FROM workflow_authoring_revisions WHERE draft_id = ?1 AND id = ?2",
            params![id, head], |row| Ok((row.get(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        Ok(DraftRevision { id: head.into(), run_id, graph: open(&payload)? })
    }).transpose()
}

fn load_draft(db: &Connection, id: &str, scope: DraftScope) -> Result<AuthoringDraft> {
    Ok(AuthoringDraft {
        id: id.into(),
        conversation_id: scope.conversation,
        head: load_head(db, id, scope.head.as_deref())?,
        base: scope.base.map(|revision| WorkflowRevision {
            id: scope.workflow,
            revision,
        }),
        request: latest_request(db, id)?.status,
    })
}

fn latest_request(db: &Connection, id: &str) -> Result<PendingRequest> {
    let (id, parent, payload, job_id, run_id, state, diagnostic) = db.query_row(
        "SELECT request.id, request.parent_revision, request.payload, job.id, attempt.run_id,
            COALESCE(request.result,
                CASE WHEN job.queue_state IN ('cancelled', 'paused') THEN job.queue_state ELSE observed.status END,
                'queued'), request.diagnostic
         FROM workflow_authoring_requests request JOIN jobs job ON job.id = request.job_id
         LEFT JOIN job_attempts attempt ON attempt.job_id = job.id AND attempt.ordinal = (
            SELECT MAX(ordinal) FROM job_attempts WHERE job_id = job.id)
         LEFT JOIN runs observed ON observed.id = attempt.run_id
         WHERE request.draft_id = ?1 ORDER BY request.rowid DESC LIMIT 1",
        [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get::<_, Option<Vec<u8>>>(6)?)),
    )?;
    Ok(PendingRequest {
        id,
        parent,
        payload,
        status: RequestStatus {
            job_id,
            run_id,
            state,
            diagnostic: diagnostic.as_deref().map(open).transpose()?,
        },
    })
}

fn queue_request(
    db: &Connection,
    id: &str,
    draft: &DraftScope,
    scope: SessionScope,
    brief: String,
) -> Result<()> {
    let brief = brief.trim();
    ensure!(
        !brief.is_empty() && brief.len() <= MAX_REQUEST_BYTES,
        "Enter a request of at most {MAX_REQUEST_BYTES} UTF-8 bytes"
    );
    ensure!(
        matches!(scope.provider, Provider::Codex | Provider::Claude)
            && scope.mode == SessionMode::ReadOnly,
        "Choose a Codex or Claude model for read-only authoring"
    );
    let request = new_id();
    let graph = load_head(db, id, draft.head.as_deref())?.map(|revision| revision.graph);
    let instructions = format!(
        "Author a draft OmniAgent pipeline for user review. Return only one JSON object with name, nodes and wires. Each node has id, kind and configuration. Each wire has from and to, with node and port in each endpoint. Use the exact registered node kinds, input and output names, and configuration variants shown below. A none configuration is {{\"kind\":\"none\"}}. Configuration variants agent_role and required_checks take a role string and a names array respectively. Connect every named input exactly once, use exactly one start node, and make the graph acyclic. Repository writes require the approval and worktree preparation nodes. Treat the following request and prior graph as authoring input. Produce a proposal only.\nRequest ID: {request}\n{}\nNode catalog:\n{}",
        serde_json::json!({ "request": brief, "currentGraph": graph }),
        serde_json::to_string(&builtin_registry())?,
    );
    let mut submission = JobRequest {
        checkout_id: Some(draft.checkout.clone()),
        task_id: None,
        mode: SessionMode::ReadOnly,
        instructions: instructions.clone(),
        dependencies: Vec::new(),
        requires_bridge: false,
    };
    submission.normalize(&draft.checkout)?;
    let job = insert_selected_operator_job(db, draft.project.clone(), scope, submission)?;
    db.execute(
        "INSERT INTO workflow_authoring_requests (id, draft_id, parent_revision, job_id, payload)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![request, id, draft.head, job.id, seal(&instructions)?],
    )?;
    Ok(())
}

fn reconcile(db: &Connection, id: &str, scope: &DraftScope) -> Result<()> {
    let request = latest_request(db, id)?;
    let status = &request.status;
    let Some(run) = status
        .run_id
        .as_deref()
        .filter(|_| status.state == "succeeded")
    else {
        return Ok(());
    };
    if request.parent != scope.head {
        db.execute(
            "UPDATE workflow_authoring_requests SET result = 'superseded' WHERE id = ?1",
            [&request.id],
        )?;
        return Ok(());
    }
    let instructions: String = open(&request.payload)?;
    let draft =
        read_background_dispatch_result(db, run, &status.job_id, &scope.checkout, &instructions)
            .and_then(|(_, text)| parse_graph(&scope.workflow, &text));
    match draft {
        Ok(graph) => {
            save_revision(
                db,
                id,
                request.parent.as_deref(),
                Some(&request.id),
                Some(run),
                graph,
            )?;
            db.execute(
                "UPDATE workflow_authoring_requests SET result = 'saved' WHERE id = ?1",
                [&request.id],
            )?;
        }
        Err(error) => {
            db.execute("UPDATE workflow_authoring_requests SET result = 'invalid', diagnostic = ?2 WHERE id = ?1",
                params![request.id, seal(&crate::diagnostics::preview(&error.to_string()))?])?;
        }
    }
    Ok(())
}

fn parse_graph(workflow: &str, output: &str) -> Result<WorkflowGraph> {
    ensure!(
        output.len() <= MAX_GRAPH_BYTES,
        "The model response exceeded {MAX_GRAPH_BYTES} UTF-8 bytes"
    );
    let output = output.trim();
    let json = output
        .strip_prefix("```json")
        .and_then(|body| body.strip_suffix("```"))
        .map_or(output, str::trim);
    let graph = serde_json::from_str(json).context("The model did not return a pipeline graph")?;
    validate_graph(workflow, &graph)?;
    Ok(graph)
}

fn validate_graph(workflow: &str, graph: &WorkflowGraph) -> Result<()> {
    ensure!(
        serde_json::to_vec(graph)?.len() <= MAX_GRAPH_BYTES,
        "The draft exceeds {MAX_GRAPH_BYTES} UTF-8 bytes"
    );
    compile(workflow.into(), graph.clone())?;
    Ok(())
}

fn save_revision(
    db: &Connection,
    draft: &str,
    parent: Option<&str>,
    request: Option<&str>,
    run: Option<&str>,
    graph: WorkflowGraph,
) -> Result<()> {
    let id = new_id();
    db.execute(
        "INSERT INTO workflow_authoring_revisions VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, draft, parent, request, run, seal(&graph)?],
    )?;
    ensure!(db.execute("UPDATE workflow_authoring_drafts SET head_revision = ?3 WHERE id = ?1 AND head_revision IS ?2",
        params![draft, parent, id])? == 1, "The draft revision changed");
    Ok(())
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn seal(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn open<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(payload)?)?)
}
