use super::*;

const READ: &str = "read_authoring_catalog";
const APPLY: &str = "apply_authoring_operations";
const SUBMIT: &str = "submit_authoring_for_review";

pub(crate) const AUTHORING_TOOLS: [&str; 3] = [READ, APPLY, SUBMIT];

pub(super) fn tools() -> [Tool; 3] {
    let text = json!({"type":"string","minLength":1,"maxLength":160});
    let offset = json!({"type":"integer","minimum":0});
    let operation = |kind: &str, required: &[&str], fields: Value| {
        let mut properties = fields.as_object().cloned().unwrap_or_default();
        properties.insert("operation".into(), json!({"const":kind}));
        json!({"type":"object", "additionalProperties":false,
            "required": (["operation"].into_iter().chain(required.iter().copied()).collect::<Vec<_>>()),
            "properties": properties
        })
    };
    let wire = json!({"type":"object", "required":["from","to"]});
    let operations = [
        operation("rename_package", &["name"], json!({"name":text})),
        operation(
            "put_member",
            &["member"],
            json!({"member":{"type":"object","required":["key","name","body"]}}),
        ),
        operation("remove_member", &["key"], json!({"key":text})),
        operation(
            "put_node",
            &["memberKey", "node"],
            json!({"memberKey":text,"node":{"type":"object","required":["id","kind","configuration"]}}),
        ),
        operation(
            "remove_node",
            &["memberKey", "nodeId"],
            json!({"memberKey":text,"nodeId":text}),
        ),
        operation(
            "put_wire",
            &["memberKey", "wire"],
            json!({"memberKey":text,"wire":wire}),
        ),
        operation(
            "remove_wire",
            &["memberKey", "wire"],
            json!({"memberKey":text,"wire":wire}),
        ),
        operation(
            "set_reference",
            &["memberKey", "slot", "reference"],
            json!({"memberKey":text,"slot":text,"reference":{"type":["object","null"]}}),
        ),
    ];
    let catalog = json!({"type":"object","additionalProperties":false,"required":["view"],"properties":{
        "view":{"enum":["draft","draft_node_definitions","node_definitions","published","draft_member","published_member"]},
        "offset":offset,"key":text,"kind":text,"expectedRevision":text,
        "version":{"type":"object","required":["destination","kind","id","revision"],
            "additionalProperties":false,"properties":{
              "destination":{"enum":["project","library"]},
              "kind":{"enum":["pipeline","role","preset","subflow","schema","command","mcp_tool"]},
              "id":text,"revision":text
            }
        }
    }});
    let apply = json!({"type":"object","additionalProperties":false,"required":["requestId","expectedRevision","operations"],"properties":{
        "requestId":text, "expectedRevision":text, "operations":{"type":"array","minItems":1,"maxItems":8,"items":{"oneOf":operations}}
    }});
    let submit = json!({"type":"object","additionalProperties":false,"required":["requestId","expectedRevision"],"properties":{
        "requestId":text, "expectedRevision":text
    }});
    [
        (READ, "Read this request's package, node definitions, and its project and local-library catalog. Use draft for the working revision, draft_node_definitions for pinned nodes, node_definitions with an optional kind for available nodes and ports, published for exact artifact versions, and draft_member or published_member for JSON chunks. Pass nextOffset to continue.", catalog, true),
        (APPLY, "Apply typed edits to this attempt's working revision. Use the last revision returned by a successful call and repeat the same requestId and content to retry. An edit affects the protected draft workspace.", apply, false),
        (SUBMIT, "Submit this working revision for human review. Use the last working revision and a new requestId. This records a reviewed proposal in the package editor.", submit, false),
    ].map(|(name, description, schema, read_only)| {
        Tool::new(name, description, schema.as_object().unwrap().clone())
            .with_annotations(ToolAnnotations::new().read_only(read_only).destructive(false).idempotent(true).open_world(false))
    })
}

pub(super) async fn call(
    ticket: &Ticket,
    request: CallToolRequestParams,
) -> Result<CallToolResponse, ErrorData> {
    if !ticket.package_authoring {
        return Err(ErrorData::invalid_params(
            "Authoring tools require an active package authoring request",
            None,
        ));
    }
    let args = Value::Object(request.arguments.unwrap_or_default());
    let invalid = |_| ErrorData::invalid_params("Invalid package authoring arguments", None);
    let operation = match request.name.as_ref() {
        READ => Operation::AuthoringCatalog(serde_json::from_value(args).map_err(invalid)?),
        APPLY => Operation::AuthoringApply(serde_json::from_value(args).map_err(invalid)?),
        _ => Operation::AuthoringSubmit(serde_json::from_value(args).map_err(invalid)?),
    };
    let result = match exchange(ticket, operation)
        .await
        .map_err(|_| bridge_unavailable())?
    {
        Response::AuthoringCatalog(status) => status,
        Response::AuthoringOutcome(outcome) => json!(outcome),
        _ => return Err(bridge_unavailable()),
    };
    Ok(CallToolResult::structured(result).into())
}
