use omniagent::{
    diagnostics,
    model_catalog::refresh::{CatalogRefresh, CatalogRefreshStatus},
    store::Store,
};
use tauri::{AppHandle, Emitter, Manager, State};

const UPDATED: &str = "model-catalog-updated";

#[tauri::command]
pub async fn model_catalog_status(
    store: State<'_, Store>,
    refresh: State<'_, CatalogRefresh>,
) -> Result<CatalogRefreshStatus, String> {
    let mut status = store.model_catalog_status().await.map_err(report)?;
    status.running = refresh.running();
    Ok(status)
}

#[tauri::command]
pub async fn refresh_model_catalog(app: AppHandle) -> Result<CatalogRefreshStatus, String> {
    // Let a refresh finish even if the initiating settings window closes.
    tauri::async_runtime::spawn(async move {
        let status = app
            .state::<CatalogRefresh>()
            .refresh(&app.state::<Store>())
            .await
            .map_err(report);
        let _ = app.emit(UPDATED, ());
        status
    })
    .await
    .map_err(|error| error.to_string())?
}

fn report(error: anyhow::Error) -> String {
    diagnostics::preview(&error.to_string())
}
