use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{protection, providers::capabilities::revision, store::Store};

use super::ContextRecordContent;

const PROMPT_HEADING: &str = "\n\nAssembled context\n";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextVersionRef {
    pub record_id: String,
    pub revision_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "source",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ContextInputRevision {
    Instruction {
        instruction_id: String,
        revision_id: String,
    },
    Record {
        version: ContextVersionRef,
        revision: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBlock {
    pub revision: ContextInputRevision,
    pub title: String,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequiredContextInstruction {
    pub instruction_id: String,
    pub revision_id: String,
    pub title: String,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleContextRequest {
    pub project_id: String,
    pub task_id: Option<String>,
    pub role: String,
    pub available_bytes: u64,
    pub objective: String,
    pub required: Vec<RequiredContextInstruction>,
    pub explicit_links: Vec<ContextVersionRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDecisionReason {
    RequiredInstruction,
    ExplicitLink,
    RelevantApprovedRecord,
    OutOfBudget,
    NoSearchMatch,
    AlreadyLinked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDecision {
    pub revision: ContextInputRevision,
    pub title: String,
    pub selection_order: Option<usize>,
    pub reason: ContextDecisionReason,
    pub encoded_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssembledRoleContext {
    pub role: String,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub blocks: Vec<ContextBlock>,
    pub decisions: Vec<ContextDecision>,
}

impl AssembledRoleContext {
    pub fn prompt_suffix(&self) -> Result<String> {
        if self.blocks.is_empty() {
            return Ok(String::new());
        }
        Ok(format!(
            "{PROMPT_HEADING}{}",
            serde_json::to_string(&self.blocks)?
        ))
    }

    fn consider(&mut self, block: ContextBlock, reason: ContextDecisionReason) -> Result<()> {
        let encoded_bytes = block_bytes(&block)?;
        let needed = self
            .used_bytes
            .saturating_add(encoded_bytes)
            .saturating_add(if self.blocks.is_empty() {
                framing_bytes()
            } else {
                1
            });
        let included = needed <= self.available_bytes;
        self.decisions.push(ContextDecision {
            revision: block.revision.clone(),
            title: block.title.clone(),
            encoded_bytes,
            selection_order: included.then(|| self.blocks.len()),
            reason: if included {
                reason
            } else {
                ContextDecisionReason::OutOfBudget
            },
        });
        if included {
            self.used_bytes = needed;
            self.blocks.push(block);
        }
        Ok(())
    }

    fn omit(&mut self, block: ContextBlock, reason: ContextDecisionReason) -> Result<()> {
        let encoded_bytes = block_bytes(&block)?;
        self.decisions.push(ContextDecision {
            revision: block.revision,
            title: block.title,
            selection_order: None,
            reason,
            encoded_bytes,
        });
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequiredContextOverBudget {
    pub role: String,
    pub available_bytes: u64,
    pub required_bytes: u64,
    pub required_inputs: Vec<ContextDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleContextAssessment {
    Ready(AssembledRoleContext),
    RequiredContextOverBudget(RequiredContextOverBudget),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleContextPolicy {
    pub revision_id: String,
    pub role: String,
    pub available_bytes: u64,
    pub required: Vec<RequiredContextInstruction>,
    pub explicit_links: Vec<ContextVersionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleDispatchContext {
    pub policy_revision_id: String,
    pub policy_digest: String,
    pub selection: AssembledRoleContext,
}

pub(crate) enum DispatchRoleContext {
    Unconfigured,
    Assembled(RoleDispatchContext),
    OverBudget(RequiredContextOverBudget),
}

pub(crate) fn bind_role_context(
    db: &Connection,
    job: &str,
    policy: &RoleContextPolicy,
) -> Result<()> {
    ensure!(
        !policy.revision_id.trim().is_empty() && !policy.role.trim().is_empty(),
        "role context requires the immutable revision of a named role"
    );
    ensure!(
        policy
            .required
            .iter()
            .all(|item| !item.instruction_id.trim().is_empty()
                && !item.revision_id.trim().is_empty()),
        "required instructions need stable identities and revisions"
    );
    let encrypted = protection::protect(&serde_json::to_vec(policy)?)?;
    db.execute(
        "INSERT INTO job_role_contexts (job_id, policy) VALUES (?1, ?2)",
        params![job, encrypted],
    )?;
    Ok(())
}

impl Store {
    pub(crate) async fn dispatch_role_context(
        &self,
        run: String,
        objective: String,
    ) -> Result<DispatchRoleContext> {
        let pinned = self.execute(move |db| {
            let row = db.query_row(
                "SELECT j.project_id, coalesce(shared.scope_task_id, j.task_id), bound.policy
                 FROM job_attempts attempt JOIN jobs j ON j.id = attempt.job_id
                 JOIN job_role_contexts bound ON bound.job_id = j.id
                 LEFT JOIN job_worktree_scopes shared ON shared.job_id = j.id
                 WHERE attempt.run_id = ?1",
                [run], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Vec<u8>>(2)?)),
            ).optional()?;
            row.map(|(project, task, encrypted)| {
                let policy: RoleContextPolicy = serde_json::from_slice(&protection::unprotect(&encrypted)?)?;
                Ok((project, task, policy))
            }).transpose()
        }).await?;
        let Some((project_id, task_id, policy)) = pinned else {
            return Ok(DispatchRoleContext::Unconfigured);
        };
        let policy_digest = revision(&policy)?;
        let result = self
            .assemble_role_context(RoleContextRequest {
                project_id,
                task_id,
                role: policy.role,
                available_bytes: policy.available_bytes,
                objective,
                required: policy.required,
                explicit_links: policy.explicit_links,
            })
            .await?;
        match result {
            RoleContextAssessment::Ready(selection) => {
                Ok(DispatchRoleContext::Assembled(RoleDispatchContext {
                    policy_revision_id: policy.revision_id,
                    policy_digest,
                    selection,
                }))
            }
            RoleContextAssessment::RequiredContextOverBudget(blocker) => {
                Ok(DispatchRoleContext::OverBudget(blocker))
            }
        }
    }
}

impl Store {
    pub async fn assemble_role_context(
        &self,
        request: RoleContextRequest,
    ) -> Result<RoleContextAssessment> {
        ensure!(
            !request.role.trim().is_empty(),
            "context assembly requires a role"
        );
        let required: Vec<_> = request
            .required
            .into_iter()
            .map(|instruction| ContextBlock {
                revision: ContextInputRevision::Instruction {
                    instruction_id: instruction.instruction_id,
                    revision_id: instruction.revision_id,
                },
                title: instruction.title,
                markdown: instruction.markdown,
            })
            .collect();
        let required_bytes = encoded_required_bytes(&required)?;
        if required_bytes > request.available_bytes {
            let inputs = required
                .into_iter()
                .map(|block| {
                    let encoded_bytes = block_bytes(&block)?;
                    Ok(ContextDecision {
                        revision: block.revision,
                        title: block.title,
                        encoded_bytes,
                        selection_order: None,
                        reason: ContextDecisionReason::OutOfBudget,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok(RoleContextAssessment::RequiredContextOverBudget(
                RequiredContextOverBudget {
                    role: request.role,
                    available_bytes: request.available_bytes,
                    required_bytes,
                    required_inputs: inputs,
                },
            ));
        }
        let (explicit, approved) = self
            .execute({
                let project = request.project_id;
                let task = request.task_id;
                let links = request.explicit_links;
                move |db| load_context_candidates(db, &project, task.as_deref(), links)
            })
            .await?;
        tokio::task::spawn_blocking(move || {
            choose_context(
                request.role,
                request.objective,
                request.available_bytes,
                required,
                explicit,
                approved,
            )
        })
        .await?
    }
}

fn load_context_candidates(
    db: &mut Connection,
    project: &str,
    task: Option<&str>,
    links: Vec<ContextVersionRef>,
) -> Result<(Vec<ContextBlock>, Vec<ContextBlock>)> {
    let tx = db.transaction()?;
    let known_scope: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1) AND (?2 IS NULL OR EXISTS(
            SELECT 1 FROM project_tasks WHERE project_id = ?1 AND id = ?2
         ))",
        params![project, task],
        |row| row.get(0),
    )?;
    ensure!(
        known_scope,
        "context assembly requires a project-owned scope"
    );
    let parent: Option<String> = match task {
        Some(task) => tx.query_row(
            "SELECT parent_task_id FROM project_task_memberships WHERE project_id = ?1 AND task_id = ?2",
            params![project, task], |row| row.get(0),
        ).optional()?.flatten(),
        None => None,
    };
    let mut explicit = Vec::new();
    let mut explicit_query = tx.prepare(
        "SELECT v.record_id, v.id, v.revision, v.content FROM project_context_revisions v
         JOIN project_context_records r ON r.project_id = v.project_id AND r.id = v.record_id
         WHERE r.project_id = ?1 AND (r.task_id IS NULL OR r.task_id IN (?2, ?3))
         AND v.record_id = ?4 AND v.id = ?5",
    )?;
    for link in links {
        let row = explicit_query
            .query_row(
                params![project, task, parent, link.record_id, link.revision_id],
                read_candidate,
            )
            .optional()?
            .context("an explicitly linked context revision is unavailable in this scope")?;
        explicit.push(decrypt_candidate(row)?);
    }
    let mut approved_query = tx.prepare(
        "SELECT v.record_id, v.id, v.revision, v.content FROM project_context_revisions v
         JOIN project_context_records r ON r.project_id = v.project_id AND r.id = v.record_id
         WHERE r.project_id = ?1 AND (r.task_id IS NULL OR r.task_id IN (?2, ?3)) AND v.status = 'accepted'
         ORDER BY v.record_id, v.revision",
    )?;
    let approved = approved_query
        .query_map(params![project, task, parent], read_candidate)?
        .map(|row| decrypt_candidate(row?))
        .collect::<Result<Vec<_>>>()?;
    Ok((explicit, approved))
}

type ContextCandidate = (ContextVersionRef, i64, Vec<u8>);

fn read_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContextCandidate> {
    Ok((
        ContextVersionRef {
            record_id: row.get(0)?,
            revision_id: row.get(1)?,
        },
        row.get(2)?,
        row.get(3)?,
    ))
}

fn decrypt_candidate((version, revision, encrypted): ContextCandidate) -> Result<ContextBlock> {
    let content: ContextRecordContent =
        serde_json::from_slice(&protection::unprotect(&encrypted)?)?;
    Ok(ContextBlock {
        revision: ContextInputRevision::Record { version, revision },
        title: content.title,
        markdown: content.markdown,
    })
}

fn choose_context(
    role: String,
    objective: String,
    available_bytes: u64,
    required: Vec<ContextBlock>,
    explicit: Vec<ContextBlock>,
    approved: Vec<ContextBlock>,
) -> Result<RoleContextAssessment> {
    let matching = relevant_records(&objective, &approved)?;
    let mut result = AssembledRoleContext {
        role,
        available_bytes,
        used_bytes: 0,
        blocks: Vec::new(),
        decisions: Vec::new(),
    };
    let mut explicit_records = HashSet::new();
    for block in required {
        result.consider(block, ContextDecisionReason::RequiredInstruction)?;
    }
    for block in explicit {
        let ContextInputRevision::Record { version, .. } = &block.revision else {
            unreachable!("an explicit context link loads a saved record")
        };
        if explicit_records.insert(version.clone()) {
            result.consider(block, ContextDecisionReason::ExplicitLink)?;
        } else {
            result.omit(block, ContextDecisionReason::AlreadyLinked)?;
        }
    }
    let mut candidates: Vec<_> = approved.into_iter().map(Some).collect();
    for index in matching {
        let block = candidates[index]
            .take()
            .context("search repeated a context revision")?;
        let ContextInputRevision::Record { version, .. } = &block.revision else {
            unreachable!("approved context loads saved records")
        };
        if explicit_records.contains(version) {
            result.omit(block, ContextDecisionReason::AlreadyLinked)?;
        } else {
            result.consider(block, ContextDecisionReason::RelevantApprovedRecord)?;
        }
    }
    for block in candidates.into_iter().flatten() {
        result.omit(block, ContextDecisionReason::NoSearchMatch)?;
    }
    Ok(RoleContextAssessment::Ready(result))
}

fn relevant_records(objective: &str, candidates: &[ContextBlock]) -> Result<Vec<usize>> {
    let mut seen = HashSet::new();
    let terms: Vec<_> = objective
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| !term.is_empty() && seen.insert(term.to_lowercase()))
        .map(|term| format!("\"{term}\""))
        .collect();
    if terms.is_empty() || candidates.is_empty() {
        return Ok(Vec::new());
    }
    // Content is indexed only for this selection and never written to the application database.
    let mut ranking = Connection::open_in_memory()?;
    ranking.pragma_update(None, "temp_store", "MEMORY")?;
    let tx = ranking.transaction()?;
    tx.execute_batch("CREATE VIRTUAL TABLE context_search USING fts5(title, markdown)")?;
    {
        let mut insert =
            tx.prepare("INSERT INTO context_search(rowid, title, markdown) VALUES (?1, ?2, ?3)")?;
        for (index, block) in candidates.iter().enumerate() {
            insert.execute(params![(index + 1) as i64, block.title, block.markdown])?;
        }
    }
    let mut query = tx.prepare(
        "SELECT rowid FROM context_search WHERE context_search MATCH ?1 ORDER BY bm25(context_search), rowid",
    )?;
    let indexes = query
        .query_map([terms.join(" OR ")], |row| row.get::<_, i64>(0))?
        .map(|row| Ok(usize::try_from(row? - 1)?))
        .collect::<Result<Vec<_>>>()?;
    Ok(indexes)
}

fn block_bytes(block: &ContextBlock) -> Result<u64> {
    Ok(serde_json::to_vec(block)?.len() as u64)
}

fn framing_bytes() -> u64 {
    (PROMPT_HEADING.len() + 2) as u64
}

fn encoded_required_bytes(blocks: &[ContextBlock]) -> Result<u64> {
    if blocks.is_empty() {
        return Ok(0);
    }
    let message_bytes = serde_json::to_vec(blocks)?.len() as u64;
    Ok(message_bytes.saturating_add(PROMPT_HEADING.len() as u64))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::{
        context_records::{
            ContextProvenance, ContextRecordKind, ContextReviewDecision, ContextReviewStatus,
        },
        repository,
    };

    #[tokio::test]
    async fn budgets_role_instructions_and_links_before_ranked_approved_context() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let checkout = directory.path().join("repo");
        std::fs::create_dir(&checkout)?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "--quiet"])
                .current_dir(&checkout)
                .status()?
                .success()
        );
        let path = directory.path().join("context.sqlite");
        let store = Store::open(path.clone())?;
        let project = repository::register(&store, checkout).await?.project_id;
        let parent = store
            .create_project_task(project.clone(), "API".into())
            .await?;
        let child = store
            .create_project_subtask(project.clone(), parent.id.clone(), "Review response".into())
            .await?;
        let sibling = store
            .create_project_task(project.clone(), "Other task".into())
            .await?;
        let propose =
            |title: &'static str, markdown: String, task: Option<String>, accept: bool| {
                let store = store.clone();
                let project = project.clone();
                async move {
                    let record = store
                        .propose_context_record(
                            project.clone(),
                            task,
                            ContextRecordKind::Note,
                            ContextRecordContent {
                                title: title.into(),
                                markdown,
                            },
                            ContextProvenance::Operator,
                        )
                        .await?;
                    if accept {
                        store
                            .review_context_revision(
                                project,
                                record.record_id.clone(),
                                record.id.clone(),
                                ContextReviewStatus::Proposed,
                                ContextReviewDecision::Accept,
                            )
                            .await?;
                    }
                    Ok::<_, anyhow::Error>(ContextVersionRef {
                        record_id: record.record_id,
                        revision_id: record.id,
                    })
                }
            };
        let linked = propose(
            "Explicit revision",
            "A source selected by the operator".into(),
            None,
            false,
        )
        .await?;
        let relevant = propose(
            "SQLite audit policy",
            "audit SQLite writes\n".repeat(24),
            Some(parent.id),
            true,
        )
        .await?;
        let irrelevant = propose("Team calendar", "Tuesday lunch".into(), None, true).await?;
        let unapproved =
            propose("Draft audit rule", "SQLite audit draft".into(), None, false).await?;
        let outside = propose(
            "Unrelated task",
            "audit SQLite here too".into(),
            Some(sibling.id),
            true,
        )
        .await?;
        let input = RoleContextRequest {
            project_id: project,
            task_id: Some(child.id),
            role: "reviewer".into(),
            available_bytes: 16_384,
            objective: "Review the ‘SQLite audit’".into(),
            required: vec![RequiredContextInstruction {
                instruction_id: "review-rules".into(),
                revision_id: "review-rules-r1".into(),
                title: "Required role instruction".into(),
                markdown: "Verify the response\r\nKeep existing errors".into(),
            }],
            explicit_links: vec![linked.clone()],
        };
        let RoleContextAssessment::Ready(full) = store.assemble_role_context(input.clone()).await?
        else {
            anyhow::bail!("the full context should fit")
        };
        assert_eq!(full.role, "reviewer");
        assert_eq!(full.prompt_suffix()?.len() as u64, full.used_bytes);
        let choices: Vec<_> = full
            .decisions
            .iter()
            .map(|item| (item.reason, item.selection_order))
            .collect();
        assert_eq!(
            choices,
            [
                (ContextDecisionReason::RequiredInstruction, Some(0)),
                (ContextDecisionReason::ExplicitLink, Some(1)),
                (ContextDecisionReason::RelevantApprovedRecord, Some(2)),
                (ContextDecisionReason::NoSearchMatch, None),
            ]
        );
        assert_eq!(
            full.blocks[1].revision,
            ContextInputRevision::Record {
                version: linked,
                revision: 1
            }
        );
        assert_eq!(
            full.blocks[2].revision,
            ContextInputRevision::Record {
                version: relevant,
                revision: 1
            }
        );
        assert_eq!(
            full.decisions[3].revision,
            ContextInputRevision::Record {
                version: irrelevant,
                revision: 1
            }
        );
        for hidden in [unapproved, outside.clone()] {
            assert!(!full.decisions.iter().any(|item| item.revision
                == ContextInputRevision::Record {
                    version: hidden.clone(),
                    revision: 1
                }));
        }

        let available = full.used_bytes - full.decisions[2].encoded_bytes - 1;
        let RoleContextAssessment::Ready(fitted) = store
            .assemble_role_context(RoleContextRequest {
                available_bytes: available,
                ..input.clone()
            })
            .await?
        else {
            anyhow::bail!("the required inputs should fit")
        };
        assert_eq!(fitted.blocks, full.blocks[..2]);
        assert_eq!(fitted.used_bytes, available);
        assert_eq!(
            fitted.decisions[2].reason,
            ContextDecisionReason::OutOfBudget
        );
        let RoleContextAssessment::RequiredContextOverBudget(blocked) = store
            .assemble_role_context(RoleContextRequest {
                available_bytes: 10,
                ..input.clone()
            })
            .await?
        else {
            anyhow::bail!("the mandatory instruction should block dispatch")
        };
        assert_eq!(
            blocked.required_bytes,
            full.decisions[0].encoded_bytes + framing_bytes()
        );
        assert_eq!(blocked.required_inputs[0].revision, full.blocks[0].revision);
        assert!(
            store
                .assemble_role_context(RoleContextRequest {
                    explicit_links: vec![outside],
                    ..input
                })
                .await
                .is_err()
        );

        let db = Connection::open(path)?;
        let indexed_in_profile: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'context_search')",
            [],
            |row| row.get(0),
        )?;
        assert!(!indexed_in_profile);
        Ok(())
    }
}
