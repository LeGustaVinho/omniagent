use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::{MAX_GRAPH_BYTES, load_head};
use crate::workflows::{
    NodeDefinition, WorkflowDefinition, WorkflowGraph, builtin_registry, compile_resolved,
};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowNodePosition {
    x: f64,
    y: f64,
}

impl WorkflowNodePosition {
    pub(super) fn within_canvas(self) -> bool {
        self.x.abs() <= 1_000_000.0 && self.y.abs() <= 1_000_000.0
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowCanvasEdit {
    pub(super) layout: BTreeMap<String, WorkflowNodePosition>,
    #[serde(default)]
    pub(super) update_definitions: BTreeMap<String, u32>,
    #[serde(default)]
    pub(super) definition_copies: BTreeMap<String, WorkflowDefinitionCopy>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct WorkflowDefinitionCopy {
    source_id: String,
    revision: u32,
}

pub(super) fn copy_node_definitions(
    next: &mut BTreeMap<String, NodeDefinition>,
    previous: &BTreeMap<String, NodeDefinition>,
    copies: BTreeMap<String, WorkflowDefinitionCopy>,
) -> Result<()> {
    for (id, copy) in copies {
        ensure!(
            !previous.contains_key(&id),
            "Node {id} already has a saved definition"
        );
        let source = previous
            .get(&copy.source_id)
            .context("The copied node's saved definition is unavailable")?;
        let target = next.get_mut(&id).context("The new node is unavailable")?;
        ensure!(
            source.kind == target.kind && source.revision == copy.revision,
            "The copied definition for {id} changed. Duplicate it from the current draft."
        );
        target.clone_from(source);
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DraftDocument {
    pub(super) graph: WorkflowGraph,
    pub(super) layout: BTreeMap<String, WorkflowNodePosition>,
    pub(super) definitions: BTreeMap<String, NodeDefinition>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum ProtectedDraft {
    Document(DraftDocument),
    Legacy(WorkflowGraph),
}

impl DraftDocument {
    pub(super) fn from_graph(graph: WorkflowGraph) -> Self {
        let registry = builtin_registry();
        let definitions = graph
            .nodes
            .iter()
            .filter_map(|node| {
                let definition = registry
                    .iter()
                    .find(|definition| definition.kind == node.kind)?;
                Some((node.id.clone(), definition.clone()))
            })
            .collect();
        Self {
            graph,
            layout: BTreeMap::new(),
            definitions,
        }
    }

    pub(super) fn from_definition(definition: WorkflowDefinition) -> Self {
        let mut definitions = BTreeMap::new();
        let nodes = definition
            .nodes
            .into_iter()
            .map(|node| {
                definitions.insert(node.instance.id.clone(), node.definition);
                node.instance
            })
            .collect();
        Self {
            graph: WorkflowGraph {
                name: definition.name,
                wires: definition.wires,
                nodes,
            },
            layout: BTreeMap::new(),
            definitions,
        }
    }

    pub(super) fn update(
        workflow: &str,
        previous: DraftDocument,
        graph: WorkflowGraph,
        edit: Option<WorkflowCanvasEdit>,
    ) -> Result<Self> {
        ensure!(
            serde_json::to_vec(&graph)?.len() <= MAX_GRAPH_BYTES,
            "The draft exceeds {MAX_GRAPH_BYTES} UTF-8 bytes"
        );
        let mut next = Self::from_graph(graph);
        let (mut layout, updates, copies) = edit.map_or_else(
            || (previous.layout, BTreeMap::new(), BTreeMap::new()),
            |edit| (edit.layout, edit.update_definitions, edit.definition_copies),
        );
        for (id, revision) in &updates {
            let current = next
                .definitions
                .get(id)
                .context("The node definition is unavailable")?;
            let previous = previous
                .definitions
                .get(id)
                .context("The draft does not contain the node to update")?;
            ensure!(
                current.kind == previous.kind
                    && current.revision == *revision
                    && *revision > previous.revision,
                "The selected update for node {id} is unavailable. Review the current registry."
            );
        }
        for (id, current) in &mut next.definitions {
            if !updates.contains_key(id)
                && let Some(previous) = previous
                    .definitions
                    .get(id)
                    .filter(|definition| definition.kind == current.kind)
            {
                current.clone_from(previous);
            }
        }
        copy_node_definitions(&mut next.definitions, &previous.definitions, copies)?;
        layout.retain(|id, _| next.definitions.contains_key(id));
        ensure!(
            layout.values().all(|point| point.within_canvas()),
            "A node position is outside the canvas limits"
        );
        next.layout = layout;
        compile_resolved(workflow.into(), next.graph.clone(), |node| {
            next.definitions.get(&node.id)
        })?;
        Ok(next)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkflowDraft {
    id: String,
    name: Option<String>,
    revision: Option<String>,
}

impl Store {
    pub async fn project_workflow_graph_drafts(
        &self,
        project: String,
    ) -> Result<Vec<ProjectWorkflowDraft>> {
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT id, head_revision FROM workflow_authoring_drafts WHERE project_id = ?1 ORDER BY rowid DESC",
            )?;
            statement.query_map(params![project], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))?
                .map(|row| {
                    let (id, revision) = row?;
                    let name = load_head(db, &id, revision.as_deref())?.map(|head| head.graph.name);
                    Ok(ProjectWorkflowDraft { id, name, revision })
                }).collect()
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::{BuiltinNode, Endpoint, NodeConfiguration, NodeInstance, Wire};

    #[test]
    fn preserves_an_old_definition_until_a_compatible_update_is_explicitly_selected() -> Result<()>
    {
        let edge = |from: &str, to: &str| Wire {
            from: Endpoint {
                node: "start".into(),
                port: from.into(),
            },
            to: Endpoint {
                node: "context".into(),
                port: to.into(),
            },
        };
        let graph = WorkflowGraph {
            name: "Edit pinned workflow".into(),
            nodes: vec![
                NodeInstance {
                    id: "start".into(),
                    kind: BuiltinNode::Start,
                    configuration: NodeConfiguration::None,
                },
                NodeInstance {
                    id: "context".into(),
                    kind: BuiltinNode::End,
                    configuration: NodeConfiguration::End,
                },
            ],
            wires: vec![edge("legacy-start", "ready")],
        };
        let mut previous = DraftDocument::from_graph(graph.clone());
        let expected = previous.definitions["start"].revision;
        let start = previous.definitions.get_mut("start").unwrap();
        start.revision -= 1;
        start
            .outputs
            .iter_mut()
            .find(|port| port.name == "start")
            .unwrap()
            .name = "legacy-start".into();
        let encoded = serde_json::to_vec(&previous)?;
        let decode = || -> Result<DraftDocument> { Ok(serde_json::from_slice(&encoded)?) };
        let update = |revision| {
            Some(WorkflowCanvasEdit {
                layout: BTreeMap::new(),
                update_definitions: BTreeMap::from([("start".into(), revision)]),
                definition_copies: BTreeMap::new(),
            })
        };
        let saved = DraftDocument::update("existing", decode()?, graph.clone(), None)?;
        assert_eq!(saved.definitions["start"], previous.definitions["start"]);
        assert!(
            DraftDocument::update("existing", decode()?, graph.clone(), update(expected + 1))
                .is_err()
        );
        assert!(
            DraftDocument::update("existing", decode()?, graph.clone(), update(expected)).is_err()
        );
        let mut corrected = graph;
        corrected.wires[0] = edge("start", "ready");
        let upgraded = DraftDocument::update("existing", decode()?, corrected, update(expected))?;
        assert_eq!(upgraded.definitions["start"].revision, expected);
        assert_eq!(
            upgraded.definitions["context"],
            previous.definitions["context"]
        );
        Ok(())
    }

    #[test]
    fn loads_the_original_protected_graph_shape() -> Result<()> {
        let encoded = serde_json::json!({"name":"Older draft","nodes":[{"id":"start","kind":"start","configuration":{"kind":"none"}}],"wires":[]});
        let ProtectedDraft::Legacy(graph) = serde_json::from_value(encoded)? else {
            anyhow::bail!("expected a legacy graph")
        };
        assert_eq!(
            DraftDocument::from_graph(graph).definitions["start"].kind,
            BuiltinNode::Start
        );
        Ok(())
    }

    #[test]
    fn copies_only_the_exact_saved_pin_into_a_new_node() -> Result<()> {
        let current = builtin_registry()
            .into_iter()
            .find(|definition| definition.kind == BuiltinNode::Start)
            .context("Start definition")?;
        let mut older = current.clone();
        older.revision -= 1;
        older.outputs[0].name = "legacy-port".into();
        let previous = BTreeMap::from([("source".into(), older.clone())]);
        let mut next = BTreeMap::from([("copy".into(), current)]);
        let requested = |revision| {
            BTreeMap::from([(
                "copy".into(),
                WorkflowDefinitionCopy {
                    source_id: "source".into(),
                    revision,
                },
            )])
        };
        assert!(
            copy_node_definitions(&mut next, &previous, requested(older.revision + 1)).is_err()
        );
        copy_node_definitions(&mut next, &previous, requested(older.revision))?;
        assert_eq!(next["copy"], older);
        assert!(
            copy_node_definitions(&mut next, &BTreeMap::new(), requested(older.revision)).is_err()
        );
        Ok(())
    }
}
