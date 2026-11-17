use omniagent::{
    diagnostics,
    repository::git_review::{
        GitBranchComparison, GitBranchDiff, GitBranchRange, GitBranchRevisions, GitCheckoutReview,
        GitDiffSource, GitFileHistory, GitFileVersion, GitFileVersionComparison, GitPermalinkLines,
        GitReviewCommit, GitReviewDiff, GitReviewRevision, SavedGitAttempt,
    },
    repository::{ManualGitWorktree, ProjectCheckout, create_git_worktree as create_worktree},
    store::Store,
};
use tauri::{AppHandle, Manager, State};

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
pub async fn git_checkout_file_history(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
    branch: Option<String>,
    pinned_head: Option<String>,
    offset: u32,
) -> Result<GitFileHistory, String> {
    store
        .git_checkout_file_history(checkout_id, path, branch, pinned_head, offset)
        .await
        .map_err(review_error)
}

#[tauri::command]
pub async fn git_compare_file_versions(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
    base: Option<GitFileVersion>,
    revised: GitFileVersion,
) -> Result<GitFileVersionComparison, String> {
    store
        .git_compare_file_versions(checkout_id, path, base, revised)
        .await
        .map_err(review_error)
}

#[tauri::command]
pub async fn git_file_permalink(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
    sha: Option<String>,
    lines: Option<GitPermalinkLines>,
) -> Result<String, String> {
    store
        .git_file_permalink(checkout_id, path, sha, lines)
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
pub async fn git_branch_comparison(
    store: State<'_, Store>,
    checkout_id: String,
    range: GitBranchRange,
) -> Result<GitBranchComparison, String> {
    store
        .git_branch_comparison(checkout_id, range)
        .await
        .map_err(review_error)
}

#[tauri::command]
pub async fn git_branch_comparison_diff(
    store: State<'_, Store>,
    checkout_id: String,
    range: GitBranchRange,
    expected: GitBranchRevisions,
    path: String,
) -> Result<GitBranchDiff, String> {
    store
        .git_branch_comparison_diff(checkout_id, range, expected, path)
        .await
        .map_err(review_error)
}

#[tauri::command]
pub async fn create_git_worktree(
    app: AppHandle,
    store: State<'_, Store>,
    project_id: String,
    checkout_id: String,
    request: ManualGitWorktree,
) -> Result<ProjectCheckout, String> {
    let default_root = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?
        .join("worktrees");
    create_worktree(&store, default_root, project_id, checkout_id, request)
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
pub async fn set_git_hunk_staged(
    store: State<'_, Store>,
    checkout_id: String,
    path: String,
    source: GitDiffSource,
    expected: GitReviewRevision,
    hunk_id: String,
) -> Result<(), String> {
    store
        .set_git_hunk_staged(checkout_id, path, source, expected, hunk_id)
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
