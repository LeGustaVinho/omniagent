use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::workflows::{
    BuiltinNode, CommandConfiguration, ConfigurationKind, Endpoint, NodeConfiguration,
    NodeDefinition, WorkflowDefinition, WorkflowGraph, WorkflowRevision,
    command::validate_command_binding,
    compile_resolved_graph, digest,
    registry::supports_pinned_execution,
    role_templates::{
        SaveConversationTemplate, TemplateBase, load_revision, save_conversation_template_in_tx,
        validate_conversation_template,
    },
    runtime::load_definition,
    subflow::{self, BoundSubflow},
};

use super::*;

impl Store {
    pub async fn publish_reviewed_workflow_package(
        &self,
        project: String,
        draft: String,
        reviewed_revision: String,
    ) -> Result<WorkflowPackagePublishOutcome> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stored = load_draft(&tx, &project, &draft)?;
            request::reconcile(&tx, &project, &stored)?;
            let stored = load_draft(&tx, &project, &draft)?;
            if stored.revision != reviewed_revision {
                tx.commit()?;
                anyhow::bail!("The package changed. Confirm the saved revision again.");
            }
            if let Some(publication) = stored.publication {
                return Ok(WorkflowPackagePublishOutcome::Published { publication });
            }
            ensure!(
                !stored
                    .request
                    .as_ref()
                    .is_some_and(WorkflowPackageRequest::pending),
                "Wait for the current package authoring request"
            );
            let (_, reviewed) = read_document(&tx, &draft, &reviewed_revision)?;
            ensure!(
                !reviewed.package.members.is_empty(),
                "Add an artifact before publishing this package"
            );
            if let Some((member, current)) = stale_member(&tx, &project, &reviewed.package)? {
                return Ok(WorkflowPackagePublishOutcome::StaleBase { member, current });
            }
            let members = resolve_package(&tx, &project, reviewed)?;
            let (originating_request_id, originating_run_id) =
                originating_request(&tx, &draft, &reviewed_revision)?;
            let id = new_id();
            let confirmed_at = tx.query_row("SELECT unixepoch()", [], |row| row.get(0))?;
            let publication = WorkflowPackagePublication {
                id,
                draft_id: draft,
                reviewed_revision,
                originating_request_id,
                originating_run_id,
                decision_source: "desktop_user".into(),
                confirmed_at,
                members,
            };
            tx.execute(
                "INSERT INTO workflow_package_publications
                 (id, project_id, draft_id, reviewed_revision, originating_request_id,
                  originating_run_id, decision_source, confirmed_at, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    publication.id,
                    project,
                    publication.draft_id,
                    publication.reviewed_revision,
                    publication.originating_request_id,
                    publication.originating_run_id,
                    publication.decision_source,
                    publication.confirmed_at,
                    seal(&publication)?
                ],
            )?;
            tx.commit()?;
            Ok(WorkflowPackagePublishOutcome::Published { publication })
        })
        .await
    }

    pub async fn published_workflow_package_artifact(
        &self,
        project: String,
        version: PackageArtifactVersion,
    ) -> Result<Option<PublishedPackageArtifact>> {
        self.execute(move |db| load_published(db, &project, &version))
            .await
    }

    pub async fn workflow_package_catalog(
        &self,
        project: String,
    ) -> Result<Vec<WorkflowPackageCatalogEntry>> {
        self.execute(move |db| {
            let mut versions = BTreeMap::new();
            let scope = PackageDestination::Project.key(&project);
            let mut heads = db.prepare(
                "SELECT scope_key, kind, artifact_id, revision FROM workflow_package_artifact_heads
                 WHERE scope_key IN (?1, 'library')",
            )?;
            for row in heads.query_map([&scope], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, String>(3)?)))? {
                let (key, kind, id, revision) = row?;
                let destination = if key == "library" { PackageDestination::Library } else { PackageDestination::Project };
                let kind = serde_json::from_value(serde_json::Value::String(kind))?;
                versions.insert((destination, kind, id.clone()), PackageArtifactVersion { destination, kind, id, revision });
            }
            for (sql, kind) in [
                ("SELECT id, head_revision FROM conversation_templates WHERE project_id = ?1", PackageArtifactKind::Role),
                ("SELECT workflow_id, revision FROM workflow_published_heads WHERE project_id = ?1", PackageArtifactKind::Pipeline),
            ] {
                let mut registered = db.prepare(sql)?;
                for row in registered.query_map([&project], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
                    let (id, revision) = row?;
                    let destination = PackageDestination::Project;
                    versions.insert((destination, kind, id.clone()), PackageArtifactVersion { destination, kind, id, revision });
                }
            }
            let mut legacy = db.prepare(
                "SELECT w.workflow_id, w.revision FROM workflow_revisions w WHERE w.project_id = ?1
                 AND NOT EXISTS(SELECT 1 FROM workflow_published_heads h
                    WHERE h.project_id = w.project_id AND h.workflow_id = w.workflow_id)",
            )?;
            let legacy = legacy.query_map([&project], |row| Ok(PackageArtifactVersion {
                destination: PackageDestination::Project, kind: PackageArtifactKind::Pipeline,
                id: row.get(0)?, revision: row.get(1)?,
            }))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut catalog = versions.into_values().chain(legacy).map(|version| {
                let artifact = load_published(db, &project, &version)?.context("A published catalog revision is unavailable")?;
                Ok(WorkflowPackageCatalogEntry { version, name: artifact.name, blockers: artifact.blockers })
            }).collect::<Result<Vec<_>>>()?;
            catalog.sort_by(|left, right| left.name.cmp(&right.name).then(left.version.cmp(&right.version)));
            Ok(catalog)
        }).await
    }
}

fn stale_member(
    db: &Connection,
    project: &str,
    package: &WorkflowPackage,
) -> Result<Option<(String, PublishedPackageArtifact)>> {
    let mut edited = BTreeSet::new();
    for member in &package.members {
        let Some(base) = &member.base else { continue };
        ensure!(
            edited.insert((base.destination, base.kind, base.id.as_str())),
            "Only one member may revise the same published artifact"
        );
        load_published(db, project, base)?.with_context(|| {
            format!("The base for {} is unavailable in this scope", member.name)
        })?;
        let current = head_revision(db, project, base)?;
        if let Some(current) = current.filter(|current| *current != base.revision) {
            let version = PackageArtifactVersion {
                revision: current,
                ..base.clone()
            };
            let current = load_published(db, project, &version)?
                .context("The current artifact revision is unavailable")?;
            return Ok(Some((member.key.clone(), current)));
        }
    }
    Ok(None)
}

pub(super) fn head_revision(
    db: &Connection,
    project: &str,
    version: &PackageArtifactVersion,
) -> Result<Option<String>> {
    let registered = match (version.destination, version.kind) {
        (PackageDestination::Project, PackageArtifactKind::Pipeline) => db.query_row(
            "SELECT revision FROM workflow_published_heads WHERE project_id = ?1 AND workflow_id = ?2",
            params![project, version.id], |row| row.get(0),
        ).optional()?,
        (PackageDestination::Project, PackageArtifactKind::Role) => db.query_row(
            "SELECT head_revision FROM conversation_templates WHERE project_id = ?1 AND id = ?2",
            params![project, version.id], |row| row.get(0),
        ).optional()?,
        _ => None,
    };
    if registered.is_some() {
        return Ok(registered);
    }
    Ok(db
        .query_row(
            "SELECT revision FROM workflow_package_artifact_heads
         WHERE scope_key = ?1 AND kind = ?2 AND artifact_id = ?3",
            params![
                version.destination.key(project),
                version.kind.sql(),
                version.id
            ],
            |row| row.get(0),
        )
        .optional()?)
}

fn resolve_package(
    db: &Connection,
    project: &str,
    document: PackageDocument,
) -> Result<Vec<PublishedPackageMember>> {
    let keys = document
        .package
        .members
        .iter()
        .map(|member| member.key.clone())
        .collect::<Vec<_>>();
    let pending = keys.iter().collect::<BTreeSet<_>>();
    for member in &document.package.members {
        for reference in member.references.values() {
            if let PackageReference::Draft { key } = reference {
                ensure!(
                    pending.contains(key),
                    "The package reference {key} does not identify an artifact"
                );
            }
        }
    }
    let mut unresolved = document.package.members;
    let mut resolved = BTreeMap::new();
    while !unresolved.is_empty() {
        let ready = unresolved
            .iter()
            .position(|member| {
                member.references.values().all(|reference| {
            !matches!(reference, PackageReference::Draft { key } if !resolved.contains_key(key))
        })
            })
            .context("Package references contain a cycle")?;
        let member = unresolved.remove(ready);
        let pins = document
            .definitions
            .get(&member.key)
            .context("The reviewed catalog definitions are missing")?;
        let result = publish_member(db, project, member, pins, &resolved)?;
        resolved.insert(result.key.clone(), result);
    }
    Ok(keys
        .into_iter()
        .filter_map(|key| resolved.remove(&key))
        .collect())
}

fn publish_member(
    db: &Connection,
    project: &str,
    member: PackageMember,
    pins: &BTreeMap<String, NodeDefinition>,
    resolved: &BTreeMap<String, PublishedPackageMember>,
) -> Result<PublishedPackageMember> {
    let mut references = BTreeMap::new();
    let mut dependencies = BTreeMap::new();
    let mut blockers = Vec::new();
    for (slot, reference) in member.references {
        let artifact = match reference {
            PackageReference::Draft { key } => resolved
                .get(&key)
                .context("The package dependency is unresolved")?
                .artifact
                .clone(),
            PackageReference::Published { version } => load_published(db, project, &version)?
                .with_context(|| {
                    format!("The published reference {slot} is unavailable in this scope")
                })?,
            PackageReference::Parameter { name, kind } => {
                blockers.push(format!("Choose the binding for {name}."));
                references.insert(slot, ResolvedPackageReference::Parameter { name, kind });
                continue;
            }
        };
        ensure!(
            member.destination == PackageDestination::Project
                || artifact.version.destination == PackageDestination::Library,
            "Library artifact {} requires a library reference or a named parameter for {slot}",
            member.name
        );
        check_transitive_references(db, project, &artifact, &mut BTreeSet::new(), false)?;
        blockers.extend(artifact.blockers.iter().cloned());
        references.insert(
            slot.clone(),
            ResolvedPackageReference::Published {
                version: artifact.version.clone(),
            },
        );
        dependencies.insert(slot, artifact);
    }
    let mut body = member.body;
    let kind = body.kind();
    let interface = match &body {
        PackageArtifactBody::Subflow {
            inputs, outputs, ..
        } => Some((inputs.clone(), outputs.clone())),
        _ => None,
    };
    let mut id = member
        .base
        .as_ref()
        .map_or_else(new_id, |base| base.id.clone());
    let mut workflow = None;
    let mut role_revision = None;
    match &mut body {
        PackageArtifactBody::Pipeline { graph } | PackageArtifactBody::Subflow { graph, .. } => {
            let named_commands = references
                .iter()
                .filter_map(|(slot, target)| {
                    matches!(
                        target,
                        ResolvedPackageReference::Parameter {
                            kind: PackageArtifactKind::Command,
                            ..
                        }
                    )
                    .then_some(slot.clone())
                })
                .collect::<BTreeSet<_>>();
            if member.destination == PackageDestination::Library {
                for node in &graph.nodes {
                    if node.kind == BuiltinNode::Command {
                        let has_named_dependency =
                            dependencies.get(&node.id).is_some_and(|artifact| {
                                matches!(
                                    artifact.body,
                                    PackageArtifactBody::Command {
                                        binding: PackageCommandBinding::Required { .. },
                                        ..
                                    }
                                )
                            });
                        ensure!(
                            named_commands.contains(&node.id) || has_named_dependency,
                            "Library Command node {} requires a named local binding",
                            node.id
                        );
                    }
                }
            }
            let compiled = if let Some(interface) = &interface {
                GraphCompiler::new(db, project, &|version| load_published(db, project, version))
                    .compile(
                        &id,
                        graph,
                        pins,
                        &dependencies,
                        named_commands,
                        &mut blockers,
                        Some(interface),
                    )?
                    .definition
            } else {
                compile_graph(
                    db,
                    project,
                    &id,
                    graph,
                    pins,
                    &dependencies,
                    named_commands,
                    &mut blockers,
                )?
            };
            if kind == PackageArtifactKind::Pipeline {
                workflow = Some(compiled);
            }
        }
        PackageArtifactBody::Role { template } => {
            validate_conversation_template(template)?;
            if member.destination == PackageDestination::Project {
                let base = member.base.as_ref().map(|base| TemplateBase {
                    template_id: base.id.clone(),
                    expected_revision: base.revision.clone(),
                });
                let SaveConversationTemplate::Saved { detail } =
                    save_conversation_template_in_tx(db, project, base, template.clone(), true)?
                else {
                    anyhow::bail!("The role base changed during publication")
                };
                id = detail.revision.template_id;
                role_revision = Some(detail.revision.version.revision);
            }
        }
        PackageArtifactBody::Preset {
            node_kind,
            configuration,
            editable_fields,
        } => {
            let definition = pins
                .get("$preset")
                .filter(|pin| pin.kind == *node_kind)
                .context("The preset's reviewed node definition is missing")?;
            ensure!(
                supports_pinned_execution(definition)
                    && configuration.matches(definition.configuration),
                "The preset needs a valid, supported node configuration"
            );
            let fields = serde_json::to_value(&configuration)?;
            let keys = editable_fields.iter().collect::<BTreeSet<_>>();
            ensure!(
                keys.len() == editable_fields.len()
                    && editable_fields
                        .iter()
                        .all(|key| key != "kind" && fields.get(key).is_some()),
                "The preset exposes an unknown or repeated editable field"
            );
            if let NodeConfiguration::Command(command) = configuration {
                ensure!(
                    member.destination == PackageDestination::Project,
                    "A library command preset requires a named Command binding"
                );
                validate_command_binding(db, project, command)?;
            }
        }
        PackageArtifactBody::Command { binding, .. } => {
            if let PackageCommandBinding::Required { name } = binding {
                bounded_name(name)?;
                blockers.push(format!(
                    "Register and bind the Command executable or script {name}."
                ));
            } else {
                ensure!(
                    member.destination == PackageDestination::Project,
                    "A library Command uses a named local binding"
                );
            }
        }
        PackageArtifactBody::McpTool {
            binding,
            tool,
            input_schema,
            ..
        } => {
            bounded_name(binding)?;
            bounded_name(tool)?;
            ensure!(
                input_schema.get("type").and_then(serde_json::Value::as_str) == Some("object"),
                "An MCP tool needs an object input schema"
            );
            blockers.push(format!("Register and bind the local MCP server {binding}."));
        }
    }
    if let PackageArtifactBody::Command {
        binding: PackageCommandBinding::Registered { .. },
        ..
    } = &body
    {
        validate_command_binding(
            db,
            project,
            &configured_command(&body).context("Invalid Command configuration")?,
        )?;
    }
    let revision = match role_revision {
        Some(revision) => revision,
        None => digest(&serde_json::to_vec(&(
            &member.name,
            &body,
            &references,
            pins,
        ))?),
    };
    let version = PackageArtifactVersion {
        destination: member.destination,
        kind,
        id,
        revision,
    };
    if let Some(mut workflow) = workflow {
        workflow.version.revision.clone_from(&version.revision);
        db.execute(
            "INSERT INTO workflow_revisions VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, workflow_id, revision) DO NOTHING",
            params![project, version.id, version.revision, seal(&workflow)?],
        )?;
        db.execute(
            "INSERT INTO workflow_published_heads VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id, workflow_id) DO UPDATE SET revision = excluded.revision",
            params![project, version.id, version.revision],
        )?;
    }
    blockers.sort();
    blockers.dedup();
    let artifact = PublishedPackageArtifact {
        version,
        name: member.name,
        body,
        references,
        definitions: pins.clone(),
        blockers,
    };
    save_artifact(db, project, &artifact)?;
    Ok(PublishedPackageMember {
        key: member.key,
        artifact,
        previous: member.base,
    })
}

fn configured_command(body: &PackageArtifactBody) -> Option<CommandConfiguration> {
    let PackageArtifactBody::Command {
        binding: PackageCommandBinding::Registered { id, revision },
        arguments,
        input,
        output,
    } = body
    else {
        return None;
    };
    Some(CommandConfiguration {
        registration_id: id.clone(),
        registration_revision: revision.clone(),
        arguments: arguments.clone(),
        input: input.clone(),
        output: output.clone(),
    })
}

pub(super) fn compile_graph(
    db: &Connection,
    project: &str,
    id: &str,
    graph: &WorkflowGraph,
    pins: &BTreeMap<String, NodeDefinition>,
    dependencies: &BTreeMap<String, PublishedPackageArtifact>,
    unbound_commands: BTreeSet<String>,
    blockers: &mut Vec<String>,
) -> Result<WorkflowDefinition> {
    compile_graph_with_loader(
        db,
        project,
        id,
        graph,
        pins,
        dependencies,
        unbound_commands,
        blockers,
        &|version| load_published(db, project, version),
    )
}

type ArtifactLoader<'a> =
    dyn Fn(&PackageArtifactVersion) -> Result<Option<PublishedPackageArtifact>> + 'a;
pub(super) type Interface = (BTreeMap<String, Endpoint>, BTreeMap<String, Endpoint>);

pub(super) fn compile_graph_with_loader(
    db: &Connection,
    project: &str,
    id: &str,
    graph: &WorkflowGraph,
    pins: &BTreeMap<String, NodeDefinition>,
    dependencies: &BTreeMap<String, PublishedPackageArtifact>,
    unbound_commands: BTreeSet<String>,
    blockers: &mut Vec<String>,
    loader: &ArtifactLoader<'_>,
) -> Result<WorkflowDefinition> {
    Ok(GraphCompiler::new(db, project, loader)
        .compile(
            id,
            graph,
            pins,
            dependencies,
            unbound_commands,
            blockers,
            None,
        )?
        .definition)
}

pub(super) fn compile_simulation_graph(
    db: &Connection,
    project: &str,
    id: &str,
    graph: &WorkflowGraph,
    pins: &BTreeMap<String, NodeDefinition>,
    dependencies: &BTreeMap<String, PublishedPackageArtifact>,
    blockers: &mut Vec<String>,
    interface: Option<&Interface>,
    loader: &ArtifactLoader<'_>,
) -> Result<(WorkflowDefinition, BTreeMap<String, NodeDefinition>)> {
    let mut compiler = GraphCompiler::new(db, project, loader);
    compiler.validate_bindings = false;
    let compiled = compiler.compile(
        id,
        graph,
        pins,
        dependencies,
        BTreeSet::new(),
        blockers,
        interface,
    )?;
    Ok((compiled.definition, compiled.authoring_definitions))
}

struct CompiledGraph {
    definition: WorkflowDefinition,
    outputs: BTreeMap<String, Endpoint>,
    authoring_definitions: BTreeMap<String, NodeDefinition>,
}

struct GraphCompiler<'a> {
    db: &'a Connection,
    project: &'a str,
    loader: &'a ArtifactLoader<'a>,
    stack: BTreeSet<PackageArtifactVersion>,
    validate_bindings: bool,
}

impl<'a> GraphCompiler<'a> {
    fn new(db: &'a Connection, project: &'a str, loader: &'a ArtifactLoader<'a>) -> Self {
        Self {
            db,
            project,
            loader,
            stack: BTreeSet::new(),
            validate_bindings: true,
        }
    }

    fn compile(
        &mut self,
        id: &str,
        graph: &WorkflowGraph,
        pins: &BTreeMap<String, NodeDefinition>,
        dependencies: &BTreeMap<String, PublishedPackageArtifact>,
        mut unbound_commands: BTreeSet<String>,
        blockers: &mut Vec<String>,
        interface: Option<&Interface>,
    ) -> Result<CompiledGraph> {
        let mut graph = graph.clone();
        let mut effective_pins = pins.clone();
        let mut calls = BTreeMap::new();
        for (slot, artifact) in dependencies {
            let node = graph
                .nodes
                .iter_mut()
                .find(|node| node.id == *slot)
                .with_context(|| format!("The reference {slot} does not identify a graph node"))?;
            match &artifact.body {
                PackageArtifactBody::Preset {
                    node_kind,
                    configuration,
                    ..
                } => {
                    ensure!(
                        node.kind == *node_kind,
                        "The preset for {slot} configures a different node type"
                    );
                    node.configuration.clone_from(configuration);
                    effective_pins.insert(
                        slot.clone(),
                        artifact
                            .definitions
                            .get("$preset")
                            .context("The pinned preset definition is unavailable")?
                            .clone(),
                    );
                }
                PackageArtifactBody::Command { .. } => {
                    ensure!(
                        node.kind == BuiltinNode::Command,
                        "The Command reference for {slot} requires a Command node"
                    );
                    if let Some(configuration) = configured_command(&artifact.body) {
                        node.configuration = NodeConfiguration::Command(configuration);
                    } else {
                        unbound_commands.insert(slot.clone());
                    }
                }
                PackageArtifactBody::Role { .. } => {
                    ensure!(
                        matches!(node.configuration, NodeConfiguration::AgentRole { .. }),
                        "The role reference for {slot} requires an Agent node"
                    );
                    blockers.push(format!(
                        "The node {slot} requires the pinned role {}.",
                        artifact.name
                    ));
                }
                PackageArtifactBody::Subflow { .. } if node.kind == BuiltinNode::Subflow => {
                    ensure!(
                        matches!(node.configuration, NodeConfiguration::None),
                        "The subflow invocation {slot} does not accept native node configuration"
                    );
                    calls.insert(slot.clone(), self.child(artifact, blockers)?);
                }
                PackageArtifactBody::Subflow { .. } if node.kind == BuiltinNode::RepeatUntil => {
                    let NodeConfiguration::RepeatUntil(configuration) = &node.configuration else {
                        anyhow::bail!("Repeat node {slot} needs a stopping condition and bound");
                    };
                    let body = self.child(artifact, blockers)?;
                    calls.insert(
                        slot.clone(),
                        crate::workflows::repetition::expand_repeat(body, configuration)?,
                    );
                }
                PackageArtifactBody::Subflow { .. } | PackageArtifactBody::McpTool { .. } => {
                    blockers.push(format!(
                        "The node {slot} requires the reusable artifact {}.",
                        artifact.name
                    ))
                }
                PackageArtifactBody::Pipeline { .. } => {
                    anyhow::bail!("A pipeline reference requires a published subflow")
                }
            }
        }
        for node in &graph.nodes {
            let pin = effective_pins
                .get_mut(&node.id)
                .context("A workflow node lost its reviewed definition")?;
            if matches!(node.configuration, NodeConfiguration::SubflowEntry { .. }) {
                ensure!(
                    interface.is_some(),
                    "A root pipeline uses the native Start contract"
                );
                *pin = subflow::entry_definition(node, pin)?;
            }
            ensure!(
                supports_pinned_execution(pin)
                    || node.kind == BuiltinNode::Start
                        && pin.configuration == ConfigurationKind::SubflowEntry,
                "The pinned definition for {} is unavailable",
                node.id
            );
            if matches!(node.kind, BuiltinNode::Subflow | BuiltinNode::RepeatUntil)
                && !calls.contains_key(&node.id)
            {
                blockers.push(format!("Select a pinned subflow for {}.", node.id));
            }
            if self.validate_bindings
                && !unbound_commands.contains(&node.id)
                && let NodeConfiguration::Command(command) = &node.configuration
            {
                validate_command_binding(self.db, self.project, command).with_context(|| {
                    format!("The Command registration for {} is unavailable", node.id)
                })?;
            }
            if self.validate_bindings
                && let NodeConfiguration::McpTool(tool) = &node.configuration
            {
                crate::workflows::mcp_tools::validate_mcp_binding(self.db, self.project, tool)
                    .with_context(|| {
                        format!("The MCP tool binding for {} is unavailable", node.id)
                    })?;
            }
        }
        let mut authoring_definitions = BTreeMap::new();
        if !self.validate_bindings {
            authoring_definitions.clone_from(&effective_pins);
            for (slot, call) in &calls {
                let definition = authoring_definitions
                    .get_mut(slot)
                    .context("A subflow lost its authored node")?;
                definition.inputs = call
                    .inputs
                    .iter()
                    .map(|(name, endpoint)| {
                        Ok(crate::workflows::Port {
                            name: name.clone(),
                            value: subflow::output(&call.workflow, endpoint)?,
                        })
                    })
                    .collect::<Result<_>>()?;
                definition.outputs = call
                    .outputs
                    .iter()
                    .map(|(name, endpoint)| {
                        Ok(crate::workflows::Port {
                            name: name.clone(),
                            value: subflow::output(&call.workflow, endpoint)?,
                        })
                    })
                    .collect::<Result<_>>()?;
            }
        }
        let empty = BTreeMap::new();
        let expanded = subflow::expand(
            graph,
            effective_pins,
            calls,
            interface.map_or(&empty, |(_, outputs)| outputs),
        )?;
        let mut definition = compile_resolved_graph(
            id.to_owned(),
            expanded.graph,
            |node| expanded.definitions.get(&node.id),
            interface.is_none(),
        )?;
        if let Some((inputs, outputs)) = interface {
            for name in inputs.keys().chain(outputs.keys()) {
                bounded_name(name)?;
            }
            if !inputs.is_empty() || !outputs.is_empty() {
                subflow::validate_interface(&definition, inputs, &expanded.outputs)?;
            }
        }
        definition.subflows = expanded.subflows;
        Ok(CompiledGraph {
            definition,
            outputs: expanded.outputs,
            authoring_definitions,
        })
    }

    fn child(
        &mut self,
        artifact: &PublishedPackageArtifact,
        blockers: &mut Vec<String>,
    ) -> Result<BoundSubflow> {
        let PackageArtifactBody::Subflow {
            graph,
            inputs,
            outputs,
        } = &artifact.body
        else {
            anyhow::bail!("Expected a pinned subflow");
        };
        ensure!(
            self.stack.len() < 32 && self.stack.insert(artifact.version.clone()),
            "Nested subflow references contain a cycle or exceed the depth limit"
        );
        let mut dependencies = BTreeMap::new();
        let mut commands = BTreeSet::new();
        for (slot, reference) in &artifact.references {
            match reference {
                ResolvedPackageReference::Published { version } => {
                    let target = (self.loader)(version)?
                        .context("A pinned subflow dependency is unavailable")?;
                    ensure!(
                        target.version == *version,
                        "A subflow dependency resolved to a different revision"
                    );
                    dependencies.insert(slot.clone(), target);
                }
                ResolvedPackageReference::Parameter { name, kind } => {
                    blockers.push(format!("Choose the binding for {name}."));
                    if *kind == PackageArtifactKind::Command {
                        commands.insert(slot.clone());
                    }
                }
            }
        }
        let interface = (inputs.clone(), outputs.clone());
        let compiled = self.compile(
            &artifact.version.id,
            graph,
            &artifact.definitions,
            &dependencies,
            commands,
            blockers,
            Some(&interface),
        )?;
        subflow::validate_interface(&compiled.definition, inputs, &compiled.outputs)?;
        self.stack.remove(&artifact.version);
        Ok(BoundSubflow {
            name: artifact.name.clone(),
            version: artifact.version.clone(),
            workflow: compiled.definition,
            inputs: inputs.clone(),
            outputs: compiled.outputs,
        })
    }
}

fn save_artifact(
    db: &Connection,
    project: &str,
    artifact: &PublishedPackageArtifact,
) -> Result<()> {
    let version = &artifact.version;
    let scope = version.destination.key(project);
    let changed = db.execute(
        "INSERT INTO workflow_package_artifact_revisions VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT DO NOTHING",
        params![
            scope,
            version.kind.sql(),
            version.id,
            version.revision,
            seal(artifact)?
        ],
    )?;
    if changed == 0 {
        ensure!(
            load_published(db, project, version)?.as_ref() == Some(artifact),
            "The saved package artifact differs from its immutable revision"
        );
    }
    db.execute(
        "INSERT INTO workflow_package_artifact_heads VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(scope_key, kind, artifact_id) DO UPDATE SET revision = excluded.revision",
        params![scope, version.kind.sql(), version.id, version.revision],
    )?;
    Ok(())
}

fn stored_artifact(
    db: &Connection,
    project: &str,
    version: &PackageArtifactVersion,
) -> Result<Option<PublishedPackageArtifact>> {
    let bytes: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM workflow_package_artifact_revisions
         WHERE scope_key = ?1 AND kind = ?2 AND artifact_id = ?3 AND revision = ?4",
            params![
                version.destination.key(project),
                version.kind.sql(),
                version.id,
                version.revision
            ],
            |row| row.get(0),
        )
        .optional()?;
    let Some(bytes) = bytes else { return Ok(None) };
    let artifact: PublishedPackageArtifact = open(&bytes)?;
    ensure!(
        &artifact.version == version,
        "A protected package artifact no longer matches its revision"
    );
    Ok(Some(artifact))
}

pub(super) fn load_published(
    db: &Connection,
    project: &str,
    version: &PackageArtifactVersion,
) -> Result<Option<PublishedPackageArtifact>> {
    if let Some(artifact) = stored_artifact(db, project, version)? {
        return Ok(Some(artifact));
    }
    if version.destination != PackageDestination::Project {
        return Ok(None);
    }
    let mut definitions = BTreeMap::new();
    let (name, body) = match version.kind {
        PackageArtifactKind::Pipeline => {
            let Some(definition) = load_definition(
                db,
                project,
                &WorkflowRevision {
                    id: version.id.clone(),
                    revision: version.revision.clone(),
                },
            )?
            else {
                return Ok(None);
            };
            (
                definition.name.clone(),
                PackageArtifactBody::Pipeline {
                    graph: WorkflowGraph {
                        name: definition.name,
                        nodes: definition
                            .nodes
                            .into_iter()
                            .map(|node| {
                                definitions.insert(node.instance.id.clone(), node.definition);
                                node.instance
                            })
                            .collect(),
                        wires: definition.wires,
                    },
                },
            )
        }
        PackageArtifactKind::Role => {
            let exists = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversation_template_revisions WHERE project_id = ?1 AND template_id = ?2 AND revision = ?3)",
                params![project, version.id, version.revision], |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Ok(None);
            }
            let role = load_revision(db, project, &version.id, &version.revision)?;
            (
                role.template.name.clone(),
                PackageArtifactBody::Role {
                    template: role.template,
                },
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(PublishedPackageArtifact {
        version: version.clone(),
        name,
        body,
        references: BTreeMap::new(),
        definitions,
        blockers: Vec::new(),
    }))
}

pub(crate) fn ensure_package_runnable(
    db: &Connection,
    project: &str,
    workflow: &WorkflowRevision,
) -> Result<()> {
    if let Some(artifact) = stored_artifact(db, project, &pipeline_reference(workflow))? {
        check_transitive_references(db, project, &artifact, &mut BTreeSet::new(), true)?;
    }
    Ok(())
}

fn check_transitive_references(
    db: &Connection,
    project: &str,
    artifact: &PublishedPackageArtifact,
    seen: &mut BTreeSet<PackageArtifactVersion>,
    runnable: bool,
) -> Result<()> {
    if !seen.insert(artifact.version.clone()) {
        return Ok(());
    }
    if runnable {
        ensure!(
            artifact.blockers.is_empty(),
            "{}",
            artifact.blockers.join(" ")
        );
        if let Some(command) = configured_command(&artifact.body) {
            validate_command_binding(db, project, &command)?;
        }
    }
    for reference in artifact.references.values() {
        match reference {
            ResolvedPackageReference::Published { version } => {
                ensure!(
                    artifact.version.destination == PackageDestination::Project
                        || version.destination == PackageDestination::Library,
                    "A library dependency is outside the library"
                );
                let dependency = load_published(db, project, version)?
                    .context("A required published dependency is unavailable")?;
                check_transitive_references(db, project, &dependency, seen, runnable)?;
            }
            ResolvedPackageReference::Parameter { name, .. } if runnable => {
                anyhow::bail!("Choose the binding for {name}.")
            }
            _ => (),
        }
    }
    Ok(())
}

fn originating_request(
    db: &Connection,
    draft: &str,
    revision: &str,
) -> Result<(Option<String>, Option<String>)> {
    Ok(db.query_row(
        "WITH RECURSIVE history(id, parent, run, distance) AS (
             SELECT id, parent_revision, run_id, 0 FROM workflow_package_revisions WHERE draft_id = ?1 AND id = ?2
             UNION ALL
             SELECT v.id, v.parent_revision, v.run_id, history.distance + 1
             FROM workflow_package_revisions v JOIN history ON v.id = history.parent
             WHERE v.draft_id = ?1 AND history.run IS NULL
         )
         SELECT request.id, history.run FROM history
         JOIN job_attempts attempt ON attempt.run_id = history.run
         JOIN workflow_package_requests request ON request.job_id = attempt.job_id AND request.draft_id = ?1
         ORDER BY distance LIMIT 1",
        params![draft, revision], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?.unwrap_or_default())
}
