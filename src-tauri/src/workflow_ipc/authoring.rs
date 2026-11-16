use anyhow::Result;
use omniagent::workflows::authoring::package::{
    PackageArtifactVersion, PublishedPackageArtifact, WorkflowPackage, WorkflowPackageCatalogEntry,
    WorkflowPackageDraft, WorkflowPackagePublishOutcome, WorkflowPackageSummary,
};
use omniagent::{
    model_catalog::ModelSelection,
    providers::SessionMode,
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
) -> Result<WorkflowPackageDraft, String> {
    store
        .save_workflow_package(project_id, draft_id, expected_revision, package)
        .await
        .map_err(report)
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
