use rusqlite::{Connection, TransactionBehavior};

use crate::{
    store::Store,
    workflows::{Port, builtin_registry},
};

use super::{
    super::{
        PackageArtifactVersion, PackageDocument, PublishedPackageArtifact,
        ResolvedPackageReference, WorkflowPackage, WorkflowPackageDraft, create_draft_document,
        load_draft, persist_revision, preview::ReviewCatalog, publish::load_published,
        read_document, request, validate_structure,
    },
    *,
};

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReusableSource {
    PackageRevision {
        draft_id: String,
        revision: String,
        member_key: String,
    },
    Published {
        version: PackageArtifactVersion,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowReusableNode {
    pub artifact: PublishedPackageArtifact,
    pub definition: NodeDefinition,
    pub graph_definitions: BTreeMap<String, NodeDefinition>,
    pub editable_overrides: Vec<String>,
}

impl Store {
    pub async fn describe_workflow_reusable_boundary(
        &self,
        project: String,
        source: ReusableSource,
        node_ids: Option<Vec<String>>,
    ) -> Result<SubflowBoundary> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let loaded = load_source(&tx, &project, &source)?;
            loaded.with_source(&tx, &project, |source| {
                describe_subflow_boundary(source, node_ids.as_deref())
            })
        })
        .await
    }

    pub async fn derive_workflow_reusable(
        &self,
        project: String,
        source: ReusableSource,
        derivation: ReusableDerivation,
    ) -> Result<WorkflowPackageDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let ReusableSource::PackageRevision {
                draft_id, revision, ..
            } = &source
            {
                let current = load_draft(&tx, &project, draft_id)?;
                request::reconcile(&tx, &project, &current)?;
                let current = load_draft(&tx, &project, draft_id)?;
                ensure!(
                    current.publication.is_none(),
                    "Select a published artifact to reuse a published package"
                );
                if current.revision != *revision {
                    tx.commit()?;
                    anyhow::bail!(
                        "The source package changed. Review the new revision before saving."
                    );
                }
                ensure!(
                    !current
                        .request
                        .as_ref()
                        .is_some_and(|request| request.pending()),
                    "Wait for the package's active authoring request before deriving reusable nodes"
                );
            }
            let loaded = load_source(&tx, &project, &source)?;
            let derived =
                loaded.with_source(&tx, &project, |source| derive_member(source, derivation))?;
            let key = derived.member.key.clone();
            let mut document = match &source {
                ReusableSource::PackageRevision { .. } => loaded.document,
                ReusableSource::Published { .. } => PackageDocument {
                    package: WorkflowPackage {
                        name: derived.member.name.clone(),
                        members: vec![],
                    },
                    definitions: BTreeMap::new(),
                    layouts: BTreeMap::new(),
                },
            };
            document
                .definitions
                .insert(key.clone(), derived.definitions);
            document.package.members.push(derived.member);
            validate_structure(&document.package)?;
            let catalog = ReviewCatalog::new(&tx, &project, &document, "derived-reusable");
            let member = catalog.draft_member(&key)?;
            if matches!(&member.body, PackageArtifactBody::Subflow { .. }) {
                catalog.compile(member, &mut Vec::new())?;
            }
            let saved = match source {
                ReusableSource::PackageRevision {
                    draft_id, revision, ..
                } => {
                    persist_revision(&tx, &draft_id, Some(&revision), None, &document)?;
                    load_draft(&tx, &project, &draft_id)?
                }
                ReusableSource::Published { .. } => {
                    create_draft_document(&tx, &project, None, &document)?
                }
            };
            tx.commit()?;
            Ok(saved)
        })
        .await
    }

    pub async fn describe_workflow_reusable_node(
        &self,
        project: String,
        version: PackageArtifactVersion,
    ) -> Result<WorkflowReusableNode> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let artifact = exact_artifact(&tx, &project, &version)?;
            let (mut definition, graph_definitions, editable_overrides) = match &artifact.body {
                PackageArtifactBody::Preset {
                    configuration,
                    editable_fields,
                    ..
                } => (
                    artifact
                        .definitions
                        .get("$preset")
                        .context("The preset definition is unavailable")?
                        .clone(),
                    BTreeMap::new(),
                    super::override_fields(configuration, editable_fields),
                ),
                PackageArtifactBody::Subflow {
                    inputs, outputs, ..
                } => {
                    let loaded = LoadedSource::published(artifact.clone());
                    let catalog = loaded.catalog(&tx, &project, &version.revision);
                    let (_, definitions) = catalog
                        .compile(catalog.draft_member(&loaded.member_key)?, &mut Vec::new())?;
                    let mut definition = builtin_registry()
                        .into_iter()
                        .find(|definition| definition.kind == BuiltinNode::Subflow)
                        .context("The reusable subflow executor is unavailable")?;
                    definition.inputs = named_ports(inputs, &definitions)?;
                    definition.outputs = named_ports(outputs, &definitions)?;
                    (definition, definitions, vec![])
                }
                PackageArtifactBody::Prompt { .. } => {
                    let expansion = artifact.prompt_expansion.as_ref()
                        .context("The published Prompt expansion is unavailable")?;
                    let definitions = expansion.workflow.nodes.iter()
                        .map(|node| (node.instance.id.clone(), node.definition.clone()))
                        .collect();
                    let mut definition = builtin_registry()
                        .into_iter()
                        .find(|definition| definition.kind == BuiltinNode::Subflow)
                        .context("The reusable subflow executor is unavailable")?;
                    definition.inputs = named_ports(&expansion.inputs, &definitions)?;
                    definition.outputs = named_ports(&expansion.outputs, &definitions)?;
                    (definition, definitions, vec![])
                }
                _ => anyhow::bail!("Choose a published preset or subflow"),
            };
            definition.title.clone_from(&artifact.name);
            Ok(WorkflowReusableNode {
                artifact,
                definition,
                graph_definitions,
                editable_overrides,
            })
        })
        .await
    }
}

struct LoadedSource {
    document: PackageDocument,
    member_key: String,
    revision: String,
}

impl LoadedSource {
    fn published(artifact: PublishedPackageArtifact) -> Self {
        let member_key = "published-source".to_owned();
        let references = artifact
            .references
            .into_iter()
            .map(|(slot, reference)| {
                let reference = match reference {
                    ResolvedPackageReference::Published { version } => {
                        PackageReference::Published { version }
                    }
                    ResolvedPackageReference::Parameter { name, kind } => {
                        PackageReference::Parameter { name, kind }
                    }
                };
                (slot, reference)
            })
            .collect();
        let member = PackageMember {
            key: member_key.clone(),
            name: artifact.name.clone(),
            destination: artifact.version.destination,
            base: None,
            body: artifact.body,
            references,
        };
        Self {
            revision: artifact.version.revision,
            document: PackageDocument {
                package: WorkflowPackage {
                    name: artifact.name,
                    members: vec![member],
                },
                definitions: BTreeMap::from([(member_key.clone(), artifact.definitions)]),
                layouts: BTreeMap::new(),
            },
            member_key,
        }
    }

    fn catalog<'a>(
        &'a self,
        db: &'a Connection,
        project: &'a str,
        revision: &'a str,
    ) -> ReviewCatalog<'a> {
        ReviewCatalog::new(db, project, &self.document, revision)
    }

    fn with_source<T>(
        &self,
        db: &Connection,
        project: &str,
        run: impl FnOnce(&DerivationSource<'_>) -> Result<T>,
    ) -> Result<T> {
        let catalog = self.catalog(db, project, &self.revision);
        let member = catalog.draft_member(&self.member_key)?;
        let definitions = self
            .document
            .definitions
            .get(&self.member_key)
            .context("The source definition pins are unavailable")?;
        let (_, effective_definitions) = catalog.compile(member, &mut Vec::new())?;
        run(&DerivationSource {
            member,
            definitions,
            effective_definitions: &effective_definitions,
            members: &self.document.package.members,
        })
    }
}

fn load_source(db: &Connection, project: &str, source: &ReusableSource) -> Result<LoadedSource> {
    match source {
        ReusableSource::PackageRevision {
            draft_id,
            revision,
            member_key,
        } => {
            load_draft(db, project, draft_id)?;
            let (_, document) = read_document(db, draft_id, revision)?;
            Ok(LoadedSource {
                document,
                member_key: member_key.clone(),
                revision: revision.clone(),
            })
        }
        ReusableSource::Published { version } => Ok(LoadedSource::published(exact_artifact(
            db, project, version,
        )?)),
    }
}

fn exact_artifact(
    db: &Connection,
    project: &str,
    version: &PackageArtifactVersion,
) -> Result<PublishedPackageArtifact> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [project],
        |row| row.get(0),
    )?;
    ensure!(exists, "Select a registered project");
    load_published(db, project, version)?
        .context("The exact published revision is unavailable in this project")
}

fn named_ports(
    bindings: &BTreeMap<String, Endpoint>,
    definitions: &BTreeMap<String, NodeDefinition>,
) -> Result<Vec<Port>> {
    bindings
        .iter()
        .map(|(name, endpoint)| {
            let value = definitions
                .get(&endpoint.node)
                .and_then(|node| node.outputs.iter().find(|port| port.name == endpoint.port))
                .with_context(|| format!("The public port {name} has no pinned type"))?
                .value;
            Ok(Port {
                name: name.clone(),
                value,
            })
        })
        .collect()
}
