use super::*;

#[derive(Clone, Copy)]
pub(super) enum ActionTable {
    Claude,
    Native,
}

pub(super) enum Claim<T> {
    Ready(Box<T>),
    Recorded(ClaudeUserPluginOutcome),
}

impl ActionTable {
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude_user_plugin_actions",
            Self::Native => "native_user_plugin_actions",
        }
    }

    pub(super) async fn save(
        self,
        store: &Store,
        id: &str,
        inventory_id: &str,
        sealed: &impl Serialize,
    ) -> Result<()> {
        let id = id.to_owned();
        let inventory_id = inventory_id.to_owned();
        let sealed = protected(sealed)?;
        store.execute(move |db| {
            db.execute(
                &format!("INSERT INTO {}(id, state, before_inventory_id, preview) VALUES (?1, 'ready', ?2, ?3)", self.name()),
                params![id, inventory_id, sealed],
            )?;
            Ok(())
        }).await
    }

    pub(super) async fn claim<T: DeserializeOwned + Send + 'static>(
        self,
        store: &Store,
        id: &str,
    ) -> Result<Claim<T>> {
        let id = id.to_owned();
        store.execute(move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let (preview, outcome): (Vec<u8>, Option<Vec<u8>>) = tx.query_row(
                &format!("SELECT preview, outcome FROM {} WHERE id = ?1", self.name()),
                [&id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?.context("The reviewed plugin change is unavailable")?;
            if let Some(previous) = outcome {
                return Ok(Claim::Recorded(decode(&previous)?));
            }
            let sealed = decode(&preview)?;
            let prepared = ClaudeUserPluginOutcome::Unconfirmed {
                reason: "This operation may have been interrupted. Refresh the CLI inventory before reviewing a new operation.".into(),
                inventory_after: None,
            };
            ensure!(tx.execute(
                &format!("UPDATE {} SET state = 'applying', outcome = ?2 WHERE id = ?1 AND state = 'ready'", self.name()),
                params![id, protected(&prepared)?],
            )? == 1, "The plugin preview was already used");
            tx.commit()?;
            Ok(Claim::Ready(Box::new(sealed)))
        }).await
    }

    pub(super) async fn settle(
        self,
        store: &Store,
        id: String,
        outcome: &ClaudeUserPluginOutcome,
    ) -> Result<()> {
        let inventory = outcome.inventory_after().map(|item| item.id.clone());
        let outcome = protected(outcome)?;
        store
            .execute(move |db| {
                ensure!(
                    db.execute(
                        &format!(
                            "UPDATE {} SET state = 'settled', outcome = ?2, after_inventory_id = ?3
                          WHERE id = ?1 AND state = 'applying'",
                            self.name()
                        ),
                        params![id, outcome, inventory],
                    )? == 1,
                    "The plugin change result could not be settled"
                );
                Ok(())
            })
            .await
    }
}
