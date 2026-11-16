use super::*;

pub(super) fn override_fields(configuration: &NodeConfiguration, fields: &[String]) -> Vec<String> {
    let possible: &[&str] = match configuration {
        NodeConfiguration::AgentRole { .. } => &["role"],
        NodeConfiguration::RequiredChecks { .. } => &["names"],
        NodeConfiguration::Condition { .. } => &["predicate"],
        NodeConfiguration::Command(_) => &["output"],
        NodeConfiguration::BuildList(crate::workflows::BuildListConfiguration::Values {
            ..
        }) => &["values"],
        NodeConfiguration::BuildList(crate::workflows::BuildListConfiguration::Evidence {
            ..
        }) => &["pointer"],
        _ => &[],
    };
    fields
        .iter()
        .filter(|field| possible.contains(&field.as_str()))
        .cloned()
        .collect()
}

pub(in crate::workflows::authoring::package) fn resolve_preset_configuration(
    instance: &NodeConfiguration,
    configuration: &NodeConfiguration,
    editable_fields: &[String],
    definition: &NodeDefinition,
) -> Result<NodeConfiguration> {
    let NodeConfiguration::PresetOverrides { fields } = instance else {
        return Ok(configuration.clone());
    };
    let editable = override_fields(configuration, editable_fields);
    let mut value = serde_json::to_value(configuration)?;
    let values = value
        .as_object_mut()
        .context("The preset has no typed configuration fields")?;
    for (key, value) in fields {
        ensure!(
            editable.contains(key) && values.contains_key(key),
            "Preset field {key} is fixed by the pinned revision"
        );
        values.insert(key.clone(), value.clone());
    }
    let resolved: NodeConfiguration = serde_json::from_value(value)
        .context("The preset overrides need to match the selected field types")?;
    ensure!(
        resolved.matches(definition.configuration),
        "The resolved preset configuration is invalid"
    );
    Ok(resolved)
}
