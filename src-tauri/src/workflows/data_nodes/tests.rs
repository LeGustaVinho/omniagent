use serde_json::json;

use super::*;

#[test]
fn set_fields_builds_nested_objects_from_literals_and_exact_input_paths() -> Result<()> {
    let configuration: SetFieldsConfiguration = serde_json::from_value(json!({
        "mode": "build",
        "operations": [
            {"op": "set", "path": "/profile/name", "value": {"source": "input", "pointer": "/items/0/name"}},
            {"op": "set", "path": "/profile/a~1b~0c", "value": {"source": "literal", "value": null}},
            {"op": "set", "path": "/missingIsDistinct",
             "value": {"source": "input", "pointer": "/optional"}}
        ]
    }))?;
    assert!(configuration.requires_source());
    let source = json!({"items": [{"name": "OmniAgent"}], "optional": null});
    assert_eq!(
        Value::Object(configuration.evaluate(Some(&source))?),
        json!({"profile": {"name": "OmniAgent", "a/b~c": null}, "missingIsDistinct": null})
    );
    let missing = json!({"items": source["items"]});
    assert!(
        configuration
            .evaluate(Some(&missing))
            .unwrap_err()
            .to_string()
            .contains("optional")
    );
    let literal_builder: SetFieldsConfiguration = serde_json::from_value(json!({
        "mode": "build",
        "operations": [{"op": "set", "path": "/ready", "value": {"source": "literal", "value": true}}]
    }))?;
    assert!(!literal_builder.requires_source());
    assert_eq!(
        Value::Object(literal_builder.evaluate(None)?),
        json!({"ready": true})
    );
    Ok(())
}

#[test]
fn set_fields_renames_the_current_object_but_reads_input_values_from_the_original() -> Result<()> {
    let configuration: SetFieldsConfiguration = serde_json::from_value(json!({
        "mode": "update",
        "operations": [
            {"op": "rename", "from": "/old", "path": "/renamed"},
            {"op": "set", "path": "/old", "value": {"source": "literal", "value": "new"}},
            {"op": "set", "path": "/original", "value": {"source": "input", "pointer": "/old"}}
        ]
    }))?;
    let source = json!({"old": ["existing"], "untouched": false});
    assert_eq!(
        Value::Object(configuration.evaluate(Some(&source))?),
        json!({"old": "new", "original": ["existing"], "renamed": ["existing"], "untouched": false})
    );
    assert_eq!(source["old"], json!(["existing"]));
    assert!(configuration.evaluate(Some(&json!(null))).is_err());
    Ok(())
}

#[test]
fn set_fields_rejects_invalid_or_overlapping_paths_and_scalar_parents() -> Result<()> {
    let setter = |path: &str| json!({"op": "set", "path": path, "value": {"source": "literal", "value": true}});
    for paths in [
        ["/a", "/a", "/x"],
        ["/a", "/a!", "/a/b"],
        ["/a", "/x", "/~2"],
    ] {
        let configuration: SetFieldsConfiguration = serde_json::from_value(json!({
            "mode": "build",
            "operations": paths.map(setter)
        }))?;
        assert!(configuration.validate().is_err());
    }
    let move_into_child: SetFieldsConfiguration = serde_json::from_value(json!({
        "mode": "update",
        "operations": [{"op": "rename", "from": "/a", "path": "/a/b"}]
    }))?;
    assert!(move_into_child.validate().is_err());
    let configuration: SetFieldsConfiguration = serde_json::from_value(json!({
        "mode": "update",
        "operations": [setter("/a/b")]
    }))?;
    for old in [json!({"a": null}), json!({"a": []}), json!({"a": true})] {
        assert!(
            configuration
                .evaluate(Some(&old))
                .unwrap_err()
                .to_string()
                .contains("non-object parent")
        );
    }
    Ok(())
}

#[test]
fn filter_map_list_uses_typed_values_preserves_order_and_records_the_input_list() -> Result<()> {
    let configuration: FilterMapListConfiguration = serde_json::from_value(json!({
        "predicate": {"op": "equals", "pointer": "/meta/weight", "value": 1},
        "mappings": [{"from": "/name", "path": "/display/title"}, {"from": "/meta/weight", "path": "/weight"}]
    }))?;
    let source = WorkflowJsonList::new(
        vec![
            json!({"name": "first", "meta": {"weight": 1}}),
            json!({"name": "text", "meta": {"weight": "1"}}),
            json!({"name": "missing", "meta": {}}),
            json!({"name": "last", "meta": {"weight": 1}}),
        ],
        Some("older-source".into()),
    )?;
    let mapped = configuration.evaluate(&source, "direct-source")?;
    assert_eq!(mapped.source_artifact_id.as_deref(), Some("direct-source"));
    assert_eq!(
        mapped.values,
        vec![
            json!({"display":{"title": "first"}, "weight": 1}),
            json!({"display":{"title": "last"}, "weight": 1})
        ]
    );
    assert!(
        configuration
            .evaluate(&WorkflowJsonList::new(vec![], None)?, "empty")?
            .values
            .is_empty()
    );
    Ok(())
}

#[test]
fn filter_map_list_distinguishes_null_from_absence_and_checks_selected_items() -> Result<()> {
    let source = WorkflowJsonList::new(
        vec![json!({}), json!({"x":null}), json!({"x": "value"})],
        None,
    )?;
    let null_only: FilterMapListConfiguration = serde_json::from_value(json!({
        "predicate": {"op": "equals", "pointer": "/x", "value": null}
    }))?;
    assert_eq!(
        null_only.evaluate(&source, "list")?.values,
        vec![json!({"x": null})]
    );
    let no_matches: FilterMapListConfiguration = serde_json::from_value(json!({
        "predicate": {"op": "exists", "pointer": "/missing"},
        "mappings": [{"from": "/missing", "path": "/x"}]
    }))?;
    assert!(no_matches.evaluate(&source, "list")?.values.is_empty());
    let select: FilterMapListConfiguration = serde_json::from_value(json!({
        "mappings": [{"from": "/x", "path": "/value"}]
    }))?;
    assert!(
        select
            .evaluate(&source, "list")
            .unwrap_err()
            .to_string()
            .contains(r#"List item 1 has no field "/x""#)
    );
    let service_owned: FilterMapListConfiguration = serde_json::from_value(json!({
        "predicate": {"op": "candidate_checks_passed"}
    }))?;
    assert!(service_owned.validate().is_err());
    Ok(())
}

#[test]
fn build_text_expands_literal_markdown_scalars_and_escaped_names() -> Result<()> {
    let configuration = BuildTextConfiguration {
        template: "# {{ title }}\n- {{enabled}} {{count}} {{empty}}\nLiteral \\{{unbound}}".into(),
    };
    assert!(configuration.requires_values());
    let fields = Map::from_iter([
        ("title".into(), json!("Review")),
        ("enabled".into(), json!(true)),
        ("count".into(), json!(4)),
        ("empty".into(), json!(null)),
    ]);
    assert_eq!(
        configuration.evaluate(Some(&fields))?,
        "# Review\n- true 4 \nLiteral {{unbound}}"
    );
    let literal = BuildTextConfiguration {
        template: r"Literal \{{value}}".into(),
    };
    assert!(!literal.requires_values());
    assert_eq!(literal.evaluate(None)?, "Literal {{value}}");
    Ok(())
}

#[test]
fn build_text_rejects_missing_or_structured_fields_and_oversize_rendering() -> Result<()> {
    for template in ["{{missing end", "{{}}", "{{some.value}}", "{{evaluate()}}"] {
        assert!(
            BuildTextConfiguration {
                template: template.into()
            }
            .validate()
            .is_err()
        );
    }
    let configuration = BuildTextConfiguration {
        template: "{{value}}{{value}}".into(),
    };
    assert!(
        configuration
            .evaluate(None)
            .unwrap_err()
            .to_string()
            .contains("missing")
    );
    for value in [json!([]), json!({})] {
        let fields = Map::from_iter([("value".into(), value)]);
        assert!(
            configuration
                .evaluate(Some(&fields))
                .unwrap_err()
                .to_string()
                .contains("scalar")
        );
    }
    let oversized = "x".repeat(MAX_DATA_BYTES / 2 + 1);
    let fields = Map::from_iter([("value".into(), json!(oversized))]);
    assert!(
        configuration
            .evaluate(Some(&fields))
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );
    Ok(())
}

#[test]
fn data_view_unwraps_plain_values_and_preserves_the_domain_evidence_envelope() {
    for (artifact, expected) in [
        (
            json!({"kind":"json_object","evidence":{"field":1}}),
            json!({"field":1}),
        ),
        (
            json!({"kind":"json_list","evidence":{"values":[1,2]}}),
            json!([1, 2]),
        ),
        (
            json!({"kind":"json_item","evidence":{"value":null}}),
            json!(null),
        ),
        (
            json!({"kind":"rendered_text","evidence":"# Text"}),
            json!("# Text"),
        ),
    ] {
        assert_eq!(data_view(&artifact), &expected);
    }
    let pinned = json!({"kind":"markdown","evidence":{"relativePath":"README.md"}});
    assert_eq!(data_view(&pinned), &pinned);
}
