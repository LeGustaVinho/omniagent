use std::{
    borrow::Cow,
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::Read,
    path::PathBuf,
    sync::LazyLock,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use regex::Regex;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    conversations::{MessageRevision, MessageRole, SessionScope},
    dispatch::ContextSnapshot,
    process_io::{CapturedProcess, Stream},
    protection, repository,
    runner::{DirectWindowsRunner, Runner},
    store::Store,
};

use super::{
    Provider, WireMessage,
    capabilities::{ReportedCapabilities, revision},
    codex::decode_skill_catalog,
    configuration,
    profiles::run_probe,
    runtime::bounded_write,
    skills::{SkillCatalog, SkillEntry, SkillSource},
};

const PREFIX: &str = "@skill:";
const MAX_SELECTIONS: usize = 8;
const MAX_SKILL_BYTES: u64 = 1024 * 1024;
const MAX_PROBE_FRAME_BYTES: usize = 2 * 1024 * 1024;
static MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\S+").expect("composer token pattern"));

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedComposerSkill {
    pub id: String,
    pub provider: Provider,
    pub name: String,
    pub path: PathBuf,
    pub namespace: Option<String>,
    pub revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerCustomization {
    pub marker: String,
    pub skill: SelectedComposerSkill,
    pub availability: ComposerAvailability,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposerAvailability {
    Provider,
    RecordedProvider,
    Filesystem,
}

impl Store {
    /// Discover against the selected checkout without opening a CLI thread.
    /// A failed probe falls back to disk and inventory from the same scope.
    pub async fn composer_customizations(
        &self,
        conversation: String,
        scope: SessionScope,
    ) -> Result<Vec<ComposerCustomization>> {
        let chat = self.conversation(conversation).await?;
        let checkout =
            repository::verify_checkout(self, chat.checkout_id.clone(), scope.checkout.clone())
                .await?;
        if scope.provider != Provider::Codex {
            return Ok(Vec::new());
        }
        let reusable_scope = scope.clone();
        let layout = checkout.clone();
        let current =
            tokio::task::spawn_blocking(move || probe_codex_skills(&reusable_scope, &layout))
                .await?;
        if let Ok(Some(current)) = current {
            return tokio::task::spawn_blocking(move || {
                discover(
                    &scope,
                    &checkout,
                    Some(current),
                    ComposerAvailability::Provider,
                )
            })
            .await?;
        }
        let saved_scope = scope.clone();
        let reported = self
            .execute(move |db| {
                let mut statement = db.prepare(
                    "SELECT s.payload FROM context_snapshots s
                 JOIN runs r ON r.id = s.run_id JOIN agent_attempts a ON a.run_id = r.id
                 WHERE r.checkout_id = ?1 AND NOT a.detached_context
                   AND EXISTS (SELECT 1 FROM dispatches d WHERE d.snapshot_id = s.id
                               AND d.ordinal = 1 AND d.delivery = 'submitted')
                 ORDER BY r.created_at DESC, a.ordinal DESC, r.id DESC LIMIT 64",
                )?;
                let mut rows = statement.query(params![chat.checkout_id])?;
                while let Some(row) = rows.next()? {
                    let snapshot: ContextSnapshot = serde_json::from_slice(
                        &protection::unprotect(row.get_ref(0)?.as_blob()?)?,
                    )?;
                    if snapshot.scope == saved_scope
                        && let Some(skills) =
                            snapshot.reported_skills.filter(|skills| skills.reported)
                    {
                        return Ok(Some(skills));
                    }
                }
                Ok(None)
            })
            .await?;
        tokio::task::spawn_blocking(move || {
            discover(
                &scope,
                &checkout,
                reported,
                ComposerAvailability::RecordedProvider,
            )
        })
        .await?
    }
}

fn probe_codex_skills(
    scope: &SessionScope,
    checkout: &repository::CheckoutLayout,
) -> Result<Option<SkillCatalog>> {
    if scope.runner != DirectWindowsRunner.identity() || !scope.executable.is_file() {
        return Ok(None);
    }
    let neutral = tempfile::Builder::new()
        .prefix("omniagent-composer-probe-")
        .tempdir()?;
    let projects: toml::Table = checkout
        .cwd
        .ancestors()
        .take_while(|directory| directory.starts_with(&checkout.root))
        .map(|directory| {
            (
                directory.to_string_lossy().into_owned(),
                toml::Value::Table(toml::Table::from_iter([(
                    "trust_level".into(),
                    toml::Value::String("untrusted".into()),
                )])),
            )
        })
        .collect();
    let arguments = [
        OsString::from("-c"),
        format!("projects={}", toml::Value::Table(projects)).into(),
        "-c".into(),
        "mcp_servers={}".into(),
        "app-server".into(),
        "--listen".into(),
        "stdio://".into(),
    ];
    let catalog = run_probe(
        |request| DirectWindowsRunner.launch(request),
        &scope.executable,
        neutral.path(),
        &arguments,
        Duration::from_secs(6),
        |mut process, deadline| {
            bounded_write(
                &mut process,
                vec![WireMessage::json(&json!({
                    "id":1, "method":"initialize", "params":{
                        "clientInfo":{"name":"omniagent-composer", "version":env!("CARGO_PKG_VERSION")},
                        "capabilities":{"experimentalApi":true}
                    }
                }))?],
            )?;
            probe_response(&process, deadline, 1)?;
            bounded_write(
                &mut process,
                vec![
                    WireMessage::json(&json!({"method":"initialized"}))?,
                    WireMessage::json(
                        &json!({"id":2,"method":"skills/list","params":{"cwds":[checkout.cwd],"forceReload":true}}),
                    )?,
                ],
            )?;
            decode_skill_catalog(&checkout.cwd, &probe_response(&process, deadline, 2)?)
        },
    )?;
    Ok(catalog.reported.then_some(catalog))
}

fn probe_response(process: &CapturedProcess, deadline: Instant, id: i64) -> Result<Value> {
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("Codex composer discovery timed out")??;
        if frame.stream != Stream::Stdout || frame.data.len() > MAX_PROBE_FRAME_BYTES {
            continue;
        }
        if let Ok(mut rpc) = serde_json::from_str::<Value>(&frame.data)
            && rpc["id"] == id
        {
            return rpc
                .get_mut("result")
                .map(Value::take)
                .context("Codex rejected composer skill discovery");
        }
    }
}

fn discover(
    scope: &SessionScope,
    checkout: &repository::CheckoutLayout,
    reported: Option<SkillCatalog>,
    report_availability: ComposerAvailability,
) -> Result<Vec<ComposerCustomization>> {
    let (_, discovered, _) =
        configuration::inspect(scope, checkout, &ReportedCapabilities::default())?;
    let live = reported.is_some() && report_availability == ComposerAvailability::Provider;
    let mut entries: BTreeMap<_, _> = discovered
        .into_iter()
        .filter(|_| !live)
        .map(|entry| (entry.id.clone(), entry))
        .collect();
    for entry in reported.into_iter().flat_map(|catalog| catalog.entries) {
        if entry.enabled == Some(true) {
            entries.insert(entry.id.clone(), entry);
        } else {
            entries.remove(&entry.id);
        }
    }
    let mut options = Vec::new();
    for entry in entries.values() {
        let Some((skill, _file)) = current_skill(entry)? else {
            continue;
        };
        options.push(ComposerCustomization {
            marker: marker(scope, &skill)?,
            skill,
            availability: if entry.source == SkillSource::Provider {
                report_availability
            } else {
                ComposerAvailability::Filesystem
            },
        });
    }
    options.sort_by(|first, second| {
        (&first.skill.name, &first.skill.namespace, &first.skill.path).cmp(&(
            &second.skill.name,
            &second.skill.namespace,
            &second.skill.path,
        ))
    });
    Ok(options)
}

fn current_skill(entry: &SkillEntry) -> Result<Option<(SelectedComposerSkill, File)>> {
    if !entry.path.is_absolute()
        || !entry.path.ends_with("SKILL.md")
        || entry.name.trim().is_empty()
    {
        return Ok(None);
    }
    let Ok(mut file) = configuration::open_locked_local_file(&entry.path) else {
        return Ok(None);
    };
    let mut contents = Vec::new();
    if (&mut file)
        .take(MAX_SKILL_BYTES + 1)
        .read_to_end(&mut contents)
        .is_err()
        || contents.len() as u64 > MAX_SKILL_BYTES
    {
        return Ok(None);
    }
    let revision = format!("{:x}", Sha256::digest(contents));
    Ok(Some((
        SelectedComposerSkill {
            id: entry.id.clone(),
            provider: entry.provider,
            name: entry.name.clone(),
            path: entry.path.clone(),
            namespace: entry.plugin_id.clone(),
            revision,
        },
        file,
    )))
}

fn marker(scope: &SessionScope, skill: &SelectedComposerSkill) -> Result<String> {
    let pin = revision(&(scope, &skill.id, &skill.namespace, &skill.revision))?;
    Ok(format!("{PREFIX}{}:{pin}", skill.id))
}

fn parts(token: &str) -> Option<(&str, &str)> {
    let (id, pin) = token.strip_prefix(PREFIX)?.split_once(':')?;
    [id, pin]
        .iter()
        .all(|field| {
            field.len() == 64
                && field
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .then_some((id, pin))
}

pub(crate) fn contains_skill_token(text: &str) -> bool {
    MARKER
        .find_iter(text)
        .any(|token| parts(token.as_str()).is_some())
}

/// A selected skill is an input action on the latest message. Replaying prior
/// message revisions as history must never select or invoke those skills again.
pub(crate) fn project_composer_messages(
    messages: &[MessageRevision],
) -> Cow<'_, [MessageRevision]> {
    if !messages
        .iter()
        .any(|message| message.role == MessageRole::User && contains_skill_token(&message.text))
    {
        return Cow::Borrowed(messages);
    }
    Cow::Owned(
        messages
            .iter()
            .cloned()
            .map(|mut message| {
                if message.role == MessageRole::User && contains_skill_token(&message.text) {
                    message.text = MARKER
                        .replace_all(&message.text, |token: &regex::Captures<'_>| {
                            if parts(&token[0]).is_some() {
                                String::new()
                            } else {
                                token[0].to_string()
                            }
                        })
                        .trim()
                        .into();
                }
                message
            })
            .collect(),
    )
}

pub(crate) fn resolve_selected_skills(
    scope: &SessionScope,
    text: &str,
    reported: Option<&SkillCatalog>,
) -> Result<Vec<(SelectedComposerSkill, File)>> {
    let mut selections = Vec::new();
    for token in MARKER.find_iter(text).map(|matched| matched.as_str()) {
        let Some((id, _)) = parts(token) else {
            continue;
        };
        if selections
            .iter()
            .any(|(skill, _): &(SelectedComposerSkill, File)| skill.id == id)
        {
            anyhow::bail!("A selected skill appears more than once in the draft");
        }
        ensure!(
            selections.len() < MAX_SELECTIONS,
            "Too many selected skills in this turn"
        );
        ensure!(
            scope.provider == Provider::Codex,
            "The selected skill belongs to a different CLI profile"
        );
        let mut available = reported
            .filter(|catalog| catalog.reported)
            .into_iter()
            .flat_map(|catalog| &catalog.entries)
            .filter(|entry| {
                entry.provider == scope.provider && entry.id == id && entry.enabled == Some(true)
            });
        let entry = available
            .next()
            .context("The selected skill is not available in the current CLI session")?;
        ensure!(
            available.next().is_none(),
            "The CLI reported multiple entries for the selected skill source"
        );
        let (skill, file) =
            current_skill(entry)?.context("The selected skill source is unavailable")?;
        ensure!(
            marker(scope, &skill)? == token,
            "The selected skill or CLI profile changed. Select the skill again."
        );
        selections.push((skill, file));
    }
    Ok(selections)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command, sync::Arc, time::Duration};

    use crate::providers::SessionMode;

    use super::*;

    fn scope(root: &Path) -> SessionScope {
        SessionScope {
            provider: Provider::Codex,
            profile_id: "codex-test".into(),
            account_binding: None,
            checkout: root.into(),
            executable: root.join("codex.exe"),
            cli_version: "0.169.0".into(),
            runner: "test".into(),
            permission_revision: "fixture".into(),
            mode: SessionMode::ReadOnly,
            model: Some("fixture-model".into()),
            effort: None,
        }
    }

    fn skill(root: &Path, directory: &str, namespace: Option<&str>) -> Result<SkillEntry> {
        let path = root.join(directory).join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(
            &path,
            "---\nname: format\ndescription: format the fixture\n---\nReturn FORMAT_FIXTURE.\n",
        )?;
        Ok(SkillEntry {
            id: SkillEntry::identity(Provider::Codex, &path)?,
            provider: Provider::Codex,
            source: SkillSource::Provider,
            name: "format".into(),
            path,
            scope: Some("repo".into()),
            plugin_id: namespace.map(str::to_owned),
            enabled: Some(true),
            revision: None,
        })
    }

    #[test]
    fn selections_pin_an_exact_export_and_revision_instead_of_its_display_name() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = dunce::canonicalize(temp.path())?;
        let original = skill(&root, ".agents/skills/format", None)?;
        let plugin = skill(&root, "plugin-cache/skills/format", Some("fixture@market"))?;
        let catalog = SkillCatalog {
            entries: vec![original.clone(), plugin.clone()],
            issues: Vec::new(),
            reported: true,
        };
        let scope = scope(&root);
        let layout = repository::CheckoutLayout {
            root: root.clone(),
            primary_root: root.clone(),
            cwd: root.clone(),
        };
        let options = discover(
            &scope,
            &layout,
            Some(catalog.clone()),
            ComposerAvailability::Provider,
        )?;
        let local = options
            .iter()
            .find(|item| item.skill.id == original.id)
            .context("missing local skill")?;
        let export = options
            .iter()
            .find(|item| item.skill.id == plugin.id)
            .context("missing plugin skill")?;
        assert_ne!(local.marker, export.marker);
        assert_eq!(local.skill.name, export.skill.name);
        let draft = format!("Inspect the fixture. {}", export.marker);
        let mut selected = resolve_selected_skills(&scope, &draft, Some(&catalog))?;
        let (selection, guard) = selected.pop().context("missing resolved skill")?;
        assert_eq!(selection, export.skill);
        assert!(selected.is_empty());
        assert!(resolve_selected_skills(&scope, &draft, None).is_err());
        let mut switched = scope.clone();
        switched.model = Some("other-model".into());
        assert!(resolve_selected_skills(&switched, &draft, Some(&catalog)).is_err());
        switched.model.clone_from(&scope.model);
        switched.checkout = root.join("other-worktree");
        assert!(resolve_selected_skills(&switched, &draft, Some(&catalog)).is_err());
        assert!(fs::write(&plugin.path, "replacement while dispatch owns the source").is_err());
        drop(guard);
        fs::write(&plugin.path, "replacement")?;
        assert!(resolve_selected_skills(&scope, &draft, Some(&catalog)).is_err());

        let message = |id: &str, text: String| MessageRevision {
            message_id: id.into(),
            revision_id: id.into(),
            role: MessageRole::User,
            text,
        };
        let transcript = vec![
            message("earlier", draft),
            message("current", "Just explain.".into()),
        ];
        let projection = project_composer_messages(&transcript);
        assert_eq!(projection[0].text, "Inspect the fixture.");
        assert!(transcript[0].text.contains(&export.marker));
        assert!(resolve_selected_skills(&scope, &transcript[1].text, Some(&catalog))?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn local_discovery_uses_the_conversation_checkout() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = dunce::canonicalize(temp.path())?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()?
                .success()
        );
        let expected = skill(&root, ".agents/skills/composer-picker-fixture", None)?;
        let store = Store::open(root.join("fixture.sqlite"))?;
        let checkout = repository::register(&store, root.clone()).await?;
        let chat = store
            .create_conversation(checkout.checkout_id, "Composer skill fixture".into())
            .await?;
        let available = store
            .composer_customizations(chat.id.clone(), scope(&root))
            .await?;
        assert!(available.iter().any(|option| option.skill.id == expected.id
            && option.availability == ComposerAvailability::Filesystem));
        assert!(
            store
                .composer_customizations(chat.id, scope(&root.join("another-worktree")))
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires installed Codex and the disposable composer plugin; no authentication is needed"]
    async fn live_composer_catalog_reads_the_installed_export_without_opening_a_thread()
    -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = dunce::canonicalize(temp.path())?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()?
                .success()
        );
        let mut selected = scope(&root);
        selected.executable = Provider::Codex.adapter().discover(None)?;
        selected.runner = DirectWindowsRunner.identity().into();
        let store = Store::open(root.join("discovery.sqlite"))?;
        let checkout = repository::register(&store, root).await?;
        let chat = store
            .create_conversation(checkout.checkout_id, "Disposable discovery fixture".into())
            .await?;
        let available = store
            .composer_customizations(chat.id.clone(), selected)
            .await?;
        ensure!(
            available
                .iter()
                .any(|entry| entry.availability == ComposerAvailability::Provider
                    && entry.skill.name == "omniagent-composer-plugin:omniagent-composer-export"
                    && entry.skill.namespace.as_deref()
                        == Some("omniagent-composer-plugin@omniagent-fixture")),
            "Codex did not expose the installed fixture export"
        );
        assert!(store.latest_conversation_run(chat.id).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires authenticated Codex and the disposable omniagent-composer-plugin@omniagent-fixture installation"]
    async fn live_composer_skill_and_plugin_export_reach_the_saved_codex_dispatch() -> Result<()> {
        use crate::{
            conversations::{ContinuationInspection, references::ConversationReferenceSelection},
            dispatch::{ConversationArtifactRevision, Delivery, SelectedMessageSpan},
            jobs::AdmissionEnvironment,
            providers::{
                profiles::{ConfiguredExecutable, DESKTOP_PERMISSION_REVISION, desktop_profile},
                runtime::start_managed_turn,
            },
            runner::{DirectWindowsRunner, Runner},
            runs::RunStatus,
        };

        let temp = tempfile::tempdir()?;
        let root = dunce::canonicalize(temp.path())?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()?
                .success()
        );
        let local_id = "omniagent-composer-native-skill";
        let local_dir = root.join(".agents/skills").join(local_id);
        fs::create_dir_all(&local_dir)?;
        fs::write(
            local_dir.join("SKILL.md"),
            format!(
                "---\nname: {local_id}\ndescription: Test the repository composer skill route.\n---\nReply only with OMNIAGENT_NATIVE_SKILL_OK.\n"
            ),
        )?;
        let profile = desktop_profile(
            ConfiguredExecutable {
                provider: Provider::Codex,
                executable: None,
            },
            None,
        )?;
        let model = profile
            .models
            .into_iter()
            .find(|entry| {
                entry.option.selectable && entry.option.selection.model_id == "gpt-6-luna"
            })
            .context("Codex fixture model unavailable")?;
        let scope = SessionScope {
            provider: Provider::Codex,
            profile_id: model.option.selection.profile_id,
            account_binding: profile.account_binding,
            checkout: root.clone(),
            executable: profile.executable,
            cli_version: profile.version,
            runner: DirectWindowsRunner.identity().into(),
            permission_revision: DESKTOP_PERMISSION_REVISION.into(),
            mode: SessionMode::ReadOnly,
            model: Some(model.option.selection.model_id),
            effort: None,
        };
        let store = Store::open(root.join("live-composer.sqlite"))?;
        let checkout = repository::register(&store, root.clone()).await?;
        let chat = store
            .create_conversation(checkout.checkout_id, "Disposable composer fixture".into())
            .await?;
        for (name, namespace, verdict) in [
            (local_id, None, "OMNIAGENT_NATIVE_SKILL_OK"),
            (
                "omniagent-composer-plugin:omniagent-composer-export",
                Some("omniagent-composer-plugin@omniagent-fixture"),
                "OMNIAGENT_PLUGIN_EXPORT_OK",
            ),
        ] {
            let options = store
                .composer_customizations(chat.id.clone(), scope.clone())
                .await?;
            let option = options
                .iter()
                .find(|option| {
                    option.skill.name == name && option.skill.namespace.as_deref() == namespace
                })
                .with_context(|| format!("picker did not discover {name} from {namespace:?}"))?;
            let current = store.conversation(chat.id.clone()).await?;
            let text = format!("Follow the selected skill. {}", option.marker);
            let (attempt, attached) = if namespace.is_some() {
                let source = store
                    .fork_conversation(
                        chat.id.clone(),
                        current.context_id.clone(),
                        None,
                        "Separate source conversation".into(),
                    )
                    .await?;
                let references = store
                    .conversation_reference_source(chat.id.clone(), source.id.clone(), None)
                    .await?;
                let chosen = references
                    .messages
                    .last()
                    .context("missing source message revision")?;
                assert!(chosen.text.contains("OMNIAGENT_NATIVE_SKILL_OK"));
                let selection = ConversationReferenceSelection::Messages {
                    context_id: references.context_id,
                    spans: vec![SelectedMessageSpan {
                        revision_id: chosen.revision_id.clone(),
                        byte_range: None,
                    }],
                };
                let foreign = temp.path().join("foreign-repository");
                fs::create_dir(&foreign)?;
                ensure!(
                    Command::new("git.exe")
                        .args(["init", "--quiet"])
                        .current_dir(&foreign)
                        .status()?
                        .success()
                );
                let foreign = repository::register(&store, foreign).await?;
                let foreign_destination = store
                    .create_conversation(foreign.checkout_id, "Foreign project".into())
                    .await?;
                assert!(
                    store
                        .conversation_reference_source(
                            foreign_destination.id.clone(),
                            source.id.clone(),
                            None
                        )
                        .await
                        .is_err()
                );
                assert!(
                    store
                        .preview_conversation_reference(
                            foreign_destination.id,
                            source.id.clone(),
                            selection.clone()
                        )
                        .await
                        .is_err()
                );
                let preview = store
                    .preview_conversation_reference(chat.id.clone(), source.id, selection)
                    .await?;
                assert!(
                    matches!(&preview.artifact, ConversationArtifactRevision::Excerpt { excerpts, .. }
                    if excerpts.len() == 1 && excerpts[0].source.origin.revision_id == chosen.revision_id
                        && excerpts[0].text == chosen.text)
                );
                let attached = store
                    .edit_conversation_reference_draft(chat.id.clone(), preview.id.clone(), true)
                    .await?;
                assert_eq!(attached, [preview.clone()]);
                store
                    .save_conversation_draft(chat.id.clone(), text.clone())
                    .await?;
                let queued = store
                    .submit_conversation_with_references(
                        chat.id.clone(),
                        current.context_id,
                        scope.clone(),
                        text,
                        vec![preview.id.clone()],
                    )
                    .await?;
                ensure!(
                    matches!(queued, ContinuationInspection::Queued { .. }),
                    "the selected chat did not queue its continued turn"
                );
                let attempt = store
                    .claim_next_job(AdmissionEnvironment {
                        runner: DirectWindowsRunner.identity().into(),
                        bridge_available: false,
                    })
                    .await?
                    .context("the selected composer turn was not admitted")?;
                (attempt, Some(preview))
            } else {
                (
                    store
                        .prepare_agent_attempt(
                            chat.id.clone(),
                            current.context_id,
                            scope.clone(),
                            text,
                        )
                        .await?,
                    None,
                )
            };
            let run = attempt.run.id.clone();
            let status = tokio::time::timeout(
                Duration::from_secs(150),
                start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), attempt, None)
                    .wait(),
            )
            .await??;
            assert_eq!(status, RunStatus::Succeeded);
            let dispatch = store
                .agent_dispatch(chat.id.clone(), run.clone())
                .await?
                .context("missing live dispatch")?;
            assert_eq!(dispatch.delivery, Delivery::Submitted);
            let selection = dispatch
                .snapshot
                .requested_skill
                .context("missing selected skill in snapshot")?;
            assert_eq!(selection.selections, [option.skill.clone()]);
            let frame: serde_json::Value = serde_json::from_slice(&dispatch.frames[0])?;
            assert!(
                frame["params"]["input"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!({
                        "type":"skill", "name":name, "path":option.skill.path
                    }))
            );
            if let Some(preview) = attached {
                let [saved] = dispatch.snapshot.conversation_references.as_slice() else {
                    anyhow::bail!("expected one saved conversation reference");
                };
                assert_eq!(saved.reference, preview);
                assert_eq!(
                    Some(saved.message_revision_id.as_str()),
                    dispatch
                        .snapshot
                        .submitted_messages
                        .last()
                        .map(|message| message.revision_id.as_str())
                );
                assert!(
                    frame["params"]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item["type"] == "text"
                            && item["text"]
                                .as_str()
                                .unwrap_or_default()
                                .contains("OMNIAGENT_NATIVE_SKILL_OK"))
                );
            }
            let history = store.conversation_messages(chat.id.clone()).await?;
            let answer = &history.last().context("missing provider response")?.text;
            ensure!(
                answer.contains(verdict),
                "Codex did not apply the selected skill {name}"
            );
            eprintln!("{name} ({namespace:?}) reached and was used by Codex in {run}");
        }
        Ok(())
    }
}
