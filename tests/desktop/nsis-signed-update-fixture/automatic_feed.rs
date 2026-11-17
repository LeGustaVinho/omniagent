use std::io::{ErrorKind, Read};

use tauri::Manager;

use super::{AppHandle, Result, Runtime, ensure, open_locked_local_file};

pub(super) fn read_fixture_feed<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<Option<serde_json::Value>> {
    let identifier = env!("OMNIAGENT_SIGNED_FIXTURE_ID");
    let root = app.path().app_local_data_dir()?;
    ensure!(
        app.config().identifier == identifier
            && root.file_name().is_some_and(|name| name == identifier),
        "Wrong software profile for the offline feed fixture"
    );
    let file = match open_locked_local_file(&root.join("fixture-raw-feed.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    const MAX_BYTES: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "Fixture feed exceeds its size limit"
    );
    Ok(Some(serde_json::from_slice(&bytes)?))
}
