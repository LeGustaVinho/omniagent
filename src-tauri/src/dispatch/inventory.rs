use std::{
    borrow::Cow,
    path::{Component, Path, PathBuf},
};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    protection,
    providers::{
        ProviderEvent, ProviderEventData, ProviderLocation,
        configuration::InstructionInventory,
        plugins::{PluginInventory, ReportedPlugin},
        skills::{SkillCatalog, SkillEntry, SkillRequest},
    },
    store::Store,
};

use super::{ContextSnapshot, NativeHistory};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchInventory {
    pub dispatch_id: String,
    pub checkout: RecordedCheckout,
    pub inherited_checkout: Option<RecordedCheckout>,
    pub native_history: NativeHistory,
    pub history_reports: Vec<HistoryReport>,
    pub inventory: SavedInventory,
    pub observations: Vec<InventoryObservation>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedCheckout {
    pub run_id: String,
    pub checkout_id: String,
    pub working_directory: PathBuf,
    pub worktree_root: Option<PathBuf>,
    pub primary_root: Option<PathBuf>,
}

impl RecordedCheckout {
    fn from_snapshot(run_id: String, checkout_id: String, snapshot: &ContextSnapshot) -> Self {
        let instructions = snapshot.instructions.as_ref();
        Self {
            run_id,
            checkout_id,
            working_directory: instructions.map_or_else(
                || snapshot.scope.checkout.clone(),
                |inventory| inventory.working_directory.clone(),
            ),
            worktree_root: instructions.map(|inventory| inventory.checkout_root.clone()),
            primary_root: instructions.map(|inventory| inventory.primary_root.clone()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryReport {
    pub event_sequence: String,
    pub turns: Option<u64>,
    pub item_kind: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedInventory {
    pub instructions: Option<InstructionInventory>,
    pub filesystem_skills: Vec<SkillEntry>,
    pub reported_skills: Option<SkillCatalog>,
    pub requested_skill: Option<SkillRequest>,
    pub plugins: Option<PluginInventory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InventoryTarget {
    Instruction { index: usize },
    Skill { id: String },
    Plugin { index: usize },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InventoryReport {
    FileRead {
        path: String,
        tool: String,
    },
    SkillUse {
        name: String,
    },
    SessionSkill {
        name: String,
    },
    SessionPlugin {
        name: String,
        path: Option<String>,
    },
    Hook {
        trigger: String,
        status: String,
        name: Option<String>,
        path: Option<String>,
    },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryObservation {
    pub event_sequence: String,
    pub location: ProviderLocation,
    pub report: InventoryReport,
    pub targets: Vec<InventoryTarget>,
}

impl Store {
    pub async fn dispatch_inventory(
        &self,
        conversation_id: String,
        run_id: String,
    ) -> Result<Option<DispatchInventory>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let record = tx.query_row(
                "SELECT d.id, s.payload, n.provider_session_id, a.provider_turn_id, r.checkout_id
                 FROM context_snapshots s
                 JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
                 JOIN agent_attempts a ON a.run_id = s.run_id
                 JOIN agent_sessions n ON n.id = a.agent_session_id
                 JOIN runs r ON r.id = a.run_id
                 WHERE a.conversation_id = ?1 AND a.run_id = ?2",
                params![conversation_id, run_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?,
                          row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?,
                          row.get::<_, String>(4)?)),
            ).optional()?;
            let Some((dispatch_id, payload, session, turn, checkout_id)) = record else {
                return Ok(None);
            };
            let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&payload)?)?;
            let checkout = RecordedCheckout::from_snapshot(run_id.clone(), checkout_id, &snapshot);
            let native_history = snapshot.native_history;
            let inherited_checkout = inherited_checkout(&tx, &run_id, &native_history)?;
            let inventory = SavedInventory {
                instructions: snapshot.instructions,
                filesystem_skills: snapshot.filesystem_skills,
                reported_skills: snapshot.reported_skills,
                requested_skill: snapshot.requested_skill,
                plugins: snapshot.plugin_inventory,
            };
            let mut query = tx.prepare(
                "SELECT seq, source, payload FROM agent_events
                 WHERE run_id = ?1 AND source IN ('provider', 'application') ORDER BY seq",
            )?;
            let mut rows = query.query([run_id])?;
            let mut observations = Vec::new();
            let mut history_reports = Vec::new();
            let mut submitted = false;
            while let Some(row) = rows.next()? {
                let sequence: i64 = row.get(0)?;
                let source: String = row.get(1)?;
                if source == "application" && submitted {
                    continue;
                }
                let payload: Vec<u8> = row.get(2)?;
                let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
                if source == "application" {
                    submitted = matches!(event.data, ProviderEventData::DispatchSubmitted { dispatch_id: id } if id == dispatch_id);
                } else if belongs_to_dispatch(&event.location, session.as_deref(), turn.as_deref()) {
                    match event.data {
                        ProviderEventData::HistoryReported { turns, item_kind }
                            if !matches!(native_history, NativeHistory::Fresh) =>
                        {
                            history_reports.push(HistoryReport {
                                event_sequence: sequence.to_string(), turns, item_kind,
                            });
                        }
                        ProviderEventData::SessionCustomizationsReported { .. } =>
                            inventory.collect(sequence, event, &mut observations),
                        _ if submitted => inventory.collect(sequence, event, &mut observations),
                        _ => {}
                    }
                }
            }
            Ok(Some(DispatchInventory {
                dispatch_id, checkout, inherited_checkout, native_history, history_reports,
                inventory, observations,
            }))
        }).await
    }
}

fn inherited_checkout(
    db: &Connection,
    run_id: &str,
    history: &NativeHistory,
) -> Result<Option<RecordedCheckout>> {
    let (source_run, context, session) = match history {
        NativeHistory::Fresh => return Ok(None),
        NativeHistory::ProviderCursor {
            session_id,
            context_id,
        } => (None, context_id, session_id),
        NativeHistory::ProviderRecovery {
            session_id,
            source_run_id,
            context_id,
            ..
        } => (Some(source_run_id), context_id, session_id),
    };
    let record = db
        .query_row(
            "SELECT source.run_id, r.checkout_id, s.payload
         FROM agent_attempts current
         JOIN agent_attempts source ON source.conversation_id = current.conversation_id
           AND source.ordinal < current.ordinal
         JOIN agent_sessions n ON n.id = source.agent_session_id
         JOIN runs r ON r.id = source.run_id
         JOIN context_snapshots s ON s.run_id = source.run_id
         JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1 AND d.delivery = 'submitted'
         WHERE current.run_id = ?1 AND source.result_context_id = ?2
           AND n.provider_session_id = ?3
           AND (source.run_id = ?4 OR
                (?4 IS NULL AND source.agent_session_id = current.agent_session_id
                 AND source.provider_terminal = 'completed' AND r.status = 'succeeded'))
         ORDER BY source.ordinal DESC LIMIT 1",
            params![run_id, context, session, source_run],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()?;
    record
        .map(|(source_run, checkout_id, payload)| {
            let snapshot = serde_json::from_slice(&protection::unprotect(&payload)?)?;
            Ok(RecordedCheckout::from_snapshot(
                source_run,
                checkout_id,
                &snapshot,
            ))
        })
        .transpose()
}

impl SavedInventory {
    fn skills(&self) -> impl Iterator<Item = &SkillEntry> {
        self.filesystem_skills.iter().chain(
            self.reported_skills
                .iter()
                .flat_map(|catalog| &catalog.entries),
        )
    }

    fn plugins(&self) -> impl Iterator<Item = (usize, &ReportedPlugin)> {
        self.plugins
            .iter()
            .flat_map(|inventory| inventory.report.plugins.iter().enumerate())
    }

    fn collect(&self, sequence: i64, event: ProviderEvent, output: &mut Vec<InventoryObservation>) {
        let record = |report, targets| InventoryObservation {
            event_sequence: sequence.to_string(),
            location: event.location.clone(),
            report,
            targets,
        };
        match event.data {
            ProviderEventData::ContextRead { path, tool } => {
                let checkout_directory = self
                    .instructions
                    .as_ref()
                    .map(|inventory| inventory.working_directory.as_path());
                let Some(resolved) = resolve_reported_path(Path::new(&path), checkout_directory)
                else {
                    return;
                };
                let mut targets =
                    self.instructions
                        .iter()
                        .flat_map(|inventory| {
                            inventory.sources.iter().enumerate().filter_map(
                                |(index, instruction)| {
                                    same_path(&instruction.path, &resolved)
                                        .then_some(InventoryTarget::Instruction { index })
                                },
                            )
                        })
                        .collect::<Vec<_>>();
                targets.extend(unambiguous_skill(
                    self.skills()
                        .filter(|skill| same_path(&skill.path, &resolved)),
                ));
                if !targets.is_empty() {
                    output.push(record(InventoryReport::FileRead { path, tool }, targets));
                }
            }
            ProviderEventData::SkillUsed { name } => {
                let targets = self.skill_named(&name).into_iter().collect();
                output.push(record(InventoryReport::SkillUse { name }, targets));
            }
            ProviderEventData::SessionCustomizationsReported { skills, plugins } => {
                for name in skills.into_iter().flatten() {
                    let targets = self.skill_named(&name).into_iter().collect();
                    output.push(record(InventoryReport::SessionSkill { name }, targets));
                }
                for plugin in plugins.into_iter().flatten() {
                    let targets = if let Some(path) = plugin.path.as_deref() {
                        only_plugin(self.plugins().filter(|(_, saved)| {
                            saved
                                .install_path
                                .as_deref()
                                .is_some_and(|saved_path| same_path(Path::new(saved_path), path))
                        }))
                    } else {
                        only_plugin(self.plugins().filter(|(_, saved)| {
                            saved.id == plugin.name
                                || saved.name.as_deref() == Some(plugin.name.as_str())
                        }))
                    }
                    .into_iter()
                    .collect();
                    output.push(record(
                        InventoryReport::SessionPlugin {
                            name: plugin.name,
                            path: plugin.path.map(|path| path.display().to_string()),
                        },
                        targets,
                    ));
                }
            }
            ProviderEventData::HookObserved {
                trigger,
                name,
                source_path,
                status,
            } => {
                let target = source_path.as_deref().and_then(|path| {
                    only_plugin(self.plugins().filter(|(_, saved)| {
                        saved
                            .install_path
                            .as_deref()
                            .is_some_and(|install| within_path(path, Path::new(install)))
                    }))
                });
                output.push(record(
                    InventoryReport::Hook {
                        trigger,
                        status,
                        name,
                        path: source_path.map(|path| path.display().to_string()),
                    },
                    target.into_iter().collect(),
                ));
            }
            _ => {}
        }
    }

    fn skill_named(&self, name: &str) -> Option<InventoryTarget> {
        unambiguous_skill(self.skills().filter(|skill| {
            skill.name == name
                || name.split_once(':').is_some_and(|(namespace, local)| {
                    skill.name == local && skill.plugin_id.as_deref() == Some(namespace)
                })
        }))
    }
}

fn belongs_to_dispatch(
    location: &ProviderLocation,
    session: Option<&str>,
    turn: Option<&str>,
) -> bool {
    location.parent_item_id.is_none()
        && location
            .session_id
            .as_deref()
            .is_none_or(|observed| session == Some(observed))
        && location
            .turn_id
            .as_deref()
            .is_none_or(|observed| turn.is_none_or(|expected| observed == expected))
}

fn unambiguous_skill<'a>(
    mut candidates: impl Iterator<Item = &'a SkillEntry>,
) -> Option<InventoryTarget> {
    let skill = candidates.next()?;
    candidates
        .all(|other| other.id == skill.id && same_path(&other.path, &skill.path))
        .then(|| InventoryTarget::Skill {
            id: skill.id.clone(),
        })
}

fn only_plugin<'a>(
    mut candidates: impl Iterator<Item = (usize, &'a ReportedPlugin)>,
) -> Option<InventoryTarget> {
    let (index, _) = candidates.next()?;
    candidates
        .next()
        .is_none()
        .then_some(InventoryTarget::Plugin { index })
}

fn exact_path(path: &Path) -> Option<&Path> {
    let path = dunce::simplified(path);
    (path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir)))
    .then_some(path)
}

fn same_path(left: &Path, right: &Path) -> bool {
    exact_path(left)
        .zip(exact_path(right))
        .is_some_and(|(left, right)| left == right)
}

fn resolve_reported_path<'a>(path: &'a Path, cwd: Option<&Path>) -> Option<Cow<'a, Path>> {
    if let Some(path) = exact_path(path) {
        return Some(Cow::Borrowed(path));
    }
    if path.file_name().is_none()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    Some(Cow::Owned(exact_path(cwd?)?.join(path)))
}

fn within_path(path: &Path, root: &Path) -> bool {
    exact_path(path)
        .zip(exact_path(root))
        .is_some_and(|(path, root)| path != root && path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use anyhow::{Context, ensure};
    use serde_json::json;

    use crate::{
        conversations::{Conversation, SessionScope},
        dispatch::{Delivery, EventSource},
        providers::{
            Provider, SessionMode, SessionPlugin, capabilities::ReportedCapabilities,
            configuration, skills::SkillSource,
        },
        repository::{self, CheckoutLayout},
        runs::RunStatus,
    };

    use super::*;

    async fn prepare_inventory(
        store: &Store,
        chat: &Conversation,
        scope: &SessionScope,
        layout: &CheckoutLayout,
        plugin_skill: &SkillEntry,
    ) -> Result<(String, String)> {
        let attempt = store
            .prepare_agent_attempt(
                chat.id.clone(),
                chat.context_id.clone(),
                scope.clone(),
                "Review this checkout".into(),
            )
            .await?;
        store.mark_agent_running(attempt.run.id.clone()).await?;
        let (_, filesystem_skills, instructions) =
            configuration::inspect(scope, layout, &ReportedCapabilities::default())?;
        let snapshot = serde_json::from_value(json!({
            "version": 13,
            "contextId": attempt.context_id,
            "scope": attempt.scope,
            "submittedMessages": attempt.input,
            "nativeHistory": { "mode": "fresh" },
            "instructions": instructions,
            "filesystemSkills": filesystem_skills,
            "reportedSkills": SkillCatalog { entries: vec![plugin_skill.clone()], reported: true, issues: Vec::new() },
            "pluginInventory": serde_json::from_slice::<PluginInventory>(&fs::read(layout.root.join("plugins.json"))?)?,
        }))?;
        let run = attempt.run.id;
        let dispatch = store
            .prepare_dispatch(run.clone(), snapshot, vec![b"turn".to_vec()])
            .await?;
        store
            .settle_dispatch(dispatch.clone(), Delivery::Submitted)
            .await?;
        Ok((run, dispatch))
    }

    fn write_registry(checkout: &Path, install: &Path, version: &str, enabled: bool) -> Result<()> {
        fs::write(
            checkout.join("plugins.json"),
            serde_json::to_vec(&json!({
                "id": format!("registry-{version}"),
                "checkedAt": "fixture",
                "report": {
                    "provider": "codex",
                    "status": "reported",
                    "plugins": [{
                        "id": "workflow",
                        "version": version,
                        "enabled": enabled,
                        "scope": "project",
                        "installPath": install,
                    }],
                    "loadTelemetryReported": false,
                }
            }))?,
        )?;
        Ok(())
    }

    async fn observe(
        store: &Store,
        run: &str,
        source: EventSource,
        location: ProviderLocation,
        data: ProviderEventData,
    ) -> Result<()> {
        store
            .record_agent_observation(run.into(), source, ProviderEvent { location, data })
            .await
    }

    #[tokio::test]
    async fn historical_inventory_links_only_attributable_observations_after_edits_and_reopen()
    -> Result<()> {
        let temp = tempfile::tempdir()?;
        let checkout = dunce::canonicalize(temp.path())?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "--quiet"])
                .current_dir(&checkout)
                .status()?
                .success()
        );
        let db = checkout.join("inventory.sqlite");
        let store = Store::open(db.clone())?;
        let repo = repository::register(&store, checkout.clone()).await?;
        let chat = store
            .create_conversation(repo.checkout_id, "Inventory fixture".into())
            .await?;
        let scope = SessionScope {
            provider: Provider::Codex,
            profile_id: "fixture".into(),
            account_binding: Some("fixture-account".into()),
            checkout: checkout.clone(),
            executable: checkout.join("codex.exe"),
            cli_version: "1.2.3".into(),
            runner: "fixture-runner".into(),
            permission_revision: "fixture-policy".into(),
            mode: SessionMode::ReadOnly,
            model: None,
            effort: None,
        };
        let layout = CheckoutLayout {
            root: checkout.clone(),
            primary_root: checkout.clone(),
            cwd: checkout.clone(),
        };
        let guide = checkout.join("AGENTS.md");
        fs::write(&guide, "Original guide")?;
        let skill_name = "inventory-shared-fixture";
        let local_skill = checkout
            .join(".agents/skills")
            .join(skill_name)
            .join("SKILL.md");
        fs::create_dir_all(local_skill.parent().unwrap())?;
        fs::write(&local_skill, "Local shared skill")?;
        let discovery_only = checkout.join(".agents/skills/inventory-discovery-only/SKILL.md");
        fs::create_dir_all(discovery_only.parent().unwrap())?;
        fs::write(&discovery_only, "Discovered before dispatch")?;
        let install = checkout.join("plugins/workflow");
        let plugin_path = install.join(skill_name).join("SKILL.md");
        let plugin_skill = SkillEntry {
            id: SkillEntry::identity(Provider::Codex, &plugin_path)?,
            provider: Provider::Codex,
            source: SkillSource::Provider,
            name: skill_name.into(),
            path: plugin_path,
            scope: Some("project".into()),
            plugin_id: Some("workflow".into()),
            enabled: Some(true),
            revision: Some("plugin-skill-r1".into()),
        };
        write_registry(&checkout, &install, "1.2.0", true)?;
        let (run, dispatch) =
            prepare_inventory(&store, &chat, &scope, &layout, &plugin_skill).await?;
        let initial = store
            .dispatch_inventory(chat.id.clone(), run.clone())
            .await?
            .context("dispatch inventory missing")?;
        assert_eq!(initial.dispatch_id, dispatch);
        assert!(initial.observations.is_empty());
        let instruction = initial
            .inventory
            .instructions
            .as_ref()
            .unwrap()
            .sources
            .iter()
            .enumerate()
            .find(|(_, source)| source.path == guide && source.revision.is_some())
            .context("recorded guide missing")?;
        let instruction_target = InventoryTarget::Instruction {
            index: instruction.0,
        };
        let guide_revision = instruction.1.revision.clone();
        let discovered = initial
            .inventory
            .filesystem_skills
            .iter()
            .find(|entry| entry.path == discovery_only)
            .context("discovery-only skill missing from the original inventory")?;
        assert!(discovered.revision.is_some());

        let session = ProviderLocation {
            session_id: Some("native-session".into()),
            ..Default::default()
        };
        let location = ProviderLocation {
            turn_id: Some("native-turn".into()),
            ..session.clone()
        };
        let qualified_name = format!("workflow:{skill_name}");
        let qualified_use = ProviderEventData::SkillUsed {
            name: qualified_name.clone(),
        };
        observe(
            &store,
            &run,
            EventSource::Provider,
            session.clone(),
            qualified_use.clone(),
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Provider,
            session,
            ProviderEventData::SessionCustomizationsReported {
                skills: Some(vec![skill_name.into()]),
                plugins: Some(vec![SessionPlugin {
                    name: "workflow".into(),
                    path: Some(install.clone()),
                }]),
            },
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Application,
            ProviderLocation::default(),
            ProviderEventData::DispatchSubmitted {
                dispatch_id: dispatch.clone(),
            },
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Provider,
            location.clone(),
            ProviderEventData::ContextRead {
                path: guide.display().to_string(),
                tool: "Read".into(),
            },
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Provider,
            location.clone(),
            ProviderEventData::ContextRead {
                path: "AGENTS.md".into(),
                tool: "Read".into(),
            },
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Provider,
            location.clone(),
            ProviderEventData::ContextRead {
                path: "unknown/../AGENTS.md".into(),
                tool: "Read".into(),
            },
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Provider,
            location.clone(),
            ProviderEventData::ContextRead {
                path: local_skill.display().to_string(),
                tool: "Read".into(),
            },
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Provider,
            location.clone(),
            ProviderEventData::SkillUsed {
                name: skill_name.into(),
            },
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Provider,
            location.clone(),
            qualified_use.clone(),
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Provider,
            ProviderLocation {
                parent_item_id: Some("child-item".into()),
                ..location.clone()
            },
            qualified_use.clone(),
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Application,
            location.clone(),
            qualified_use,
        )
        .await?;
        observe(
            &store,
            &run,
            EventSource::Provider,
            location,
            ProviderEventData::HookObserved {
                trigger: "preToolUse".into(),
                name: None,
                source_path: Some(install.join("hooks.json")),
                status: "completed".into(),
            },
        )
        .await?;
        store
            .finish_agent_attempt(run.clone(), RunStatus::Succeeded, None)
            .await?;

        fs::write(&guide, "Edited guide")?;
        fs::remove_file(&discovery_only)?;
        write_registry(&checkout, &install, "2.0.0", false)?;
        drop(store);
        let store = Store::open(db)?;
        let historical = store
            .dispatch_inventory(chat.id.clone(), run.clone())
            .await?
            .unwrap();
        assert!(
            store
                .dispatch_inventory("unrelated".into(), run)
                .await?
                .is_none()
        );
        assert_eq!(historical.dispatch_id, dispatch);
        assert_eq!(
            historical.inventory.instructions.as_ref().unwrap().sources[instruction.0].revision,
            guide_revision
        );
        let plugin = &historical
            .inventory
            .plugins
            .as_ref()
            .unwrap()
            .report
            .plugins[0];
        assert_eq!(
            (plugin.version.as_deref(), plugin.enabled),
            (Some("1.2.0"), Some(true))
        );
        let recorded = &historical.observations;
        assert!(historical.inventory.filesystem_skills.contains(discovered));
        assert!(recorded.iter().all(|entry| !entry.targets.contains(&InventoryTarget::Skill {
            id: discovered.id.clone(),
        })));
        let local_skill_id = SkillEntry::identity(Provider::Codex, &local_skill)?;
        let expected_links = [
            vec![],
            vec![InventoryTarget::Plugin { index: 0 }],
            vec![instruction_target.clone()],
            vec![instruction_target],
            vec![InventoryTarget::Skill { id: local_skill_id }],
            vec![],
            vec![InventoryTarget::Skill {
                id: plugin_skill.id.clone(),
            }],
            vec![InventoryTarget::Plugin { index: 0 }],
        ];
        assert_eq!(
            recorded
                .iter()
                .map(|observation| &observation.targets)
                .collect::<Vec<_>>(),
            expected_links.iter().collect::<Vec<_>>()
        );
        assert!(matches!(
            recorded[0].report,
            InventoryReport::SessionSkill { .. }
        ));
        assert!(matches!(
            recorded[3].report,
            InventoryReport::FileRead { ref path, .. } if path == "AGENTS.md"
        ));
        assert!(matches!(
            recorded[4].report,
            InventoryReport::FileRead { .. }
        ));
        assert!(matches!(
            recorded[5].report,
            InventoryReport::SkillUse { .. }
        ));
        assert!(
            matches!(recorded[6].report, InventoryReport::SkillUse { ref name } if name == &qualified_name)
        );
        assert!(matches!(recorded[7].report, InventoryReport::Hook { .. }));

        let chat = store.conversation(chat.id).await?;
        let (next_run, _) =
            prepare_inventory(&store, &chat, &scope, &layout, &plugin_skill).await?;
        let next = store.dispatch_inventory(chat.id, next_run).await?.unwrap();
        assert!(
            next.inventory
                .instructions
                .unwrap()
                .sources
                .iter()
                .any(|source| {
                    source.path == guide
                        && source.revision.is_some()
                        && source.revision != guide_revision
                })
        );
        let next_plugins = next.inventory.plugins.unwrap().report.plugins;
        assert_eq!(
            (next_plugins[0].version.as_deref(), next_plugins[0].enabled),
            (Some("2.0.0"), Some(false))
        );
        assert!(!next.inventory.filesystem_skills.contains(discovered));
        assert!(next.observations.is_empty());
        Ok(())
    }
}
