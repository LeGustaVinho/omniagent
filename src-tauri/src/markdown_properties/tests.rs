use super::*;

#[test]
fn discovers_exact_properties_and_compares_yaml_values_without_coercing_types() -> Result<()> {
    let markdown = concat!(
        "\u{feff}---\r\n",
        "'owner':   'Cíntia'  # comment\r\n",
        "status: ready\r\n",
        "'status': queued\r\n",
        "tasks: [one, 1]\r\n",
        "note: |-\r\n  preserve this\r\n",
        "other: &anchor ready\r\n",
        "reference: *anchor\r\n",
        "---\r\n",
        "owner: body text\r\n"
    );
    let rows = describe_markdown_properties(markdown)?;
    assert_eq!(
        rows.iter()
            .map(|row| (&*row.name, row.line))
            .collect::<Vec<_>>(),
        [
            ("owner", 2),
            ("status", 3),
            ("status", 4),
            ("tasks", 5),
            ("note", 6),
            ("other", 8),
            ("reference", 9)
        ]
    );
    let owner = &rows[0];
    assert_eq!(owner.value, "'Cíntia'");
    assert!(owner.editable && owner.filterable);
    for row in [&rows[1], &rows[2], &rows[4], &rows[6]] {
        assert!(!row.editable && !row.filterable);
    }
    assert_eq!(
        matching_markdown_property(
            markdown,
            "owner",
            &comparable_markdown_property("\"Cíntia\"")?
        ),
        Some((2, "'Cíntia'".into()))
    );
    assert_eq!(
        matching_markdown_property(
            markdown,
            "tasks",
            &comparable_markdown_property("['one', 1]")?
        ),
        Some((5, "[one, 1]".into()))
    );
    assert_eq!(
        matching_markdown_property(
            markdown,
            "tasks",
            &comparable_markdown_property("[one, '1']")?
        ),
        None
    );
    assert_eq!(
        matching_markdown_property(markdown, "status", &comparable_markdown_property("ready")?),
        None
    );
    assert_eq!(
        matching_markdown_property(
            markdown,
            "owner",
            &comparable_markdown_property("body text")?
        ),
        None
    );
    assert!(describe_markdown_properties("---\n# Comment\n---\n")?.is_empty());
    Ok(())
}

#[test]
fn edits_one_quoted_key_without_touching_markdown_or_plugin_data() -> Result<()> {
    let original = concat!(
        "\u{feff}--- \t\r\n",
        "# Header stays\r\n",
        "'owner':    \"Cíntia\"  # note stays\r\n",
        "unknown: {flow: ['# stays', true]}\n",
        "# plugin and property order stay\r\n",
        "kanban-plugin: board\r\n",
        "---\t\r\n\r\n",
        "- [/] Held by [owner:: Cíntia] 📅 2026-12-01\n",
        "```dataview\r\nLIST FROM #project\r\n```\r\n",
        "%% kanban:settings\n{\"collapse-tags\":true}\n%%",
    );
    let edited = replace_markdown_property(original, "owner", "\"Lúcia 😀 # same value\"")?;
    assert_eq!(
        edited,
        original.replacen("\"Cíntia\"", "\"Lúcia 😀 # same value\"", 1)
    );
    Ok(())
}

#[test]
fn preserves_anchors_sibling_properties_and_the_missing_final_line_break() -> Result<()> {
    let original = "---\n'labels': &saved [todo, 'keep: note']  # set\nrelated: *saved\nowner: {nested: old}\n---";
    let edited = replace_markdown_property(original, "labels", "[next, 'a,b']")?;
    assert_eq!(
        edited,
        original.replacen("[todo, 'keep: note']", "[next, 'a,b']", 1)
    );
    let flow = "---\n{owner: first, plugin: {type: keep, enabled: true}}\n---\n\nBody";
    let edited = replace_markdown_property(flow, "owner", "doe, jane")?;
    assert_eq!(edited, flow.replacen("first", "\"doe, jane\"", 1));
    Ok(())
}

#[test]
fn fills_and_clears_nullable_values_and_keeps_their_comments() -> Result<()> {
    let original = "---\r\nowner: # choose owner\r\nother: 123\r\n---\r\n";
    assert_eq!(replace_markdown_property(original, "owner", "")?, original);
    let populated = replace_markdown_property(original, "owner", "Ana")?;
    assert_eq!(populated, original.replacen("owner: #", "owner: Ana #", 1));
    let same = replace_markdown_property(&populated, "owner", "Ana")?;
    assert_eq!(same, populated);
    let empty = replace_markdown_property(&populated, "owner", "")?;
    assert_eq!(empty, populated.replacen("Ana", "", 1));

    let padded = "---\rowner:    Ana   \rother: keep\r---";
    assert_eq!(
        replace_markdown_property(padded, "owner", "")?,
        padded.replacen("Ana", "", 1)
    );
    Ok(())
}

#[test]
fn edits_block_values_when_their_comments_can_be_preserved() -> Result<()> {
    let original = "---\r\nowner: |- # owner comment\r\n  Old value\r\n# other comment\r\nunknown: last\r\n---";
    let edited = replace_markdown_property(original, "owner", "New value")?;
    assert_eq!(
        edited,
        "---\r\nowner: New value # owner comment\r\n# other comment\r\nunknown: last\r\n---"
    );

    let original = "---\nowner:\n  - Ana # important\nunknown: last\n---\n";
    assert!(
        replace_markdown_property(original, "owner", "Jose")
            .unwrap_err()
            .to_string()
            .contains("comments")
    );
    Ok(())
}

#[test]
fn rejects_ambiguous_missing_or_invalid_yaml_without_rematching_the_body() {
    let duplicate = "---\nowner: first\n'owner': second\n---\n";
    assert!(replace_markdown_property(duplicate, "owner", "third").is_err());
    let nested = "---\nproperties:\n  owner: value\n---\n";
    assert!(replace_markdown_property(nested, "owner", "third").is_err());
    let body = "# README\n\n---\nowner: visible example\n---\n";
    assert!(replace_markdown_property(body, "owner", "third").is_err());
    let source = "---\nowner: first\nunknown: keep\n---\n";
    assert!(replace_markdown_property(source, "owner", "changed\nunknown: rewritten").is_err());
    assert!(replace_markdown_property(source, "owner", "changed # injected comment").is_err());
    assert!(replace_markdown_property(source, "owner", "[malformed").is_err());
    let malformed = "---\nowner: good\nunknown: [malformed\n---\n";
    assert!(replace_markdown_property(malformed, "owner", "changed").is_err());
}
