use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use ts_rs::TS;

use crate::{protection, store::Store};

#[derive(Debug, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub struct FixtureInputEvidence {
    pub text: String,
    pub sent: bool,
}

impl Store {
    pub async fn prepare_fixture_input(&self, run_id: String, input: String) -> Result<()> {
        ensure!(input.len() <= 16384, "fixture input exceeds 16 KiB");
        let encrypted = protection::protect(input.as_bytes())?;
        self.execute(move |db| {
            let changed = db.execute(
                "INSERT INTO fixture_inputs(turn_id, payload, delivery)
                 SELECT t.id, ?2, 'prepared' FROM turns t JOIN runs r ON r.id = t.run_id
                 WHERE r.id = ?1 AND r.status = 'waiting_for_input' ORDER BY t.ordinal DESC LIMIT 1",
                params![run_id, encrypted],
            )?;
            ensure!(changed == 1, "fixture is not waiting for input");
            Ok(())
        }).await
    }

    pub async fn mark_fixture_input_sent(&self, run_id: String) -> Result<()> {
        self.execute(move |db| {
            let changed = db.execute(
                "UPDATE fixture_inputs SET delivery = 'sent' WHERE delivery = 'prepared' AND turn_id = (
                     SELECT id FROM turns WHERE run_id = ?1 ORDER BY ordinal DESC LIMIT 1
                 )",
                [&run_id],
            )?;
            ensure!(changed == 1, "fixture input was not prepared");
            Ok(())
        }).await
    }

    pub async fn fixture_input(
        &self,
        checkout_id: String,
        run_id: String,
    ) -> Result<Option<FixtureInputEvidence>> {
        self.execute(move |db| {
            let row: Option<(Vec<u8>, bool)> = db.query_row(
                "SELECT i.payload, i.delivery = 'sent'
                 FROM fixture_inputs i JOIN turns t ON i.turn_id = t.id JOIN runs r ON r.id = t.run_id
                 WHERE r.id = ?1 AND r.checkout_id = ?2 ORDER BY t.ordinal DESC LIMIT 1",
                params![run_id, checkout_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            row.map(|(encrypted, sent)| {
                let text = String::from_utf8(protection::unprotect(&encrypted)?)?;
                Ok(FixtureInputEvidence { text, sent })
            }).transpose()
        }).await
    }
}
