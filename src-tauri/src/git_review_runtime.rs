use omniagent::{
    diagnostics,
    repository::git_review::{
        GitCheckoutReview, GitDiffSource, GitReviewCommit, GitReviewDiff, GitReviewRevision,
        SavedGitAttempt,
    },
    store::Store,
};
use tauri::State;

fn review_error(error: anyhow::Error) -> String {
    diagnostics::preview(&format!("{error:#}"))
}

#[tauri::command]
pub async fn git_checkout_review(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<GitCheckoutReview, String> {
    store
        .git_checkout_review(checkout_id)
        .await
        .map_err(review_error)
}

#[tauri::command]
pub async fn git_checkout_history(
    store: State<'_, Store>,
    checkout_id: String,
    branch: Option<String>,
) -> Result<Vec<GitReviewCommit>, String> {
    store
        .git_checkout_history(checkout_id, branch)
        .await
        .map_err(review_error)
}

#[tauri::command]
pub async fn git_checkout_diff(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
    source: GitDiffSource,
) -> Result<GitReviewDiff, String> {
    store
        .git_checkout_diff(checkout_id, path, source)
        .await
        .map_err(review_error)
}

#[tauri::command]
pub async fn set_git_path_staged(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
    staged: bool,
    expected: GitReviewRevision,
) -> Result<(), String> {
    store
        .set_git_path_staged(checkout_id, path, staged, expected)
        .await
        .map_err(review_error)
}

#[tauri::command]
pub async fn commit_git_index(
    store: State<'_, Store>,
    checkout_id: String,
    message: String,
    expected: GitReviewRevision,
) -> Result<String, String> {
    store
        .commit_git_index(checkout_id, message, expected)
        .await
        .map_err(review_error)
}

#[tauri::command]
pub async fn git_saved_attempts(
    store: State<'_, Store>,
    project_id: String,
) -> Result<Vec<SavedGitAttempt>, String> {
    store
        .git_saved_attempts(project_id)
        .await
        .map_err(review_error)
}
