use crate::workflows::WorkflowGraph;

use super::*;

fn member_mut<'a>(package: &'a mut WorkflowPackage, key: &str) -> Result<&'a mut PackageMember> {
    package
        .members
        .iter_mut()
        .find(|member| member.key == key)
        .context("The selected package member is unavailable")
}

fn graph_mut<'a>(package: &'a mut WorkflowPackage, key: &str) -> Result<&'a mut WorkflowGraph> {
    match &mut member_mut(package, key)?.body {
        PackageArtifactBody::Pipeline { graph } | PackageArtifactBody::Subflow { graph, .. } => {
            Ok(graph)
        }
        _ => anyhow::bail!("The selected member does not contain a graph"),
    }
}

pub(super) fn apply(package: &mut WorkflowPackage, operation: &DraftOperation) -> Result<()> {
    match operation {
        DraftOperation::RenamePackage { name } => package.name.clone_from(name),
        DraftOperation::PutMember { member } => {
            if let Some(existing) = package
                .members
                .iter_mut()
                .find(|existing| existing.key == member.key)
            {
                ensure!(
                    existing.base.is_none() || existing.base == member.base,
                    "A selected published base is fixed for this authoring request"
                );
                existing.clone_from(member);
            } else {
                package.members.push((**member).clone());
            }
        }
        DraftOperation::RemoveMember { key } => {
            ensure!(
                member_mut(package, key)?.base.is_none(),
                "The selected published artifact must stay in this package"
            );
            package.members.retain(|member| member.key != *key);
        }
        DraftOperation::PutNode { member_key, node } => {
            let graph = graph_mut(package, member_key)?;
            bounded_name(&node.id)?;
            if let Some(existing) = graph
                .nodes
                .iter_mut()
                .find(|existing| existing.id == node.id)
            {
                existing.clone_from(node);
            } else {
                graph.nodes.push(node.clone());
            }
        }
        DraftOperation::RemoveNode {
            member_key,
            node_id,
        } => {
            let graph = graph_mut(package, member_key)?;
            ensure!(
                graph.nodes.iter().any(|node| node.id == *node_id),
                "The selected node is unavailable"
            );
            graph.nodes.retain(|node| node.id != *node_id);
            graph
                .wires
                .retain(|wire| wire.from.node != *node_id && wire.to.node != *node_id);
            member_mut(package, member_key)?.references.remove(node_id);
        }
        DraftOperation::PutWire { member_key, wire } => {
            let graph = graph_mut(package, member_key)?;
            ensure!(
                graph.nodes.iter().any(|node| node.id == wire.from.node)
                    && graph.nodes.iter().any(|node| node.id == wire.to.node),
                "The proposed connection needs an existing source and target node"
            );
            if !graph.wires.contains(wire) {
                graph.wires.push(wire.clone());
            }
        }
        DraftOperation::RemoveWire { member_key, wire } => {
            let graph = graph_mut(package, member_key)?;
            ensure!(
                graph.wires.contains(wire),
                "The selected connection is unavailable"
            );
            graph.wires.retain(|existing| existing != wire);
        }
        DraftOperation::SetReference {
            member_key,
            slot,
            reference,
        } => {
            let references = &mut member_mut(package, member_key)?.references;
            if let Some(reference) = reference {
                references.insert(slot.clone(), reference.clone());
            } else {
                references.remove(slot);
            }
        }
    }
    Ok(())
}

pub(super) fn validate_visible(
    db: &Connection,
    project: &str,
    package: &WorkflowPackage,
) -> Result<()> {
    for member in &package.members {
        if let Some(base) = &member.base {
            publish::load_published(db, project, base)?
                .context("The selected base is outside the visible catalog")?;
        }
        for reference in member.references.values() {
            if let PackageReference::Published { version } = reference {
                ensure!(
                    member.destination == PackageDestination::Project
                        || version.destination == PackageDestination::Library,
                    "Library references need a library revision or a binding parameter"
                );
                publish::load_published(db, project, version)?
                    .context("The published reference is outside the visible catalog")?;
            }
        }
    }
    Ok(())
}

pub(super) fn validate_submission(
    db: &Connection,
    project: &str,
    package: &WorkflowPackage,
) -> Result<()> {
    ensure!(
        !package.members.is_empty(),
        "Add an artifact before submitting a package for review"
    );
    validate_visible(db, project, package)?;
    let members = package
        .members
        .iter()
        .map(|member| (&member.key, member))
        .collect::<BTreeMap<_, _>>();
    let mut unsorted = BTreeMap::new();
    for member in &package.members {
        let mut dependencies = BTreeSet::new();
        for reference in member.references.values() {
            if let PackageReference::Draft { key } = reference {
                let dependency = members
                    .get(key)
                    .context("A package reference is missing its member")?;
                ensure!(
                    member.destination == PackageDestination::Project
                        || dependency.destination == PackageDestination::Library,
                    "Library references need a library revision or a binding parameter"
                );
                dependencies.insert(key);
            }
        }
        unsorted.insert(&member.key, dependencies);
    }
    while !unsorted.is_empty() {
        let ready = unsorted
            .iter()
            .find(|(_, dependencies)| dependencies.is_empty())
            .map(|(key, _)| *key)
            .context("The package references contain a cycle")?;
        unsorted.remove(ready);
        for dependencies in unsorted.values_mut() {
            dependencies.remove(ready);
        }
    }
    Ok(())
}
