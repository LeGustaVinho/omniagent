use crate::workflows::{BuiltinNode, Effect, Endpoint, NodeConfiguration, PortType, digest};

use super::{
    preview::ReviewCatalog,
    publish::{head_revision, load_published},
    *,
};

mod bindings;
mod credentials;
#[cfg(test)]
mod tests;

use bindings::{DestinationBindings, LocalCatalog};
pub use bindings::{ExchangeBinding, ExchangeBindingKind, ExchangeBindingRequirement};

const FORMAT: &str = "omniagent.workflow-package";
const VERSION: u16 = 1;
const MAX_EXCHANGE_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowExchange {
    pub format: String,
    pub version: u16,
    pub package: WorkflowPackage,
    pub sources: BTreeMap<String, PackageArtifactVersion>,
    pub definitions: BTreeMap<String, BTreeMap<String, NodeDefinition>>,
    pub bindings: BTreeMap<String, ExchangeBindingRequirement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangePermission {
    RepositoryWritesRequireRelease,
    ChangesLocalGitState,
    ImplementationReleaseUsesDestinationPolicy,
    ExternalMcpRequiresLocalApproval,
    ModelRequiresLocalAuthentication,
    AgentCanChangeTaskRecords,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeIssue {
    pub member: String,
    pub node: Option<String>,
    pub port: Option<String>,
    pub message: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExchangePreview {
    pub name: String,
    pub members: BTreeMap<String, PackageArtifactVersion>,
    pub bindings: BTreeMap<String, ExchangeBindingRequirement>,
    pub missing_bindings: Vec<String>,
    pub destination_notes: Vec<String>,
    pub permissions: BTreeSet<ExchangePermission>,
    pub conflicts: Vec<ExchangeIssue>,
    pub can_import: bool,
    pub review_digest: String,
}

impl Store {
    pub async fn export_workflow_exchange(
        &self,
        project: String,
        selected: Vec<PackageArtifactVersion>,
    ) -> Result<WorkflowExchange> {
        let local = LocalCatalog::load(self, &project).await?;
        self.execute(move |db| {
            let tx = db.transaction()?;
            require_project(&tx, &project)?;
            let artifacts = collect_dependencies(&tx, &project, selected)?;
            let document = build_exchange(&local, artifacts)?;
            let serialized = serde_json::to_value(&document)?;
            ensure!(
                serde_json::to_vec(&serialized)?.len() <= MAX_EXCHANGE_BYTES,
                "The workflow exchange exceeds the size limit"
            );
            credentials::check(&serialized)?;
            Ok(document)
        })
        .await
    }

    pub async fn preview_workflow_exchange(
        &self,
        project: String,
        document: String,
        bindings: BTreeMap<String, ExchangeBinding>,
    ) -> Result<WorkflowExchangePreview> {
        let document = parse(&document, &bindings)?;
        let local = LocalCatalog::load(self, &project).await?;
        self.execute(move |db| {
            let tx = db.transaction()?;
            let (preview, _) = prepare(&tx, &project, document, bindings, &local)?;
            Ok(preview)
        })
        .await
    }

    pub async fn import_reviewed_workflow_exchange(
        &self,
        project: String,
        document: String,
        bindings: BTreeMap<String, ExchangeBinding>,
        reviewed_digest: String,
    ) -> Result<WorkflowPackageDraft> {
        let document = parse(&document, &bindings)?;
        let local = LocalCatalog::load(self, &project).await?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (preview, package) = prepare(&tx, &project, document, bindings, &local)?;
            ensure!(
                preview.review_digest == reviewed_digest,
                "The destination or package changed; review the new import preview"
            );
            ensure!(
                preview.can_import,
                "Resolve the package conflicts and required local bindings before importing"
            );
            let draft = create_draft(&tx, &project, None, package)?;
            tx.commit()?;
            Ok(draft)
        })
        .await
    }
}

fn require_project(db: &Connection, project: &str) -> Result<()> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [project],
        |row| row.get(0),
    )?;
    ensure!(exists, "Select a registered project for workflow exchange");
    Ok(())
}

fn collect_dependencies(
    db: &Connection,
    project: &str,
    selected: Vec<PackageArtifactVersion>,
) -> Result<BTreeMap<PackageArtifactVersion, PublishedPackageArtifact>> {
    ensure!(
        !selected.is_empty() && selected.len() <= MAX_MEMBERS,
        "Select between one and {MAX_MEMBERS} published revisions"
    );
    let mut pending = selected.into_iter().collect::<BTreeSet<_>>();
    let mut artifacts = BTreeMap::new();
    while let Some(version) = pending.pop_first() {
        if artifacts.contains_key(&version) {
            continue;
        }
        ensure!(
            artifacts.len() < MAX_MEMBERS,
            "The dependency closure exceeds {MAX_MEMBERS} artifacts"
        );
        let artifact = load_published(db, project, &version)?
            .context("A selected revision or dependency is not visible in this project")?;
        for entry in artifact.references.values() {
            if let ResolvedPackageReference::Published { version } = entry {
                pending.insert(version.clone());
            }
        }
        artifacts.insert(version, artifact);
    }
    Ok(artifacts)
}

fn build_exchange(
    local: &LocalCatalog,
    artifacts: BTreeMap<PackageArtifactVersion, PublishedPackageArtifact>,
) -> Result<WorkflowExchange> {
    let keys = artifacts
        .keys()
        .enumerate()
        .map(|(index, version)| (version.clone(), format!("artifact-{}", index + 1)))
        .collect::<BTreeMap<_, _>>();
    let kinds = keys
        .iter()
        .map(|(version, key)| (key.clone(), version.kind))
        .collect::<BTreeMap<_, _>>();
    let mut sources = BTreeMap::new();
    let mut definitions = BTreeMap::new();
    let mut requirements = BTreeMap::new();
    let mut members = Vec::with_capacity(artifacts.len());
    for (source, artifact) in artifacts {
        let key = keys[&source].clone();
        let references = artifact
            .references
            .into_iter()
            .map(|(slot, reference)| {
                let reference = match reference {
                    ResolvedPackageReference::Published { version } => PackageReference::Draft {
                        key: keys[&version].clone(),
                    },
                    ResolvedPackageReference::Parameter { name, kind } => {
                        PackageReference::Parameter { name, kind }
                    }
                };
                (slot, reference)
            })
            .collect();
        let mut member = PackageMember {
            key: key.clone(),
            name: artifact.name,
            destination: source.destination,
            base: None,
            body: artifact.body,
            references,
        };
        bindings::export_member(local, &mut member, &kinds, &mut requirements)?;
        sources.insert(key.clone(), source);
        definitions.insert(key, artifact.definitions);
        members.push(member);
    }
    let package = WorkflowPackage {
        name: if members.len() == 1 {
            members[0].name.clone()
        } else {
            "Workflow package exchange".into()
        },
        members,
    };
    validate_structure(&package)?;
    Ok(WorkflowExchange {
        format: FORMAT.into(),
        version: VERSION,
        package,
        sources,
        definitions,
        bindings: requirements,
    })
}

fn parse(document: &str, bindings: &BTreeMap<String, ExchangeBinding>) -> Result<WorkflowExchange> {
    ensure!(
        document.len() <= MAX_EXCHANGE_BYTES
            && serde_json::to_vec(bindings)?.len() <= MAX_PACKAGE_BYTES,
        "The workflow exchange or local bindings exceed the size limit"
    );
    let exchange: WorkflowExchange =
        serde_json::from_str(document).context("Invalid declarative workflow exchange")?;
    ensure!(
        exchange.format == FORMAT && exchange.version == VERSION,
        "Unsupported workflow exchange version"
    );
    validate_structure(&exchange.package)?;
    let keys = exchange
        .package
        .members
        .iter()
        .map(|member| &member.key)
        .collect::<BTreeSet<_>>();
    ensure!(
        !keys.is_empty()
            && exchange.sources.keys().collect::<BTreeSet<_>>() == keys
            && exchange.definitions.keys().collect::<BTreeSet<_>>() == keys,
        "Every imported artifact needs its source revision and node definitions"
    );
    for member in &exchange.package.members {
        let source = &exchange.sources[&member.key];
        bounded_name(&source.id)?;
        bounded_name(&source.revision)?;
        ensure!(
            member.base.is_none()
                && source.kind == member.body.kind()
                && source.destination == member.destination,
            "Imported artifact {} must create a new local draft of the same kind",
            member.key
        );
        for reference in member.references.values() {
            match reference {
                PackageReference::Draft { key } => ensure!(
                    keys.contains(key),
                    "The dependency {key} is missing from the exchange"
                ),
                PackageReference::Published { .. } => {
                    anyhow::bail!("Published dependencies must be included inside the exchange")
                }
                PackageReference::Parameter { .. } => {}
            }
        }
    }
    validate_dependency_order(&exchange.package)?;
    Ok(exchange)
}

fn validate_dependency_order(package: &WorkflowPackage) -> Result<()> {
    let members = package
        .members
        .iter()
        .map(|member| (&member.key, member))
        .collect::<BTreeMap<_, _>>();
    for member in members.values() {
        for reference in member.references.values() {
            if let PackageReference::Draft { key } = reference {
                ensure!(
                    member.destination == PackageDestination::Project
                        || members[key].destination == PackageDestination::Library,
                    "A library dependency needs a library artifact or a named parameter"
                );
            }
        }
    }
    let mut pending = package.members.iter().collect::<Vec<_>>();
    let mut ready = BTreeSet::new();
    while !pending.is_empty() {
        let index = pending
            .iter()
            .position(|member| {
                member.references.values().all(|reference| match reference {
                    PackageReference::Draft { key } => ready.contains(key),
                    _ => true,
                })
            })
            .context("The exchange contains a dependency cycle")?;
        ready.insert(&pending.remove(index).key);
    }
    Ok(())
}

fn prepare(
    db: &Connection,
    project: &str,
    exchange: WorkflowExchange,
    choices: BTreeMap<String, ExchangeBinding>,
    local: &LocalCatalog,
) -> Result<(WorkflowExchangePreview, WorkflowPackage)> {
    require_project(db, project)?;
    let digest_input = serde_json::to_vec(&(&exchange, &choices))?;
    let WorkflowExchange {
        package,
        sources,
        definitions,
        bindings,
        ..
    } = exchange;
    let mut package = package;
    let kinds = package
        .members
        .iter()
        .map(|member| (member.key.clone(), member.body.kind()))
        .collect();
    let mut bound = DestinationBindings::new(db, project, local, &bindings, &choices)?;
    for member in &mut package.members {
        bound.apply(member, &kinds)?;
    }
    let (missing, mut notes, mut conflicts) = bound.finish()?;
    let document = PackageDocument::capture(db, project, package, None)?;
    let permissions = permissions(&document);
    let catalog = ReviewCatalog::new(db, project, &document, "exchange-review");
    let mut local_heads = BTreeMap::new();
    for member in &document.package.members {
        for reference in member.references.values() {
            if let PackageReference::Parameter { name, .. } = reference {
                notes.push(format!(
                    "Bind {name} in the destination workflow before execution."
                ));
            }
        }
        compare_definitions(
            &member.key,
            &definitions[&member.key],
            &document.definitions[&member.key],
            &mut conflicts,
        );
        let pins = &document.definitions[&member.key];
        let expanded = match &member.body {
            PackageArtifactBody::Pipeline { .. }
            | PackageArtifactBody::Subflow { .. }
            | PackageArtifactBody::Prompt { .. } => match catalog.compile(member, &mut notes) {
                Ok(definitions) => Some(definitions),
                Err(error) => {
                    conflicts.push(issue(
                        &member.key,
                        None,
                        None,
                        crate::diagnostics::preview(&error.to_string()),
                    ));
                    None
                }
            },
            _ => {
                if let Err(error) = validate_member_body(member, pins) {
                    conflicts.push(issue(
                        &member.key,
                        None,
                        None,
                        crate::diagnostics::preview(&error.to_string()),
                    ));
                }
                None
            }
        };
        let source = &sources[&member.key];
        if let Some(revision) = head_revision(db, project, source)? {
            let head = PackageArtifactVersion {
                revision,
                ..source.clone()
            };
            if matches!(member.body, PackageArtifactBody::Subflow { .. })
                && let Some(existing) = load_published(db, project, &head)?
            {
                compare_public_ports(
                    member,
                    expanded.as_ref().unwrap_or(pins),
                    &existing,
                    published_subflow_ports(db, project, &existing).as_ref(),
                    &mut conflicts,
                );
            }
            local_heads.insert(member.key.clone(), head);
        }
    }
    notes.sort_unstable();
    notes.dedup();
    let can_import = missing.is_empty() && !conflicts.iter().any(|issue| issue.blocking);
    let name = document.package.name.clone();
    let review_digest = digest(&serde_json::to_vec(&(
        project,
        digest(&digest_input),
        &local_heads,
        &missing,
        &notes,
        &conflicts,
        &permissions,
    ))?);
    let preview = WorkflowExchangePreview {
        name,
        members: sources,
        bindings,
        missing_bindings: missing,
        destination_notes: notes,
        permissions,
        conflicts,
        can_import,
        review_digest,
    };
    Ok((preview, document.package))
}

fn validate_member_body(
    member: &PackageMember,
    pins: &BTreeMap<String, NodeDefinition>,
) -> Result<()> {
    match &member.body {
        PackageArtifactBody::Role { template } => {
            crate::workflows::role_templates::validate_conversation_template(
                &mut template.clone(),
            )?;
            ensure!(
                template.requirements.is_valid(),
                "The role requirements are invalid or exceed the limits"
            );
        }
        PackageArtifactBody::Preset {
            node_kind,
            configuration,
            editable_fields,
        } => {
            let definition = pins
                .get("$preset")
                .filter(|pin| pin.kind == *node_kind)
                .context("The preset's reviewed node definition is unavailable")?;
            ensure!(
                crate::workflows::registry::supports_pinned_execution(definition)
                    && configuration.matches(definition.configuration),
                "The preset needs a valid, supported node configuration"
            );
            let fields = serde_json::to_value(configuration)?;
            let keys = editable_fields.iter().collect::<BTreeSet<_>>();
            ensure!(
                keys.len() == editable_fields.len()
                    && editable_fields
                        .iter()
                        .all(|key| key != "kind" && fields.get(key).is_some()),
                "The preset exposes an unknown or repeated editable field"
            );
        }
        _ => {}
    }
    Ok(())
}

fn issue(
    member: &str,
    node: Option<&str>,
    port: Option<&str>,
    message: impl Into<String>,
) -> ExchangeIssue {
    ExchangeIssue {
        member: member.into(),
        node: node.map(str::to_owned),
        port: port.map(str::to_owned),
        message: message.into(),
        blocking: true,
    }
}

fn permissions(document: &PackageDocument) -> BTreeSet<ExchangePermission> {
    let mut permissions = BTreeSet::new();
    for node in document
        .definitions
        .values()
        .flat_map(|nodes| nodes.values())
    {
        match node.effect {
            Effect::WriteRepository => {
                permissions.insert(ExchangePermission::RepositoryWritesRequireRelease);
            }
            Effect::ProvisionManagedCheckout
            | Effect::VerifyRepositoryCommit
            | Effect::VerifyRepositoryMerge => {
                permissions.insert(ExchangePermission::ChangesLocalGitState);
            }
            Effect::ReleaseImplementation => {
                permissions.insert(ExchangePermission::ImplementationReleaseUsesDestinationPolicy);
            }
            Effect::InvokeModel => {
                permissions.insert(ExchangePermission::ModelRequiresLocalAuthentication);
            }
            Effect::ChangeTaskRecords => {
                permissions.insert(ExchangePermission::AgentCanChangeTaskRecords);
            }
            Effect::InvokeLocalTool => {
                permissions.insert(ExchangePermission::ExternalMcpRequiresLocalApproval);
            }
            _ => {}
        }
    }
    for member in &document.package.members {
        if let PackageArtifactBody::Prompt { prompt } = &member.body {
            permissions.insert(ExchangePermission::ModelRequiresLocalAuthentication);
            if prompt.executor == BuiltinNode::Implement {
                permissions.insert(ExchangePermission::RepositoryWritesRequireRelease);
            }
        }
        let permission = match member.body.kind() {
            PackageArtifactKind::Command => ExchangePermission::RepositoryWritesRequireRelease,
            PackageArtifactKind::Role => ExchangePermission::ModelRequiresLocalAuthentication,
            PackageArtifactKind::McpTool => ExchangePermission::ExternalMcpRequiresLocalApproval,
            _ => continue,
        };
        permissions.insert(permission);
    }
    permissions
}

fn compare_definitions(
    member: &str,
    source: &BTreeMap<String, NodeDefinition>,
    current: &BTreeMap<String, NodeDefinition>,
    issues: &mut Vec<ExchangeIssue>,
) {
    for key in source.keys().chain(current.keys()).collect::<BTreeSet<_>>() {
        match (source.get(key), current.get(key)) {
            (Some(before), Some(after)) => {
                if before.kind != after.kind
                    || before.executor != after.executor
                    || before.effect != after.effect
                    || before.configuration != after.configuration
                    || before.revision != after.revision
                {
                    issues.push(issue(
                        member,
                        Some(key),
                        None,
                        "The destination has a different node definition",
                    ));
                }
                for (direction, a, b) in [
                    ("input", &before.inputs, &after.inputs),
                    ("output", &before.outputs, &after.outputs),
                ] {
                    let a = a
                        .iter()
                        .map(|port| (port.name.clone(), Some(port.value)))
                        .collect::<BTreeMap<_, _>>();
                    let b = b
                        .iter()
                        .map(|port| (port.name.clone(), Some(port.value)))
                        .collect::<BTreeMap<_, _>>();
                    compare_ports(member, Some(key), direction, &a, &b, issues);
                }
            }
            _ => issues.push(issue(
                member,
                Some(key),
                None,
                "The import and destination have different node definitions",
            )),
        }
    }
}

fn compare_public_ports(
    member: &PackageMember,
    incoming: &BTreeMap<String, NodeDefinition>,
    existing: &PublishedPackageArtifact,
    installed: Option<&BTreeMap<String, NodeDefinition>>,
    issues: &mut Vec<ExchangeIssue>,
) {
    let (
        PackageArtifactBody::Subflow {
            graph,
            inputs,
            outputs,
        },
        PackageArtifactBody::Subflow {
            graph: old_graph,
            inputs: old_inputs,
            outputs: old_outputs,
        },
    ) = (&member.body, &existing.body)
    else {
        return;
    };
    for (direction, before, after) in [
        ("public input", old_inputs, inputs),
        ("public output", old_outputs, outputs),
    ] {
        let old_ports = interface_ports(
            old_graph,
            installed.unwrap_or(&existing.definitions),
            before,
        );
        let new_ports = interface_ports(graph, incoming, after);
        let start = issues.len();
        compare_ports(&member.key, None, direction, &old_ports, &new_ports, issues);
        // An import creates a separate artifact; installed consumers keep their old interface.
        for issue in &mut issues[start..] {
            issue.blocking = false;
        }
    }
}

fn published_subflow_ports(
    db: &Connection,
    project: &str,
    existing: &PublishedPackageArtifact,
) -> Option<BTreeMap<String, NodeDefinition>> {
    let member = PackageMember {
        key: "installed".into(),
        name: existing.name.clone(),
        destination: existing.version.destination,
        base: None,
        body: existing.body.clone(),
        references: existing
            .references
            .iter()
            .map(|(slot, reference)| {
                let reference = match reference {
                    ResolvedPackageReference::Published { version } => {
                        PackageReference::Published {
                            version: version.clone(),
                        }
                    }
                    ResolvedPackageReference::Parameter { name, kind } => {
                        PackageReference::Parameter {
                            name: name.clone(),
                            kind: *kind,
                        }
                    }
                };
                (slot.clone(), reference)
            })
            .collect(),
    };
    let document = PackageDocument {
        package: WorkflowPackage {
            name: existing.name.clone(),
            members: vec![member],
        },
        definitions: BTreeMap::from([("installed".into(), existing.definitions.clone())]),
        layouts: BTreeMap::new(),
    };
    ReviewCatalog::new(db, project, &document, "exchange-installed")
        .compile(&document.package.members[0], &mut Vec::new())
        .ok()
}

fn interface_ports(
    graph: &crate::workflows::WorkflowGraph,
    definitions: &BTreeMap<String, NodeDefinition>,
    endpoints: &BTreeMap<String, Endpoint>,
) -> BTreeMap<String, Option<PortType>> {
    endpoints
        .iter()
        .map(|(name, endpoint)| {
            let declared = graph
                .nodes
                .iter()
                .find(|node| node.id == endpoint.node)
                .and_then(|node| match &node.configuration {
                    NodeConfiguration::SubflowEntry { outputs }
                        if node.kind == BuiltinNode::Start =>
                    {
                        Some(outputs)
                    }
                    _ => None,
                });
            let ports = declared.or_else(|| {
                definitions
                    .get(&endpoint.node)
                    .map(|definition| &definition.outputs)
            });
            (
                name.clone(),
                ports
                    .and_then(|ports| ports.iter().find(|port| port.name == endpoint.port))
                    .map(|port| port.value),
            )
        })
        .collect()
}

fn compare_ports(
    member: &str,
    node: Option<&str>,
    direction: &str,
    before: &BTreeMap<String, Option<PortType>>,
    after: &BTreeMap<String, Option<PortType>>,
    issues: &mut Vec<ExchangeIssue>,
) {
    for port in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
        if before.get(port) != after.get(port) {
            issues.push(issue(
                member,
                node,
                Some(port),
                format!("The {direction} {port} disagrees with the destination contract"),
            ));
        }
    }
}
