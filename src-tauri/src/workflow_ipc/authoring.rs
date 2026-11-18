use std::collections::BTreeMap;

use anyhow::Result;
use omniagent::workflows::authoring::package::{
    PackageArtifactVersion, PromptCard, PromptSuggestionStatus, PublishedPackageArtifact,
    WorkflowAuthoringOutput, WorkflowPackage, WorkflowPackageCatalogEntry, WorkflowPackageDraft,
    WorkflowPackagePreview, WorkflowPackagePublishOutcome, WorkflowPackageSummary,
    WorkflowSimulation, WorkflowSimulationScenario,
    derive_reusable::{ReusableDerivation, ReusableSource, SubflowBoundary, WorkflowReusableNode},
    exchange::{ExchangeBinding, WorkflowExchange, WorkflowExchangePreview},
};
use omniagent::{
    model_catalog::ModelSelection,
    providers::{Provider, SessionMode},
    store::Store,
    workflows::{
        WorkflowGraph, WorkflowRevision,
        authoring::{
            AuthoringBase, AuthoringDraft, WorkflowCanvasEdit, WorkflowPublicationOutcome,
        },
    },
};
use tauri::State;

use super::report;
use crate::conversation_runtime::selected_scope;

#[tauri::command]
pub async fn describe_workflow_reusable_boundary(
    store: State<'_, Store>,
    project_id: String,
    source: ReusableSource,
    node_ids: Option<Vec<String>>,
) -> Result<SubflowBoundary, String> {
    store
        .describe_workflow_reusable_boundary(project_id, source, node_ids)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn derive_workflow_reusable(
    store: State<'_, Store>,
    project_id: String,
    source: ReusableSource,
    request: ReusableDerivation,
) -> Result<WorkflowPackageDraft, String> {
    store
        .derive_workflow_reusable(project_id, source, request)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn describe_workflow_reusable_node(
    store: State<'_, Store>,
    project_id: String,
    version: PackageArtifactVersion,
) -> Result<WorkflowReusableNode, String> {
    store
        .describe_workflow_reusable_node(project_id, version)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn export_workflow_exchange(
    store: State<'_, Store>,
    project_id: String,
    selected: Vec<PackageArtifactVersion>,
) -> Result<WorkflowExchange, String> {
    store
        .export_workflow_exchange(project_id, selected)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn preview_workflow_exchange(
    store: State<'_, Store>,
    project_id: String,
    document: String,
    bindings: BTreeMap<String, ExchangeBinding>,
) -> Result<WorkflowExchangePreview, String> {
    store
        .preview_workflow_exchange(project_id, document, bindings)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn import_reviewed_workflow_exchange(
    store: State<'_, Store>,
    project_id: String,
    document: String,
    bindings: BTreeMap<String, ExchangeBinding>,
    reviewed_digest: String,
) -> Result<WorkflowPackageDraft, String> {
    store
        .import_reviewed_workflow_exchange(project_id, document, bindings, reviewed_digest)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn list_workflow_packages(
    store: State<'_, Store>,
    project_id: String,
    conversation_id: Option<String>,
) -> Result<Vec<WorkflowPackageSummary>, String> {
    store
        .workflow_packages(project_id, conversation_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn workflow_package_catalog(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<WorkflowPackageCatalogEntry>, String> {
    store
        .workflow_package_catalog(project_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn read_published_workflow_package_artifact(
    store: State<'_, Store>,
    project_id: String,
    version: PackageArtifactVersion,
) -> Result<Option<PublishedPackageArtifact>, String> {
    store
        .published_workflow_package_artifact(project_id, version)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn inspect_workflow_package(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
) -> Result<WorkflowPackageDraft, String> {
    store
        .workflow_package(project_id, draft_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn preview_workflow_package_revision(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
    revision: String,
) -> Result<WorkflowPackagePreview, String> {
    store
        .preview_workflow_package_revision(project_id, draft_id, revision)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn simulate_workflow_package_revision(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
    revision: String,
    member_key: String,
    scenario: WorkflowSimulationScenario,
) -> Result<WorkflowSimulation, String> {
    store
        .simulate_workflow_package_revision(project_id, draft_id, revision, member_key, scenario)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn create_workflow_package(
    store: State<'_, Store>,
    project_id: String,
    conversation_id: Option<String>,
    package: WorkflowPackage,
) -> Result<WorkflowPackageDraft, String> {
    store
        .create_workflow_package(project_id, conversation_id, package)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_workflow_package(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
    expected_revision: String,
    package: WorkflowPackage,
    canvas_by_member: Option<BTreeMap<String, WorkflowCanvasEdit>>,
) -> Result<WorkflowPackageDraft, String> {
    store
        .save_workflow_package_canvas(
            project_id,
            draft_id,
            expected_revision,
            package,
            canvas_by_member.unwrap_or_default(),
        )
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn request_prompt_contract_suggestion(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    prompt: PromptCard,
) -> Result<String, String> {
    async {
        let scope = selected_scope(&store, checkout_id.clone(), selection, effort, SessionMode::ReadOnly).await?;
        store.request_prompt_contract_suggestion(project_id, checkout_id, scope, prompt).await
    }.await.map_err(report)
}

#[tauri::command]
pub async fn prompt_contract_suggestion(
    store: State<'_, Store>,
    project_id: String,
    job_id: String,
) -> Result<PromptSuggestionStatus, String> {
    store.prompt_contract_suggestion(project_id, job_id).await.map_err(report)
}

#[tauri::command]
pub async fn request_workflow_package_changes(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    draft_id: String,
    expected_revision: String,
    selection: ModelSelection,
    effort: Option<String>,
    request: String,
) -> Result<WorkflowPackageDraft, String> {
    async {
        let scope = selected_scope(
            &store,
            checkout_id.clone(),
            selection,
            effort,
            SessionMode::ReadOnly,
        )
        .await?;
        store
            .request_workflow_package_changes(
                project_id,
                draft_id,
                expected_revision,
                checkout_id,
                scope,
                request,
            )
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn start_workflow_package_authoring(
    store: State<'_, Store>,
    project_id: String,
    conversation_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    base: Option<WorkflowRevision>,
    brief: String,
) -> Result<WorkflowPackageDraft, String> {
    async {
        let checkout = store
            .conversation(conversation_id.clone())
            .await?
            .checkout_id;
        let scope = selected_scope(
            &store,
            checkout.clone(),
            selection,
            effort,
            SessionMode::ReadOnly,
        )
        .await?;
        store
            .start_workflow_package_authoring(
                project_id,
                conversation_id,
                checkout,
                scope,
                base,
                brief,
            )
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn start_workflow_editor_authoring(
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    base: Option<PackageArtifactVersion>,
    brief: String,
    output: Option<WorkflowAuthoringOutput>,
) -> Result<WorkflowPackageDraft, String> {
    async {
        anyhow::ensure!(
            matches!(selection.provider, Provider::Codex | Provider::Claude),
            "Choose a Codex or Claude authoring profile"
        );
        let scope = selected_scope(
            &store,
            checkout_id.clone(),
            selection,
            effort,
            SessionMode::ReadOnly,
        )
        .await?;
        store
            .start_workflow_editor_authoring(project_id, checkout_id, scope, base, brief, output)
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn publish_reviewed_workflow_package(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
    reviewed_revision: String,
) -> Result<WorkflowPackagePublishOutcome, String> {
    store
        .publish_reviewed_workflow_package(project_id, draft_id, reviewed_revision)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn publish_reviewed_workflow(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
    reviewed_revision_id: String,
    expected_base: Option<WorkflowRevision>,
) -> Result<WorkflowPublicationOutcome, String> {
    store
        .publish_reviewed_workflow(project_id, draft_id, reviewed_revision_id, expected_base)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn workflow_authoring_bases(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<AuthoringBase>, String> {
    store
        .workflow_authoring_bases(project_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn conversation_workflow_authoring(
    store: State<'_, Store>,
    conversation_id: String,
) -> Result<Vec<AuthoringDraft>, String> {
    store
        .conversation_workflow_authoring(conversation_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn start_workflow_authoring(
    store: State<'_, Store>,
    conversation_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    base: Option<WorkflowRevision>,
    brief: String,
) -> Result<AuthoringDraft, String> {
    async {
        let chat = store.conversation(conversation_id.clone()).await?;
        let scope = selected_scope(
            &store,
            chat.checkout_id.clone(),
            selection,
            effort,
            SessionMode::ReadOnly,
        )
        .await?;
        store
            .start_workflow_authoring(conversation_id, chat.checkout_id, scope, base, brief)
            .await
    }
    .await
    .map_err(report)
}

#[tauri::command]
pub async fn inspect_workflow_authoring(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
) -> Result<AuthoringDraft, String> {
    store
        .inspect_workflow_authoring(project_id, draft_id)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn save_workflow_authoring_draft(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
    selected_revision: String,
    graph: WorkflowGraph,
    canvas: Option<WorkflowCanvasEdit>,
) -> Result<AuthoringDraft, String> {
    store
        .save_workflow_authoring_canvas(project_id, draft_id, selected_revision, graph, canvas)
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn request_workflow_authoring_changes(
    store: State<'_, Store>,
    project_id: String,
    draft_id: String,
    selected_revision: String,
    feedback: String,
    selection: ModelSelection,
    effort: Option<String>,
) -> Result<AuthoringDraft, String> {
    async {
        let checkout = store
            .workflow_authoring_checkout(project_id.clone(), draft_id.clone())
            .await?;
        let scope =
            selected_scope(&store, checkout, selection, effort, SessionMode::ReadOnly).await?;
        store
            .request_workflow_authoring_changes(
                project_id,
                draft_id,
                selected_revision,
                scope,
                feedback,
            )
            .await
    }
    .await
    .map_err(report)
}
