use serde_json::{Value, json};

use crate::workflows::{
    BuiltinNode, NodeConfiguration, WorkflowDefinition, command::validate_command_binding,
    mcp_tools::validate_mcp_binding,
};

use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPackagePreview {
    revision: String,
    revisions: Vec<String>,
    members: Vec<MemberPreview>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberPreview {
    key: String,
    name: String,
    kind: PackageArtifactKind,
    base: Option<PackageArtifactVersion>,
    changes: Vec<FieldChange>,
    changes_truncated: bool,
    dependencies: BTreeMap<String, PackageReference>,
    affected_uses: Vec<PublishedUse>,
    diagnostics: Vec<String>,
    requirements: Vec<String>,
    graph_definitions: BTreeMap<String, NodeDefinition>,
}

#[derive(Serialize)]
struct FieldChange {
    path: String,
    change: &'static str,
    before: Option<Value>,
    after: Option<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishedUse {
    consumer: PackageArtifactVersion,
    name: String,
    slot: String,
    pinned_revision: String,
}

fn review_document(
    db: &Connection,
    project: &str,
    draft: &str,
    revision: &str,
) -> Result<PackageDocument> {
    ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM workflow_package_drafts WHERE id = ?1 AND project_id = ?2)",
        params![draft, project], |row| row.get::<_, bool>(0))?, "The package draft is unavailable in this project");
    Ok(read_document(db, draft, revision)
        .context("The selected package revision is unavailable")?
        .1)
}

impl Store {
    pub async fn preview_workflow_package_revision(
        &self,
        project: String,
        draft: String,
        revision: String,
    ) -> Result<WorkflowPackagePreview> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let document = review_document(&tx, &project, &draft, &revision)?;
            let revisions = tx.prepare("WITH RECURSIVE history(id, depth) AS (
                SELECT head_revision, 0 FROM workflow_package_drafts WHERE id = ?1
                UNION ALL SELECT r.parent_revision, h.depth + 1 FROM history h JOIN workflow_package_revisions r
                 ON r.draft_id = ?1 AND r.id = h.id WHERE r.parent_revision IS NOT NULL
                ) SELECT id FROM history ORDER BY depth")?.query_map([&draft], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            let published = if document.package.members.iter().any(|member| member.base.is_some()) {
                published_consumers(&tx, &project)?
            } else { Vec::new() };
            let catalog = ReviewCatalog::new(&tx, &project, &document, &revision);
            let members = document.package.members.iter().map(|member| preview_member(&catalog, member, &published)).collect::<Result<_>>()?;
            Ok(WorkflowPackagePreview { revision, revisions, members })
        }).await
    }
}

pub(super) struct ReviewCatalog<'a> {
    db: &'a Connection,
    project: &'a str,
    document: &'a PackageDocument,
    revision: &'a str,
}

impl<'a> ReviewCatalog<'a> {
    pub(super) fn new(
        db: &'a Connection,
        project: &'a str,
        document: &'a PackageDocument,
        revision: &'a str,
    ) -> Self {
        Self {
            db,
            project,
            document,
            revision,
        }
    }

    fn draft_version(&self, member: &PackageMember) -> PackageArtifactVersion {
        PackageArtifactVersion {
            destination: member.destination,
            kind: member.body.kind(),
            id: format!("draft:{}", member.key),
            revision: self.revision.into(),
        }
    }

    pub(super) fn draft_member(&self, key: &str) -> Result<&PackageMember> {
        self.document
            .package
            .members
            .iter()
            .find(|member| member.key == key)
            .with_context(|| format!("Missing draft artifact {key}"))
    }

    fn load(&self, version: &PackageArtifactVersion) -> Result<Option<PublishedPackageArtifact>> {
        if version.revision == self.revision
            && let Some(key) = version.id.strip_prefix("draft:")
        {
            let member = self.draft_member(key)?;
            ensure!(
                self.draft_version(member) == *version,
                "The draft dependency type or destination changed"
            );
            let references = member
                .references
                .iter()
                .map(|(slot, reference)| {
                    let target = match reference {
                        PackageReference::Draft { key } => ResolvedPackageReference::Published {
                            version: self.draft_version(self.draft_member(key)?),
                        },
                        PackageReference::Published { version } => {
                            ResolvedPackageReference::Published {
                                version: version.clone(),
                            }
                        }
                        PackageReference::Parameter { name, kind } => {
                            ResolvedPackageReference::Parameter {
                                name: name.clone(),
                                kind: *kind,
                            }
                        }
                    };
                    Ok((slot.clone(), target))
                })
                .collect::<Result<_>>()?;
            return Ok(Some(PublishedPackageArtifact {
                version: version.clone(),
                name: member.name.clone(),
                body: member.body.clone(),
                references,
                definitions: self
                    .document
                    .definitions
                    .get(key)
                    .context("The reviewed node definitions are unavailable")?
                    .clone(),
                layout: self.document.layouts.get(key).cloned().unwrap_or_default(),
                blockers: Vec::new(),
                prompt_expansion: None,
            }));
        }
        publish::load_published(self.db, self.project, version)
    }

    pub(super) fn compile(
        &self,
        member: &PackageMember,
        requirements: &mut Vec<String>,
    ) -> Result<(WorkflowDefinition, BTreeMap<String, NodeDefinition>)> {
        let artifact = self
            .load(&self.draft_version(member))?
            .context("The draft graph is unavailable")?;
        let mut dependencies = BTreeMap::new();
        for (slot, reference) in &artifact.references {
            match reference {
                ResolvedPackageReference::Published { version } => {
                    let dependency = self
                        .load(version)?
                        .with_context(|| format!("The pinned reference {slot} is unavailable"))?;
                    ensure!(
                        member.destination == PackageDestination::Project
                            || dependency.version.destination == PackageDestination::Library,
                        "Library artifact {} requires a library reference or named parameter for {slot}",
                        member.name
                    );
                    requirements.extend(publish::dependency_blockers(
                        &member.body,
                        slot,
                        &dependency,
                    ));
                    dependencies.insert(slot.clone(), dependency);
                }
                ResolvedPackageReference::Parameter { name, .. } => {
                    requirements.push(format!("Bind {name} for execution."))
                }
            }
        }
        if let PackageArtifactBody::Prompt { prompt } = &member.body {
            let expanded = publish::compile_prompt_graph(
                self.db,
                self.project,
                &artifact.version.id,
                &member.name,
                prompt,
                &dependencies,
                requirements,
                &|version| self.load(version),
                false,
            )?;
            let definitions = expanded
                .workflow
                .nodes
                .iter()
                .map(|node| (node.instance.id.clone(), node.definition.clone()))
                .collect();
            return Ok((expanded.workflow, definitions));
        }
        let (graph, interface) = match &member.body {
            PackageArtifactBody::Pipeline { graph } => (graph, None),
            PackageArtifactBody::Subflow {
                graph,
                inputs,
                outputs,
            } => (graph, Some((inputs.clone(), outputs.clone()))),
            _ => anyhow::bail!("Choose a pipeline or subflow to review its graph"),
        };
        if member.destination == PackageDestination::Library {
            for node in &graph.nodes {
                if let NodeConfiguration::McpTool(tool) = &node.configuration {
                    publish::require_portable_mcp(
                        tool,
                        &node.id,
                        &artifact.references,
                        &dependencies,
                    )?;
                }
            }
        }
        let (definition, authoring_definitions) = publish::compile_review_graph(
            self.db,
            self.project,
            &artifact.version.id,
            graph,
            &artifact.definitions,
            &dependencies,
            requirements,
            interface.as_ref(),
            &|version| self.load(version),
        )?;
        for node in &definition.nodes {
            let named_checks = match &node.instance.configuration {
                NodeConfiguration::VerifyTestPhase(test) => std::slice::from_ref(&test.check),
                NodeConfiguration::IntegratedChecks(checks) => checks.checks.as_slice(),
                NodeConfiguration::StagedChecks { names, .. } => names.as_slice(),
                NodeConfiguration::CandidateAcceptance(acceptance) => {
                    std::slice::from_ref(&acceptance.build_check)
                }
                _ => &[],
            };
            for name in named_checks {
                if !crate::repository::verification::test_phase::load_test_gate(
                    self.db,
                    self.project,
                    name,
                )?
                .has_registered_command()
                {
                    requirements.push(format!("Register the project check {name}."));
                }
            }
            match node.instance.kind {
                BuiltinNode::SearchPublicRepositories => {
                    requirements.push(
                        "Authenticate the GitHub CLI (gh) for public repository research.".into(),
                    );
                }
                BuiltinNode::OpenOrUpdatePullRequest
                | BuiltinNode::WaitForCi
                | BuiltinNode::MergePullRequest => {
                    let bound: bool = self.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM workflow_github_project_settings WHERE project_id = ?1)",
                        [self.project], |row| row.get(0),
                    )?;
                    if !bound {
                        requirements.push("Set the GitHub account, repository, destination and required checks in project settings.".into());
                    }
                }
                BuiltinNode::RunBacklog => {
                    if crate::tasks::load_delivery_milestone_policy(self.db, self.project)?
                        .destination_branch
                        .is_none()
                    {
                        requirements.push(
                            "Set the delivery destination branch in the project's Agile policy."
                                .into(),
                        );
                    }
                }
                _ => {}
            }
            let binding = match &node.instance.configuration {
                NodeConfiguration::Command(command) => {
                    validate_command_binding(self.db, self.project, command)
                }
                NodeConfiguration::McpTool(tool) if !tool.is_package_placeholder() => {
                    validate_mcp_binding(self.db, self.project, tool)
                }
                _ => continue,
            };
            if let Err(error) = binding {
                requirements.push(format!(
                    "{} {} needs a local binding. {error}",
                    node.definition.title, node.instance.id
                ));
            }
        }
        Ok((definition, authoring_definitions))
    }
}

fn preview_member(
    catalog: &ReviewCatalog<'_>,
    member: &PackageMember,
    published: &[PublishedPackageArtifact],
) -> Result<MemberPreview> {
    let mut diagnostics = Vec::new();
    let mut requirements = Vec::new();
    let base = if let Some(version) = &member.base {
        let artifact = publish::load_published(catalog.db, catalog.project, version)?;
        if artifact.is_none() {
            diagnostics.push("The exact published base is unavailable.".into());
        } else if let Some(head) = publish::head_revision(catalog.db, catalog.project, version)?
            .filter(|head| head != &version.revision)
        {
            diagnostics.push(format!("The published base moved to {head}."));
        }
        artifact
    } else {
        None
    };
    let mut diff = FieldDiff::default();
    let before = base
        .as_ref()
        .map(|artifact| {
            fields(
                &artifact.name,
                artifact.version.destination,
                &artifact.body,
                &artifact.references,
                &artifact.layout,
                Some(&artifact.definitions),
            )
        })
        .transpose()?;
    let after = fields(
        &member.name,
        member.destination,
        &member.body,
        &member.references,
        catalog
            .document
            .layouts
            .get(&member.key)
            .unwrap_or(&BTreeMap::new()),
        catalog.document.definitions.get(&member.key),
    )?;
    diff.compare("", before.as_ref(), Some(&after));
    let affected_uses = member
        .base
        .as_ref()
        .map(|target| uses_of(catalog, target, published))
        .transpose()?
        .unwrap_or_default();
    for (slot, reference) in &member.references {
        match reference {
            PackageReference::Draft { key } if catalog.draft_member(key).is_err() => diagnostics
                .push(format!(
                    "The reference {slot} targets missing draft member {key}."
                )),
            PackageReference::Published { version } => {
                match publish::load_published(catalog.db, catalog.project, version)? {
                    Some(artifact) => requirements.extend(publish::dependency_blockers(
                        &member.body,
                        slot,
                        &artifact,
                    )),
                    None => diagnostics.push(format!(
                        "The published reference {slot} is unavailable in this scope."
                    )),
                }
            }
            PackageReference::Parameter { name, .. } => {
                requirements.push(format!("Bind {name} for execution."))
            }
            _ => {}
        }
    }
    let mut graph_definitions = catalog
        .document
        .definitions
        .get(&member.key)
        .cloned()
        .unwrap_or_default();
    match &member.body {
        PackageArtifactBody::Pipeline { .. }
        | PackageArtifactBody::Subflow { .. }
        | PackageArtifactBody::Prompt { .. } => match catalog.compile(member, &mut requirements) {
            Ok((_, definitions)) => graph_definitions = definitions,
            Err(error) => diagnostics.push(format!("{error:#}")),
        },
        PackageArtifactBody::McpTool { binding, .. } => {
            requirements.push(format!("Bind local MCP server {binding} for execution."));
        }
        PackageArtifactBody::Schema { fields } => {
            if let Err(error) = crate::workflows::schema::object(fields) {
                diagnostics.push(format!("{error:#}"));
            }
        }
        PackageArtifactBody::Command { binding, .. } => {
            if let PackageCommandBinding::Required { name } = binding {
                requirements.push(format!("Register and bind {name} for execution."));
            }
        }
        _ => {}
    }
    requirements.sort();
    requirements.dedup();
    Ok(MemberPreview {
        key: member.key.clone(),
        name: member.name.clone(),
        kind: member.body.kind(),
        base: member.base.clone(),
        changes: diff.changes,
        changes_truncated: diff.truncated,
        dependencies: member.references.clone(),
        affected_uses,
        diagnostics,
        requirements,
        graph_definitions,
    })
}

fn published_consumers(db: &Connection, project: &str) -> Result<Vec<PublishedPackageArtifact>> {
    let scope = PackageDestination::Project.key(project);
    let rows = db
        .prepare(
            "SELECT scope_key, kind, artifact_id, revision FROM workflow_package_artifact_heads
        WHERE scope_key IN (?1, 'library') ORDER BY scope_key, kind, artifact_id",
        )?
        .query_map([&scope], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(scope, kind, id, revision)| {
            let version = PackageArtifactVersion {
                kind: serde_json::from_value(Value::String(kind))?,
                id,
                revision,
                destination: if scope == "library" {
                    PackageDestination::Library
                } else {
                    PackageDestination::Project
                },
            };
            publish::load_published(db, project, &version)?
                .context("A published consumer revision is unavailable")
        })
        .collect()
}

fn uses_of(
    catalog: &ReviewCatalog<'_>,
    target: &PackageArtifactVersion,
    published: &[PublishedPackageArtifact],
) -> Result<Vec<PublishedUse>> {
    let mut uses = Vec::new();
    let mut loaded = BTreeMap::new();
    for consumer in published {
        let mut queue = consumer
            .references
            .iter()
            .filter_map(published_reference)
            .collect::<Vec<_>>();
        let mut seen = BTreeSet::new();
        while let Some((slot, version)) = queue.pop() {
            if version.kind == target.kind
                && version.id == target.id
                && version.destination == target.destination
            {
                uses.push(PublishedUse {
                    consumer: consumer.version.clone(),
                    name: consumer.name.clone(),
                    slot,
                    pinned_revision: version.revision.clone(),
                });
                continue;
            }
            if !seen.insert(version.clone()) {
                continue;
            }
            if !loaded.contains_key(&version) {
                let references = catalog
                    .load(&version)?
                    .map(|artifact| artifact.references)
                    .unwrap_or_default();
                loaded.insert(version.clone(), references);
            }
            queue.extend(
                loaded[&version]
                    .iter()
                    .filter_map(published_reference)
                    .map(|(inner, version)| (format!("{slot} → {inner}"), version)),
            );
        }
    }
    Ok(uses)
}

fn published_reference(
    (slot, reference): (&String, &ResolvedPackageReference),
) -> Option<(String, PackageArtifactVersion)> {
    let ResolvedPackageReference::Published { version } = reference else {
        return None;
    };
    Some((slot.clone(), version.clone()))
}

fn fields(
    name: &str,
    destination: PackageDestination,
    body: &PackageArtifactBody,
    references: &impl Serialize,
    layout: &BTreeMap<String, WorkflowNodePosition>,
    definitions: Option<&BTreeMap<String, crate::workflows::NodeDefinition>>,
) -> Result<Value> {
    let mut value = serde_json::to_value(body)?;
    if let PackageArtifactBody::Pipeline { graph } | PackageArtifactBody::Subflow { graph, .. } =
        body
    {
        let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|node| (&node.id, node)).collect();
        let wires: BTreeMap<_, _> = graph
            .wires
            .iter()
            .map(|wire| {
                let endpoint =
                    |end: &crate::workflows::Endpoint| format!("{:?}.{:?}", end.node, end.port);
                (
                    format!("{} → {}", endpoint(&wire.from), endpoint(&wire.to)),
                    wire,
                )
            })
            .collect();
        value["graph"] = json!({ "name": graph.name, "nodes": nodes, "wires": wires });
    }
    let definition_versions: BTreeMap<_, _> = definitions
        .into_iter()
        .flat_map(|pins| pins.iter())
        .map(|(id, definition)| (id, definition.revision))
        .collect();
    Ok(
        json!({ "name": name, "destination": destination, "body": value, "references": references,
        "layout": layout, "definitionVersions": definition_versions }),
    )
}

#[derive(Default)]
struct FieldDiff {
    changes: Vec<FieldChange>,
    truncated: bool,
}

impl FieldDiff {
    fn compare(&mut self, path: &str, before: Option<&Value>, after: Option<&Value>) {
        if before == after || self.truncated {
            return;
        }
        if self.changes.len() == 240 {
            self.truncated = true;
            return;
        }
        let left = before.and_then(Value::as_object);
        let right = after.and_then(Value::as_object);
        let graph_container = matches!(
            path,
            "/body" | "/body/graph" | "/body/graph/nodes" | "/body/graph/wires"
        );
        if left.is_some() && right.is_some() || path.is_empty() || graph_container {
            let keys: BTreeSet<_> = left
                .into_iter()
                .chain(right)
                .flat_map(|object| object.keys())
                .collect();
            for key in keys {
                let name = key.replace('~', "~0").replace('/', "~1");
                self.compare(
                    &format!("{path}/{name}"),
                    left.and_then(|object| object.get(key)),
                    right.and_then(|object| object.get(key)),
                );
            }
        } else {
            let change = match (before, after) {
                (None, _) => "added",
                (_, None) => "removed",
                _ => "changed",
            };
            self.changes.push(FieldChange {
                path: path.into(),
                change,
                before: before.cloned(),
                after: after.cloned(),
            });
        }
    }
}
