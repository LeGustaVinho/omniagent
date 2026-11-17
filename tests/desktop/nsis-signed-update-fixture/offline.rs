use super::*;

pub async fn read_signed_fixture_migration(store: &Store) -> Result<Option<String>> {
    store
        .execute(|db| {
            if !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'nsis_fixture_migration')",
                [],
                |row| row.get::<_, bool>(0),
            )? {
                return Ok(None);
            }
            Ok(Some(db.query_row(
                "SELECT state FROM nsis_fixture_migration",
                [],
                |row| row.get(0),
            )?))
        })
        .await
}

pub async fn dirty_signed_fixture_migration(store: &Store) -> Result<()> {
    store
        .execute(|db| {
            ensure!(
                db.execute(
                    "UPDATE nsis_fixture_migration SET state = 'failed-new-build'",
                    []
                )? == 1,
                "The fixture failed to write its rollback canary"
            );
            Ok(())
        })
        .await
}
