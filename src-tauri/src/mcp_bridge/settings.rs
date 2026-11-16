use super::*;

const READ: &str = "read_editable_settings";
const PROPOSE: &str = "propose_settings_patch";
pub(crate) const SETTINGS_TOOLS: [&str; 2] = [READ, PROPOSE];

pub(super) fn tools() -> [Tool; 2] {
    let read = json!({"type":"object","additionalProperties":false,"properties":{
        "templateOffset":{"type":"integer","minimum":0,"maximum":16384}
    }});
    let id = json!({"type":"string","minLength":1,"maxLength":160});
    let revision = json!({"type":"integer","minimum":0});
    let propose = json!({"type":"object","additionalProperties":false,
    "required":["requestId","changes"],"properties":{
        "requestId":id,
        "changes":{"type":"array","minItems":1,"maxItems":8,"items":{"oneOf":[
            {"type":"object","additionalProperties":false,
             "required":["field","expectedRevision","minutes"],"properties":{
                "field":{"const":"live_question_timeout"},"expectedRevision":revision,
                "minutes":{"type":"integer","minimum":1,"maximum":120}
             }},
            {"type":"object","additionalProperties":false,
             "required":["field","templateId","templateRevision","expectedRevision","label"],"properties":{
                "field":{"const":"template_display_label"},"templateId":id,"templateRevision":id,
                "expectedRevision":revision,"label":{"type":["string","null"],"minLength":1,"maxLength":120}
             }}
        ]}}
    }});
    [
        (READ, "Read the live question timeout and eligible template display labels in this project's scope. Use the returned revisions when proposing changes. Pass nextTemplateOffset for more templates.", read, true),
        (PROPOSE, "Propose typed changes to the project's question timeout or a template's display label. Changes outside the current user allowlist await confirmation as a whole. Reuse requestId to retry identical content.", propose, false),
    ].map(|(name, description, schema, read_only)| {
        Tool::new(name, description, schema.as_object().unwrap().clone())
            .with_annotations(ToolAnnotations::new().read_only(read_only).destructive(false).idempotent(true).open_world(false))
    })
}

pub(super) async fn call(
    ticket: &Ticket,
    request: CallToolRequestParams,
) -> Result<CallToolResponse, ErrorData> {
    if ticket.package_authoring {
        return Err(ErrorData::invalid_params(
            "This authoring ticket is limited to draft tools",
            None,
        ));
    }
    let args = Value::Object(request.arguments.unwrap_or_default());
    let invalid = |_| ErrorData::invalid_params("Invalid editable settings arguments", None);
    let operation = if request.name == READ {
        Operation::ReadEditableSettings(serde_json::from_value(args).map_err(invalid)?)
    } else {
        Operation::ProposeSettingsPatch(serde_json::from_value(args).map_err(invalid)?)
    };
    let result = match exchange(ticket, operation)
        .await
        .map_err(|_| bridge_unavailable())?
    {
        Response::EditableSettings(catalog) => json!(catalog),
        Response::SettingsProposal(proposal) => json!(proposal),
        Response::SettingsRejected { reason } => {
            return Err(ErrorData::invalid_params(reason, None));
        }
        _ => return Err(bridge_unavailable()),
    };
    Ok(CallToolResult::structured(result).into())
}

pub(super) fn rejected(error: anyhow::Error) -> Response {
    let reason = crate::diagnostics::preview(&error.to_string())
        .chars()
        .take(240)
        .collect();
    Response::SettingsRejected { reason }
}
