use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{protection, providers::Provider, repository, store::Store};

mod scan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum CustomizationKind {
    Hook,
    McpServer,
    Plugin,
    SkillCommand,
    Helper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum ReviewState {
    Pending,
    Changed,
    Approved,
    Disabled,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct CustomizationEntry {
    pub id: String,
    pub provider: Provider,
    pub kind: CustomizationKind,
    pub source: String,
    pub source_revision: String,
    pub label: String,
    pub target: Option<String>,
    pub arguments: Vec<String>,
    pub requested_scope: String,
    pub local_files: Vec<ScannedFile>,
    pub fingerprint: String,
    pub review: ReviewState,
    pub issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ScannedFile {
    pub path: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct MarkdownInstruction {
    pub file: ScannedFile,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct CustomizationInventory {
    pub working_directory: String,
    pub instructions: Vec<MarkdownInstruction>,
    pub entries: Vec<CustomizationEntry>,
    pub unresolved_sources: Vec<String>,
}

pub(crate) fn inspect(root: &Path, cwd: &Path) -> Result<CustomizationInventory> {
    scan::inspect(root, cwd)
}

pub(crate) fn save(
    db: &Connection,
    checkout_id: &str,
    inventory: &CustomizationInventory,
) -> Result<()> {
    let payload = protection::protect(&serde_json::to_vec(inventory)?)?;
    db.execute(
        "INSERT INTO checkout_customizations(checkout_id, cwd, payload) VALUES (?1, ?2, ?3)
         ON CONFLICT(checkout_id) DO UPDATE SET cwd = excluded.cwd, payload = excluded.payload",
        params![checkout_id, inventory.working_directory, payload],
    )?;
    Ok(())
}

fn apply_approvals(
    db: &Connection,
    checkout_id: &str,
    inventory: &mut CustomizationInventory,
) -> Result<()> {
    let mut query = db.prepare(
        "SELECT EXISTS(SELECT 1 FROM customization_approvals a JOIN checkouts c
                      ON c.id = a.checkout_id AND c.project_id = a.project_id
                      WHERE a.checkout_id = ?1 AND a.entry_id = ?2 AND a.fingerprint = ?3),
                EXISTS(SELECT 1 FROM customization_approvals a JOIN checkouts c
                      ON c.id = a.checkout_id AND c.project_id = a.project_id
                      WHERE a.checkout_id = ?1 AND a.entry_id = ?2)",
    )?;
    for entry in &mut inventory.entries {
        if entry.review != ReviewState::Pending {
            continue;
        }
        let (exact, previous): (bool, bool) = query
            .query_row(params![checkout_id, entry.id, entry.fingerprint], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        entry.review = match (exact, previous) {
            (true, _) => ReviewState::Approved,
            (_, true) => ReviewState::Changed,
            _ => ReviewState::Pending,
        };
    }
    Ok(())
}

impl Store {
    pub async fn saved_checkout_customizations(
        &self,
        checkout_id: String,
    ) -> Result<CustomizationInventory> {
        self.execute(move |db| {
            let payload: Vec<u8> = db.query_row(
                "SELECT payload FROM checkout_customizations WHERE checkout_id = ?1",
                [&checkout_id],
                |row| row.get(0),
            )?;
            let mut inventory = serde_json::from_slice(&protection::unprotect(&payload)?)?;
            apply_approvals(db, &checkout_id, &mut inventory)?;
            Ok(inventory)
        })
        .await
    }

    async fn inspect_registered_checkout(
        &self,
        checkout_id: &str,
    ) -> Result<CustomizationInventory> {
        let id = checkout_id.to_owned();
        let cwd: String = self.execute(move |db| {
            Ok(db.query_row(
                "SELECT coalesce(i.cwd, c.root) FROM checkouts c LEFT JOIN checkout_customizations i ON c.id = i.checkout_id
                 WHERE c.id = ?1", [&id], |row| row.get(0),
            )?)
        }).await?;
        let checkout =
            repository::verify_checkout(self, checkout_id.to_owned(), PathBuf::from(cwd)).await?;
        tokio::task::spawn_blocking(move || inspect(&checkout.root, &checkout.cwd)).await?
    }

    pub async fn refresh_checkout_customizations(
        &self,
        checkout_id: String,
    ) -> Result<CustomizationInventory> {
        let mut inventory = self.inspect_registered_checkout(&checkout_id).await?;
        self.execute(move |db| {
            save(db, &checkout_id, &inventory)?;
            apply_approvals(db, &checkout_id, &mut inventory)?;
            Ok(inventory)
        })
        .await
    }

    pub async fn approve_checkout_customization(
        &self,
        checkout_id: String,
        entry_id: String,
        fingerprint: String,
    ) -> Result<CustomizationInventory> {
        let mut inventory = self.inspect_registered_checkout(&checkout_id).await?;
        let entry = inventory
            .entries
            .iter()
            .find(|entry| entry.id == entry_id)
            .context("Customization disappeared. Refresh the inventory before approving.")?;
        ensure!(
            entry.fingerprint == fingerprint,
            "Customization changed. Review the current declaration before approving."
        );
        ensure!(
            entry.review == ReviewState::Pending,
            "Customization is disabled or its target could not be resolved."
        );
        let payload = protection::protect(&serde_json::to_vec(entry)?)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO customization_approvals(checkout_id, project_id, entry_id, fingerprint, payload)
                 SELECT id, project_id, ?2, ?3, ?4 FROM checkouts WHERE id = ?1
                 ON CONFLICT DO NOTHING",
                params![checkout_id, entry_id, fingerprint, payload],
            )?;
            save(&tx, &checkout_id, &inventory)?;
            apply_approvals(&tx, &checkout_id, &mut inventory)?;
            tx.commit()?;
            Ok(inventory)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    #[tokio::test]
    async fn approval_covers_one_checkout_and_the_exact_script_and_declaration() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = dunce::canonicalize(temp.path())?;
        let repo = root.join("repository");
        fs::create_dir(&repo)?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()?
                .success()
        );
        fs::create_dir(repo.join(".claude"))?;
        fs::create_dir(repo.join("scripts"))?;
        fs::write(repo.join("AGENTS.md"), "Text is available immediately.")?;
        fs::write(
            repo.join(".claude/settings.json"),
            r#"{"hooks":{"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"powershell.exe -NoProfile -File ./scripts/start.ps1"}]}]}}"#,
        )?;
        fs::write(
            repo.join(".mcp.json"),
            r#"{"mcpServers":{"server":{"command":"node","args":["./scripts/server.js"]}}}"#,
        )?;
        let script = repo.join("scripts/start.ps1");
        fs::write(&script, "Set-Content launch-marker started")?;
        fs::write(
            repo.join("scripts/server.js"),
            "require('fs').writeFileSync('mcp-marker','started');",
        )?;
        let db_path = root.join("review.sqlite");
        let store = Store::open(db_path.clone())?;
        let checkout = repository::register(&store, repo.clone()).await?;
        let initial = store
            .saved_checkout_customizations(checkout.checkout_id.clone())
            .await?;
        assert_eq!(
            initial.instructions[0].text,
            "Text is available immediately."
        );
        let hook = initial
            .entries
            .iter()
            .find(|entry| entry.kind == CustomizationKind::Hook)
            .unwrap();
        let mcp = initial
            .entries
            .iter()
            .find(|entry| entry.kind == CustomizationKind::McpServer)
            .unwrap();
        assert_eq!(hook.review, ReviewState::Pending);
        assert_eq!(hook.provider, Provider::Claude);
        assert_eq!(hook.local_files.len(), 1);
        assert!(hook.local_files[0].path.ends_with("start.ps1"));
        assert_eq!(mcp.review, ReviewState::Pending);
        assert_eq!(mcp.local_files.len(), 1);
        let approved = store
            .approve_checkout_customization(
                checkout.checkout_id.clone(),
                hook.id.clone(),
                hook.fingerprint.clone(),
            )
            .await?;
        assert_eq!(
            approved
                .entries
                .iter()
                .find(|entry| entry.id == hook.id)
                .unwrap()
                .review,
            ReviewState::Approved
        );
        assert_eq!(
            approved
                .entries
                .iter()
                .find(|entry| entry.id == mcp.id)
                .unwrap()
                .review,
            ReviewState::Pending
        );

        ensure!(
            Command::new("git.exe")
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgSign=false",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    "Fixture"
                ])
                .current_dir(&repo)
                .status()?
                .success()
        );
        let linked = root.join("linked");
        ensure!(
            Command::new("git.exe")
                .args(["worktree", "add", "--quiet", "--detach"])
                .arg(&linked)
                .current_dir(&repo)
                .status()?
                .success()
        );
        fs::create_dir(linked.join(".claude"))?;
        fs::create_dir(linked.join("scripts"))?;
        fs::copy(
            repo.join(".claude/settings.json"),
            linked.join(".claude/settings.json"),
        )?;
        fs::copy(&script, linked.join("scripts/start.ps1"))?;
        let linked = repository::register(&store, linked).await?;
        assert_eq!(linked.project_id, checkout.project_id);
        let linked = store
            .saved_checkout_customizations(linked.checkout_id)
            .await?;
        assert_eq!(linked.entries[0].fingerprint, hook.fingerprint);
        assert_eq!(linked.entries[0].review, ReviewState::Pending);

        fs::write(&script, "Set-Content launch-marker changed")?;
        assert!(
            store
                .approve_checkout_customization(
                    checkout.checkout_id.clone(),
                    hook.id.clone(),
                    hook.fingerprint.clone()
                )
                .await
                .is_err()
        );
        let current = store
            .refresh_checkout_customizations(checkout.checkout_id.clone())
            .await?;
        let changed = current
            .entries
            .iter()
            .find(|entry| entry.id == hook.id)
            .unwrap();
        assert_eq!(changed.review, ReviewState::Changed);
        assert_ne!(changed.fingerprint, hook.fingerprint);
        store
            .approve_checkout_customization(
                checkout.checkout_id.clone(),
                changed.id.clone(),
                changed.fingerprint.clone(),
            )
            .await?;
        fs::write(
            repo.join(".claude/settings.json"),
            r#"{"hooks":{"SessionStart":[{"matcher":"resume","hooks":[{"type":"command","command":"powershell.exe -NoProfile -File ./scripts/start.ps1"}]}]}}"#,
        )?;
        let reopened = Store::open(db_path)?;
        let current = reopened
            .refresh_checkout_customizations(checkout.checkout_id)
            .await?;
        assert_eq!(
            current
                .entries
                .iter()
                .find(|entry| entry.id == hook.id)
                .unwrap()
                .review,
            ReviewState::Changed
        );
        assert!(!repo.join("launch-marker").exists());
        assert!(!repo.join("mcp-marker").exists());
        Ok(())
    }
}
