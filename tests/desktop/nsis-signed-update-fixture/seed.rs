use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::process::Command;

use crate::{
    protection, restore,
    workflows::{
        DeliveryRoles,
        builtin::{BUILTIN_DELIVERY_ID, MAINTENANCE_ACTIVATION_ID},
        maintenance::{
            MaintenanceActivationMode, MaintenanceActivationRequest, MaintenanceBuild,
            MaintenanceConfiguration, MaintenanceHandoffRequest,
        },
        tests::{approve_reviewed_plan_fixture, link_reviewed_plan_fixture},
    },
};

use super::*;

#[path = "signed_update_fixture_queue.rs"]
mod queue;

const PLAN: &str = "Deliver one tested fixture build.\n\n```omniagent-plan\n{\"units\":[{\"purpose\":\"Verify the next package\",\"completionCriteria\":[\"The fixture build inputs match the version\"]}]}\n```";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildPlan {
    repository: String,
    baseline_commit: String,
    candidates: Vec<Candidate>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    version: String,
    commit: String,
    request: MaintenanceHandoffRequest,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignedCase {
    database: PathBuf,
    packages: PathBuf,
    requests: Vec<MaintenanceActivationRequest>,
    manifests: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mismatched_manifest: Option<serde_json::Value>,
}

fn automatic() -> bool {
    std::env::var("OMNIAGENT_SIGNED_TEST_AUTOMATIC").as_deref() == Ok("1")
}

#[tokio::test]
#[ignore = "only for the explicit isolated NSIS signed update harness"]
async fn seed_signed_update_case() -> Result<()> {
    let case = dunce::canonicalize(
        std::env::var_os("OMNIAGENT_SIGNED_TEST_CASE")
            .context("Missing disposable updater case")?,
    )?;
    ensure!(
        case.file_name()
            .and_then(|p| p.to_str())
            .is_some_and(|p| p.starts_with("omniagent-signed-update-")),
        "Refusing to seed outside a disposable updater case"
    );
    match std::env::var("OMNIAGENT_SIGNED_TEST_STEP")?.as_str() {
        "plan" => prepare(&case).await,
        "sign" => sign(&case).await,
        step => anyhow::bail!("Unknown updater fixture step: {step}"),
    }
}

async fn prepare(case: &Path) -> Result<()> {
    let keys = minisign::KeyPair::generate_unencrypted_keypair()?;
    fs::write(
        case.join("fixture.pub"),
        STANDARD.encode(keys.pk.to_box()?.into_string()),
    )?;
    fs::write(
        case.join("fixture-key.dpapi"),
        protection::protect(&keys.sk.to_bytes())?,
    )?;
    let installation = restore::open_installation(&case.join("seed"))?;
    let store = Store::open(installation.layout.database_path())?;
    let repo = approved_delivery_repository(&store, case).await?;
    let source = Path::new(&repo.checkout_root);
    let status = Command::new("tar.exe")
        .arg("-xf")
        .arg(case.join("fixture-source.tar"))
        .arg("-C")
        .arg(source)
        .status()?;
    ensure!(
        status.success(),
        "Could not extract the fixture from its archived source"
    );
    git(source, &["config", "core.autocrlf", "false"])?;
    git(source, &["config", "commit.gpgsign", "false"])?;
    git(source, &["config", "user.name", "NSIS Fixture"])?;
    git(
        source,
        &["config", "user.email", "nsis-fixture@example.invalid"],
    )?;
    git(source, &["add", "-A", "--", ".", ":(exclude)codex.exe"])?;
    git(
        source,
        &["commit", "-qm", "Archive the staged NSIS fixture source"],
    )?;
    let baseline_commit = git(source, &["rev-parse", "HEAD"])?;

    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").context("Windows root is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let arguments = |mode: &str| {
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-File".into(),
            ".\\fixture-build.ps1".into(),
            mode.into(),
            case.to_str()
                .expect("Windows fixture paths use Unicode")
                .into(),
        ]
    };
    store
        .set_candidate_gate_policy(
            repo.project_id.clone(),
            vec![CandidateGate {
                name: "Fixture version inputs".into(),
                required: true,
                action: CandidateGateAction::Command {
                    executable: powershell.clone(),
                    arguments: arguments("check"),
                    timeout_seconds: Some(30),
                },
            }],
        )
        .await?;
    let settings = store
        .set_workflow_maintenance(
            repo.project_id.clone(),
            None,
            MaintenanceConfiguration {
                source_checkout_id: repo.checkout_id.clone(),
                workflow_id: BUILTIN_DELIVERY_ID.into(),
                build: MaintenanceBuild {
                    executable: powershell,
                    arguments: arguments("package"),
                    timeout_seconds: Some(120),
                    package_path: "accepted-installer.exe".into(),
                    version_path: "accepted-version.txt".into(),
                },
                required_checks: vec!["Fixture version inputs".into()],
                activation: MaintenanceActivationMode::SignedUpdater,
            },
        )
        .await?;

    let mut candidates = Vec::new();
    for (old, next, migration) in [("0.1.0", "0.1.1", true), ("0.1.1", "0.1.2", false)] {
        if automatic() && !migration {
            break;
        }
        let run = store
            .start_builtin_delivery(repo.project_id.clone(), DeliveryRoles::default())
            .await?;
        let linked = link_reviewed_plan_fixture(
            &store,
            &repo,
            None,
            &run,
            PLAN,
            &format!("Accept fixture {next}"),
        )
        .await?;
        let fixture =
            approve_reviewed_plan_fixture(&store, case, repo.clone(), run, linked).await?;
        let [unit] = fixture.units.as_slice() else {
            anyhow::bail!("Expected one fixture release unit")
        };
        let worktree = store
            .live_managed_worktree(repo.project_id.clone(), unit.task_id.clone())
            .await?
            .context("Missing fixture delivery checkout")?
            .worktree;
        let job = queue_implementation(&store, &fixture, &worktree, unit).await?;
        let captured =
            capture_implementation_changes(&store, &fixture, &worktree, unit, job, |path| {
                change_versions(path, old, next, migration)
            })
            .await?;
        let mut files = vec![
            "src-tauri/Cargo.toml",
            "src-tauri/Cargo.lock",
            "src-tauri/tauri.conf.json",
        ];
        if migration {
            files.push("src-tauri/src/store.rs");
        }
        let saved = select_candidate(&store, &fixture, captured, &files).await?;
        let review =
            review_candidate(&store, &fixture, &worktree, unit, &saved, &first_reviewer()).await?;
        let checks = verify_candidate(&store, &fixture, unit, &saved).await?;
        let acceptance = accept_candidate(&store, &fixture, unit, &saved, &review, &checks).await?;
        let result = commit(
            &store,
            &fixture,
            unit,
            &AcceptedUnit {
                candidate: saved,
                acceptance,
            },
            None,
        )
        .await?;
        let WorkflowUnitOutcome::Artifact { artifact } = &result else {
            anyhow::bail!("Expected verified Git output")
        };
        let git_artifact_id = artifact.id.clone();
        let commit = verified(&store, &fixture, unit, result).await?;
        candidates.push(Candidate {
            version: next.into(),
            commit: commit.sha.clone(),
            request: MaintenanceHandoffRequest {
                project_id: repo.project_id.clone(),
                workflow_run_id: fixture.run.id,
                node_id: MAINTENANCE_ACTIVATION_ID.into(),
                git_artifact_id,
                task_id: unit.task_id.clone(),
                commit_evidence_id: commit.id,
                expected_settings_revision: settings.revision.clone(),
            },
        });
        git(source, &["merge", "--ff-only", &commit.sha])?;
    }
    if automatic() {
        queue::seed(&store, case, &repo).await?;
    }
    fs::write(
        case.join("plan.json"),
        serde_json::to_vec_pretty(&BuildPlan {
            repository: repo.checkout_root,
            baseline_commit,
            candidates,
        })?,
    )?;
    Ok(())
}

fn change_versions(root: &Path, old: &str, next: &str, migration: bool) -> Result<()> {
    for relative in ["src-tauri/Cargo.toml", "src-tauri/Cargo.lock"] {
        let file = root.join(relative);
        let text = fs::read_to_string(&file)?.replace("\r\n", "\n");
        let from = format!("name = \"omniagent\"\nversion = \"{old}\"");
        ensure!(
            text.contains(&from),
            "The fixture cannot find the prior crate version"
        );
        fs::write(
            file,
            text.replacen(
                &from,
                &format!("name = \"omniagent\"\nversion = \"{next}\""),
                1,
            ),
        )?;
    }
    let file = root.join("src-tauri/tauri.conf.json");
    let mut configuration: serde_json::Value = serde_json::from_slice(&fs::read(&file)?)?;
    ensure!(
        configuration["version"] == old,
        "The fixture cannot find the prior Tauri version"
    );
    configuration["version"] = next.into();
    fs::write(file, serde_json::to_vec_pretty(&configuration)?)?;
    if migration {
        let file = root.join("src-tauri/src/store.rs");
        let mut source = fs::read_to_string(&file)?;
        let start = source
            .find("const MIGRATIONS:")
            .context("Missing fixture migration list")?;
        let close = start
            + source[start..]
                .find("\n];")
                .context("Missing migration list terminator")?;
        source.insert_str(
            close,
            "\n    include_str!(\"../migrations/fixture-nsis-update.sql\"),",
        );
        fs::write(file, source)?;
    }
    Ok(())
}

async fn sign(case: &Path) -> Result<()> {
    let plan: BuildPlan = serde_json::from_slice(&fs::read(case.join("plan.json"))?)?;
    let installation = restore::open_installation(&case.join("seed"))?;
    let store = Store::open(installation.layout.database_path())?;
    let package_root = installation.layout.maintenance_packages();
    let secret = protection::unprotect(&fs::read(case.join("fixture-key.dpapi"))?)?;
    let secret = minisign::SecretKey::from_bytes(&secret)?;
    let key = String::from_utf8(STANDARD.decode(fs::read_to_string(case.join("fixture.pub"))?)?)?;
    let public = minisign::PublicKey::from_box(minisign::PublicKeyBox::from_string(&key)?)?;
    let mut requests = Vec::new();
    let mut manifests = Vec::new();
    let mut mismatched_manifest = None;
    for Candidate {
        version,
        commit,
        request,
    } in plan.candidates
    {
        let handoff = store
            .create_maintenance_handoff(request, package_root.clone())
            .await?;
        ensure!(
            handoff.version == version && handoff.source_revision == commit,
            "Fixture package scope drifted"
        );
        let bytes = fs::read(package_root.join(format!("{}.package", handoff.id)))?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == handoff.sha256,
            "Fixture package bytes changed"
        );
        let manifest = |source: &str| -> Result<serde_json::Value> {
            let comment = format!("version:{version}\tsource:{source}");
            let signing = minisign::sign(Some(&public), &secret, &bytes[..], Some(&comment), None)?
                .into_string();
            Ok(
                json!({"version":version, "url":"https://release.invalid/nsis-test.exe", "signature":STANDARD.encode(signing)}),
            )
        };
        if automatic() {
            mismatched_manifest = Some(manifest(&"f".repeat(40))?);
        }
        let request = MaintenanceActivationRequest {
            handoff_id: handoff.id,
            project_id: handoff.project_id,
            expected_settings_revision: handoff.settings_revision,
            expected_sha256: handoff.sha256,
        };
        store.require_signed_update_handoff(request.clone()).await?;
        requests.push(request);
        manifests.push(manifest(&commit)?);
    }
    let checkpoint = store
        .create_application_backup(installation.identity, package_root, case.to_path_buf())
        .await?;
    let export = case.join("exported");
    fs::create_dir(&export)?;
    for entry in &checkpoint.manifest.entries {
        entry.copy_verified(&checkpoint.root, &export)?;
    }
    let staged = SignedCase {
        database: export.join("omniagent.sqlite3"),
        packages: export.join("maintenance-packages"),
        requests,
        manifests,
        mismatched_manifest,
    };
    fs::write(
        case.join("signed-case.json"),
        serde_json::to_vec_pretty(&staged)?,
    )?;
    Ok(())
}
