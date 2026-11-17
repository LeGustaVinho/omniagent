use std::{path::PathBuf, time::Duration};

use tauri::{
    App, AppHandle, Manager, State, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;

use crate::fixture_runtime::FixtureRuns;
use omniagent::{
    diagnostics,
    jobs::{JobNoticeKind, JobNotices, JobScheduler},
    repository::{self, RepositoryMarkdown, RepositoryMarkdownRevision},
    store::Store,
};

pub fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn create_tray(app: &App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let exit_item = MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &exit_item])?;
    let mut tray = TrayIconBuilder::with_id("omniagent")
        .tooltip("OmniAgent")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show(app),
            "exit" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let scheduler = app.state::<JobScheduler>();
                    if scheduler.stop_admission().await {
                        let fixtures = app.state::<FixtureRuns>();
                        let (fixtures, jobs) =
                            tokio::join!(fixtures.shutdown(), scheduler.shutdown());
                        app.exit(i32::from(fixtures.is_err() || jobs.is_err()));
                    }
                });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

pub fn notify_job_activity(
    app: AppHandle,
    mut notices: JobNotices,
) -> tauri_plugin_notification::Result<()> {
    let on_click = app.clone();
    app.notification().on_action(move |action| {
        if action.action_id() == "tap" {
            let window = on_click.clone();
            let _ = on_click.run_on_main_thread(move || show(&window));
        }
    })?;
    tauri::async_runtime::spawn(async move {
        loop {
            let result = async {
                let notice = notices.next().await?;
                let body = match notice.kind {
                    JobNoticeKind::JobApproval => "A queued job needs your approval.",
                    JobNoticeKind::Permission => "A running job needs a permission decision.",
                    JobNoticeKind::Question => "A job is waiting for your answer.",
                    JobNoticeKind::Succeeded => "A job completed.",
                    JobNoticeKind::Failed => "A job failed.",
                    JobNoticeKind::Interrupted => "An interrupted job needs review.",
                };
                app.notification()
                    .builder()
                    .title("OmniAgent")
                    .body(body)
                    .show()?;
                notices.acknowledge(&notice).await
            }
            .await;
            if let Err(error) = result {
                eprintln!(
                    "Job notification: {}",
                    diagnostics::preview(&error.to_string())
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    });
    Ok(())
}

pub fn hide_main_on_close(window: &tauri::Window, event: &WindowEvent) {
    if window.label() == "main"
        && let WindowEvent::CloseRequested { api, .. } = event
    {
        api.prevent_close();
        let _ = window.hide();
    }
}

#[tauri::command]
pub async fn choose_repository(app: AppHandle) -> Result<Option<PathBuf>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut builder = app.dialog().file().set_title("Open a Git repository");
        if let Some(window) = app.get_webview_window("main") {
            builder = builder.set_parent(&window);
        }
        builder
            .blocking_pick_folder()
            .map(|path| path.into_path())
            .transpose()
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn choose_repository_markdown(
    app: AppHandle,
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Option<RepositoryMarkdown>, String> {
    let root = repository::checkout_root(&store, checkout_id.clone())
        .await
        .map_err(markdown_error)?;
    let path = tauri::async_runtime::spawn_blocking(move || {
        let mut picker = app
            .dialog()
            .file()
            .set_title("Select repository Markdown")
            .set_directory(root)
            .add_filter("Markdown", &["md", "markdown"]);
        if let Some(window) = app.get_webview_window("main") {
            picker = picker.set_parent(&window);
        }
        picker
            .blocking_pick_file()
            .map(|path| path.into_path())
            .transpose()
    })
    .await
    .map_err(markdown_error)?
    .map_err(markdown_error)?;
    let Some(path) = path else {
        return Ok(None);
    };
    store
        .capture_repository_markdown(checkout_id, path)
        .await
        .map(Some)
        .map_err(markdown_error)
}

#[tauri::command]
pub async fn list_repository_markdown_revisions(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Vec<RepositoryMarkdownRevision>, String> {
    store
        .repository_markdown_revisions(checkout_id)
        .await
        .map_err(markdown_error)
}

#[tauri::command]
pub async fn read_repository_markdown(
    store: State<'_, Store>,
    checkout_id: String,
    revision_id: String,
) -> Result<Option<RepositoryMarkdown>, String> {
    store
        .repository_markdown(checkout_id, revision_id)
        .await
        .map_err(markdown_error)
}

fn markdown_error(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn repository_note_catalog(
    store: State<'_, Store>,
    checkout_id: String,
) -> Result<Vec<omniagent::repository::RepositoryNoteCatalogEntry>, String> {
    store
        .repository_note_catalog(checkout_id)
        .await
        .map_err(markdown_error)
}
