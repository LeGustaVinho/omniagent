use crate::{jobs::JobRole, providers::Provider};

use super::*;

fn template(name: &str, model_id: &str) -> ConversationTemplate {
    ConversationTemplate {
        name: name.into(),
        role: JobRole::Pipeline,
        model: ModelSelection {
            provider: Provider::Codex,
            profile_id: "fixture".into(),
            model_id: model_id.into(),
        },
        effort: Some("medium".into()),
        instructions: format!("Read only with {name}."),
        tool_scope: SessionMode::ReadOnly,
        completion_contract: format!("Return evidence from {name}."),
        review_policy: TemplateReviewPolicy::Independent,
        requirements: Default::default(),
    }
}

#[test]
fn a_bound_role_never_grants_write_or_waives_its_review_requirement() -> Result<()> {
    let role = template("Read-only role", "pinned-model");
    let mut scope = SessionScope {
        provider: role.model.provider,
        profile_id: role.model.profile_id.clone(),
        account_binding: None,
        executable: "codex.exe".into(),
        checkout: Default::default(),
        cli_version: "fixture".into(),
        runner: "fixture".into(),
        permission_revision: "fixture".into(),
        mode: SessionMode::ReadOnly,
        model: Some(role.model.model_id.clone()),
        effort: role.effort.clone(),
    };
    qualify_bound_role(&role, None, &scope)?;
    scope.mode = SessionMode::ApprovedWrite;
    let error = qualify_bound_role(&role, None, &scope).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not permit repository writes")
    );

    scope.mode = SessionMode::ReadOnly;
    for policy in [
        TemplateReviewPolicy::CrossReview,
        TemplateReviewPolicy::HumanApproval,
    ] {
        let role = ConversationTemplate {
            review_policy: policy,
            ..role.clone()
        };
        let error = qualify_bound_role(&role, None, &scope).unwrap_err();
        assert!(error.to_string().contains("does not bind that"));
    }
    Ok(())
}
