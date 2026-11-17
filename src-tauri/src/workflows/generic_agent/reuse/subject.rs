use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::{
    protection,
    workflows::{
        ArtifactKind, ArtifactValue, BuiltinNode, NodeInputs, PinnedNode, WorkflowArtifact,
        WorkflowDefinition, WorkflowRun,
        artifacts::load_artifact,
        authoring::package::PackageArtifactVersion,
        control::{ControlStageReceipt, ControlStageState},
        data_nodes::{EvidenceOutcome, evidence_text},
        digest,
        subflow::WorkflowSubflow,
    },
};

pub(super) struct Round<'a> {
    pub root: &'a str,
    pub ordinal: u16,
    pub version: &'a PackageArtifactVersion,
    path: &'a str,
}

impl<'a> Round<'a> {
    pub(super) fn for_node(definition: &'a WorkflowDefinition, node: &str) -> Option<Self> {
        definition
            .subflows
            .iter()
            .filter_map(|scope| {
                let (root, number) = scope.path.rsplit_once("::iteration-")?;
                let ordinal = number.parse::<u16>().ok()?;
                if !(1..=32).contains(&ordinal)
                    || ordinal.to_string() != number
                    || !scope.nodes.iter().any(|id| id == node)
                    || !is_repeat_round(definition, root, scope, ordinal)
                {
                    return None;
                }
                Some(Self {
                    root,
                    ordinal,
                    version: &scope.version,
                    path: &scope.path,
                })
            })
            .max_by_key(|round| round.path.len())
    }

    pub(super) fn previous(
        &self,
        definition: &'a WorkflowDefinition,
        node: &str,
    ) -> Option<String> {
        let previous = self.ordinal.checked_sub(1).filter(|&ordinal| ordinal > 0)?;
        let relative = node.strip_prefix(self.path)?.strip_prefix("::")?;
        let qualified = format!("{}::iteration-{previous}::{relative}", self.root);
        let round = Self::for_node(definition, &qualified)?;
        (round.root == self.root && round.ordinal == previous && round.version == self.version)
            .then_some(qualified)
    }

    pub(super) fn normalize(&self, node: &str) -> String {
        node.strip_prefix(self.path)
            .and_then(|relative| relative.strip_prefix("::"))
            .map_or_else(
                || node.into(),
                |relative| format!("{}::iteration::{relative}", self.root),
            )
    }
}

fn is_repeat_round(
    definition: &WorkflowDefinition,
    root: &str,
    scope: &WorkflowSubflow,
    ordinal: u16,
) -> bool {
    let condition = format!("{root}::condition-{ordinal}");
    let limit = format!("{root}::limit");
    definition.subflows.iter().any(|parent| {
        parent.path == root
            && parent.nodes.iter().any(|node| node == &condition)
            && scope.nodes.iter().all(|node| parent.nodes.contains(node))
    }) && definition
        .nodes
        .iter()
        .any(|node| node.instance.id == condition && node.instance.kind == BuiltinNode::Condition)
        && definition.nodes.iter().any(|node| {
            node.instance.id == limit && node.instance.kind == BuiltinNode::IterationLimit
        })
        && definition.wires.iter().any(|wire| {
            wire.to.node == condition
                && wire.to.port == "ready"
                && scope.nodes.contains(&wire.from.node)
        })
}

pub(super) fn input_key(inputs: &NodeInputs) -> Result<String> {
    Ok(digest(&serde_json::to_vec(inputs)?))
}

pub(super) fn completion_inputs(
    db: &Connection,
    run: &WorkflowRun,
    artifact: &str,
) -> Result<Vec<NodeInputs>> {
    let mut query = db.prepare(
        "SELECT input_revision, payload FROM workflow_completion_inputs WHERE output_artifact_id = ?1 AND execution_key = ?2",
    )?;
    query
        .query_map(
            params![artifact, run.plan_revision.as_deref().unwrap_or_default()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?
        .map(|row| {
            let (revision, protected) = row?;
            let bytes = protection::unprotect(&protected)?;
            ensure!(
                digest(&bytes) == revision,
                "The retained review inputs changed"
            );
            Ok(serde_json::from_slice(&bytes)?)
        })
        .collect()
}

pub(super) fn fingerprint(
    db: &Connection,
    run: &WorkflowRun,
    definition: &WorkflowDefinition,
    round: &Round<'_>,
    unit: Option<&str>,
    inputs: &NodeInputs,
    receipts: &[ControlStageReceipt],
) -> Result<String> {
    ensure!(
        inputs.len() == 2,
        "Reused review requires only the paired native prompt and subject"
    );
    let subject = load_artifact(
        db,
        &run.project_id,
        &run.id,
        inputs
            .get("review_subject")
            .context("Review subject missing")?,
        ArtifactKind::EvidenceSet,
    )?
    .context("Review subject unavailable")?;
    let prompt = load_artifact(
        db,
        &run.project_id,
        &run.id,
        inputs.get("prompt").context("Review prompt missing")?,
        ArtifactKind::RenderedText,
    )?
    .context("Review prompt unavailable")?;
    let ArtifactValue::EvidenceSet(entries) = &subject.value else {
        unreachable!()
    };
    ensure!(
        subject.node_instance_id == prompt.node_instance_id
            && subject.unit_id == prompt.unit_id
            && subject.port == "evidence"
            && prompt.port == "text"
            && prompt.value == ArtifactValue::RenderedText(evidence_text(entries)?),
        "The prompt and subject are not the two outputs of the same native evidence collection"
    );
    let records = completion_inputs(db, run, &subject.id)?;
    ensure!(
        records.len() == 1 && completion_inputs(db, run, &prompt.id)? == records,
        "The review prompt and subject have different native input pins"
    );
    let mut verifier = Subject {
        db,
        run,
        definition,
        round,
        receipts,
        unit,
        remaining: 1024,
    };
    let mut value = verifier.value(&subject, 0)?;
    canonicalize(&mut value);
    Ok(digest(&serde_json::to_vec(&value)?))
}

fn canonicalize(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.sort_keys();
            object.values_mut().for_each(canonicalize);
        }
        Value::Array(array) => array.iter_mut().for_each(canonicalize),
        _ => {}
    }
}

struct Subject<'a, 'b> {
    db: &'a Connection,
    run: &'a WorkflowRun,
    definition: &'a WorkflowDefinition,
    round: &'a Round<'b>,
    receipts: &'a [ControlStageReceipt],
    unit: Option<&'a str>,
    remaining: usize,
}

impl Subject<'_, '_> {
    fn node(&self, id: &str) -> Result<&PinnedNode> {
        self.definition
            .nodes
            .iter()
            .find(|node| node.instance.id == id)
            .context("A review source is outside its pinned definition")
    }

    fn provenance(&self, id: &str, port: &str) -> Result<Value> {
        let node = self.node(id)?;
        Ok(json!([
            self.round.normalize(id),
            port,
            node.definition.revision,
            node.instance.configuration
        ]))
    }

    fn receipt(&self, node: &str, unit: Option<&str>) -> Option<&ControlStageReceipt> {
        self.receipts.iter().find(|receipt| {
            receipt.node_id == node
                && receipt.unit_id.as_deref() == unit
                && receipt.plan_revision == self.run.plan_revision
        })
    }

    fn value(&mut self, artifact: &WorkflowArtifact, depth: usize) -> Result<Value> {
        ensure!(
            depth <= 16 && self.remaining > 0,
            "Review subjects exceed the evidence traversal limit"
        );
        self.remaining -= 1;
        ensure!(
            artifact.workflow_run_id == self.run.id
                && (artifact.unit_id.is_none() || artifact.unit_id.as_deref() == self.unit),
            "A review source belongs to another run or logical unit"
        );
        let producer = self.provenance(&artifact.node_instance_id, &artifact.port)?;
        let content = match &artifact.value {
            ArtifactValue::EvidenceSet(entries) => {
                ensure!(
                    self.node(&artifact.node_instance_id)?.instance.kind
                        == BuiltinNode::CollectEvidence
                        && artifact.port == "evidence"
                        && self
                            .receipt(&artifact.node_instance_id, artifact.unit_id.as_deref())
                            .is_some_and(|receipt| receipt.state == ControlStageState::Completed),
                    "Review subjects need native completed evidence collections"
                );
                let mut records = completion_inputs(self.db, self.run, &artifact.id)?;
                ensure!(
                    records.len() == 1,
                    "A review evidence collection has no exact input provenance"
                );
                let mut pins = records.remove(0);
                let wires = self
                    .definition
                    .wires
                    .iter()
                    .filter(|wire| {
                        wire.to.node == artifact.node_instance_id && wire.to.port == "evidence"
                    })
                    .collect::<Vec<_>>();
                ensure!(
                    wires.len() == entries.len(),
                    "A review evidence collection lost its pinned inputs"
                );
                let mut content = Vec::with_capacity(entries.len());
                for (index, (entry, wire)) in entries.iter().zip(wires).enumerate() {
                    ensure!(
                        entry.source == wire.from,
                        "A review evidence source differs from its wire"
                    );
                    let selected = pins.remove(&format!("evidence.{}", index + 1));
                    let scope = self.provenance(&entry.source.node, &entry.source.port)?;
                    let value = match &entry.outcome {
                        EvidenceOutcome::Completed { artifact_id, value } => {
                            ensure!(
                                selected.as_ref() == Some(artifact_id),
                                "A review input differs from its completion pin"
                            );
                            let expected: ArtifactValue = serde_json::from_value(value.clone())?;
                            let row = load_artifact(
                                self.db,
                                &self.run.project_id,
                                &self.run.id,
                                artifact_id,
                                expected.kind(),
                            )?
                            .context("A selected review artifact is unavailable")?;
                            ensure!(
                                row.value == expected,
                                "A review input differs from its protected value"
                            );
                            if row.node_instance_id != entry.source.node
                                || row.port != entry.source.port
                            {
                                let source = self.node(&entry.source.node)?;
                                let unit = self.unit.filter(|_| {
                                    crate::workflows::control::unit_nodes(self.definition)
                                        .contains(entry.source.node.as_str())
                                });
                                ensure!(
                                    source.instance.kind == BuiltinNode::RepeatOutput
                                        && self
                                            .receipt(&entry.source.node, unit)
                                            .and_then(|receipt| receipt
                                                .forwarded_outputs
                                                .get(&entry.source.port))
                                            .is_some_and(
                                                |output| &output.artifact_id == artifact_id
                                            ),
                                    "A review evidence input lacks its recorded producer"
                                );
                            }
                            self.value(&row, depth + 1)?
                        }
                        EvidenceOutcome::Skipped => {
                            let unit = self.unit.filter(|_| {
                                crate::workflows::control::unit_nodes(self.definition)
                                    .contains(entry.source.node.as_str())
                            });
                            ensure!(
                                selected.is_none()
                                    && self.receipt(&entry.source.node, unit).is_some_and(
                                        |receipt| receipt.state == ControlStageState::Skipped
                                    ),
                                "The skipped review evidence is not supported by its control receipt"
                            );
                            json!({"state": "skipped"})
                        }
                        EvidenceOutcome::Failed => {
                            anyhow::bail!("A failed review dependency needs a fresh review")
                        }
                    };
                    content.push(json!([scope, value]));
                }
                ensure!(pins.is_empty(), "Review evidence has unaccounted inputs");
                Value::Array(content)
            }
            ArtifactValue::JsonObject(_)
            | ArtifactValue::RenderedText(_)
            | ArtifactValue::Markdown(_)
            | ArtifactValue::GithubResearch(_) => serde_json::to_value(&artifact.value)?,
            _ => anyhow::bail!("This review artifact has no supported stable content"),
        };
        Ok(json!([producer, content]))
    }
}
