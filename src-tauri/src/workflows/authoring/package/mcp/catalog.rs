use super::*;

const PAGE_SIZE: usize = 6;
const CHUNK_BYTES: usize = 3 * 1024;
const MAX_OFFSET: usize = 16_384;

pub(super) fn read(
    db: &Connection,
    session: &AuthoringSession,
    view: ReadCatalog,
) -> Result<Value> {
    let result = match view {
        ReadCatalog::Draft { offset } => {
            let (_, document) = read_document(db, &session.draft, &session.working)?;
            let mut result = page(
                document.package.members.iter().map(|member| json!({
                    "key":member.key, "name":member.name, "kind":member.body.kind(), "base":member.base,
                    "destination":member.destination,
                })),
                offset,
            )?;
            result["name"] = json!(document.package.name);
            result
        }
        ReadCatalog::BuiltinNodes { offset } => page(
            builtin_registry()
                .into_iter()
                .map(|definition| json!(definition)),
            offset,
        )?,
        ReadCatalog::Published { offset } => published_page(db, &session.project, offset)?,
        ReadCatalog::DraftMember {
            key,
            expected_revision,
            offset,
        } => {
            ensure!(
                session.working == expected_revision,
                "Refresh the draft revision before reading its member"
            );
            let (_, document) = read_document(db, &session.draft, &session.working)?;
            let member = document
                .package
                .members
                .iter()
                .find(|member| member.key == key)
                .context("The package member is unavailable")?;
            chunk(member, offset)?
        }
        ReadCatalog::PublishedMember { version, offset } => {
            let artifact = publish::load_published(db, &session.project, &version)?
                .context("The published revision is outside the visible catalog")?;
            chunk(&artifact, offset)?
        }
    };
    Ok(json!({
        "draftId": session.draft, "authoringRequestId": session.authoring_request,
        "revision": session.working, "reviewHead": session.head, "requestBase": session.base,
        "submittedRevision": session.submitted, "result": result,
    }))
}

fn page(items: impl Iterator<Item = Value>, offset: usize) -> Result<Value> {
    ensure!(
        offset <= MAX_OFFSET,
        "The catalog offset is outside the page bounds"
    );
    let mut items = items.skip(offset).take(PAGE_SIZE + 1).collect::<Vec<_>>();
    let next = (items.len() > PAGE_SIZE).then_some(offset + PAGE_SIZE);
    items.truncate(PAGE_SIZE);
    Ok(json!({"items": items, "nextOffset": next}))
}

fn published_page(db: &Connection, project: &str, offset: usize) -> Result<Value> {
    ensure!(
        offset <= MAX_OFFSET,
        "The catalog offset is outside the page bounds"
    );
    let scope = PackageDestination::Project.key(project);
    let mut query = db.prepare(
        "SELECT scope_key, kind, artifact_id, revision FROM (
           SELECT scope_key, kind, artifact_id, revision FROM workflow_package_artifact_heads
             WHERE scope_key IN (?1, 'library')
               AND NOT (scope_key = ?1 AND kind IN ('pipeline', 'role'))
           UNION
           SELECT ?1, 'role', id, head_revision FROM conversation_templates WHERE project_id = ?2
           UNION
           SELECT ?1, 'pipeline', workflow_id, revision FROM workflow_published_heads WHERE project_id = ?2
           UNION
           SELECT ?1, 'pipeline', entry.workflow_id, entry.revision FROM workflow_revisions entry
             WHERE entry.project_id = ?2 AND NOT EXISTS (
               SELECT 1 FROM workflow_published_heads head WHERE head.project_id = entry.project_id AND head.workflow_id = entry.workflow_id
             )
         )
         ORDER BY 1, 2, 3, 4 LIMIT ?3 OFFSET ?4",
    )?;
    let rows = query.query_map(
        params![
            scope,
            project,
            i64::try_from(PAGE_SIZE + 1)?,
            i64::try_from(offset)?
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    let mut items = Vec::new();
    for row in rows {
        let (scope, kind, id, revision) = row?;
        let version = PackageArtifactVersion {
            destination: if scope == "library" {
                PackageDestination::Library
            } else {
                PackageDestination::Project
            },
            kind: serde_json::from_value(Value::String(kind))?,
            id,
            revision,
        };
        let artifact = publish::load_published(db, project, &version)?
            .context("A catalog revision is unavailable")?;
        let name = artifact.name.chars().take(150).collect::<String>();
        items.push(
            json!({"version": version, "name": name, "hasBlockers": !artifact.blockers.is_empty()}),
        );
    }
    let next = (items.len() > PAGE_SIZE).then_some(offset + PAGE_SIZE);
    items.truncate(PAGE_SIZE);
    Ok(json!({"items": items, "nextOffset": next}))
}

fn chunk(value: &impl Serialize, offset: usize) -> Result<Value> {
    let text = serde_json::to_string(value)?;
    ensure!(
        offset <= text.len() && text.is_char_boundary(offset),
        "Select an offset returned by the catalog"
    );
    let mut end = text.len().min(offset.saturating_add(CHUNK_BYTES));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(json!({"json": &text[offset..end], "nextOffset": (end < text.len()).then_some(end)}))
}
