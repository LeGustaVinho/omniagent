use base64::{Engine, engine::general_purpose::STANDARD};
use uuid::Uuid;

use crate::{
    repository::{
        destination_drift::{
            DestinationDriftOutcome, DestinationDriftPolicy, authorized_destination_drift,
        },
        verified_merge::{DeliveryMergeOutcome, DeliveryMergeRequest},
    },
    tasks::{DeliveryCompletionMilestone, DeliveryMilestonePolicy},
};

use super::*;

#[tokio::test]
async fn an_advanced_destination_gets_fresh_checks_and_its_own_acceptance() -> Result<()> {
    let test = setup_drift().await?;
    let initial = test.initial.clone();
    let actual = test.advance("destination.txt", "destination revision")?;
    assert_eq!(
        test.prepare(&actual).await?,
        DestinationDriftOutcome::Paused {
            expected_destination_sha: initial.clone(),
            actual_sha: actual.clone(),
        }
    );
    assert_eq!(
        test.store
            .delivery_destination_drift_policy(test.fixture.project.clone())
            .await?,
        DestinationDriftPolicy::Pause
    );
    let prepared = test
        .store
        .execute(|db| {
            Ok(db.query_row(
                "SELECT COUNT(*) FROM workflow_destination_drift_updates",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await?;
    assert_eq!(prepared, 0);

    fs::write(
        test.destination.join("local-draft.txt"),
        "destination draft",
    )?;
    fs::write(test.source_root.join("local-draft.txt"), "source draft")?;
    let before = test.snapshots()?;
    let DestinationDriftOutcome::RequiresAcceptance {
        preview,
        verification,
    } = test.prepare_requested(&actual, true).await?
    else {
        anyhow::bail!("a clean update did not request renewed acceptance");
    };
    assert_eq!(preview.source_sha, test.commit.sha);
    assert_eq!(preview.expected_destination_sha, initial);
    assert_eq!(preview.destination_sha, actual);
    assert_eq!(preview.changed_paths, ["first.txt", "second.txt"]);
    let patch = test
        .store
        .delivery_destination_drift_patch(
            test.fixture.project.clone(),
            test.commit.task_id.clone(),
            test.fixture.run.id.clone(),
            preview.id.clone(),
        )
        .await?;
    assert!(!patch.truncated);
    let patch = String::from_utf8(STANDARD.decode(&patch.patch_base64)?)?;
    assert!(patch.contains("+first.txt") && patch.contains("+second.txt"));
    assert!(!patch.contains("local-draft.txt") && !patch.contains("destination.txt"));
    assert_ne!(preview.tree, test.commit.tree);
    assert_ne!(verification.id, test.commit.verification_id);
    assert_eq!(verification.candidate_id, preview.id);
    assert!(matches!(
        verification.gates.as_slice(),
        [gate] if gate.gate.required
        && matches!(&gate.outcome, Some(CandidateGateOutcome::Passed { stdout, .. }) if stdout.contains("CHECKED SOURCE AND ADVANCED DESTINATION"))
    ));
    assert_eq!(test.snapshots()?, before);
    assert_eq!(
        test.store
            .delivery_destination_drift_policy(test.fixture.project.clone())
            .await?,
        DestinationDriftPolicy::Pause
    );

    assert!(
        test.authorize(&actual, &test.commit.acceptance_id)
            .await
            .is_err()
    );
    assert!(
        test.accept(&preview.id, &test.commit.verification_id)
            .await
            .is_err()
    );
    assert!(
        test.store
            .accept_delivery_destination_drift(
                test.fixture.project.clone(),
                test.commit.task_id.clone(),
                Uuid::new_v4().to_string(),
                preview.id.clone(),
                verification.id.clone(),
            )
            .await
            .is_err()
    );
    let DestinationDriftOutcome::Accepted { acceptance_id, .. } =
        test.accept(&preview.id, &verification.id).await?
    else {
        anyhow::bail!("the explicit acceptance was not recorded");
    };
    assert_eq!(test.authorize(&actual, &acceptance_id).await?, preview);
    let reopened = Store::open(test.database.clone())?;
    assert!(matches!(
        reopened.accept_delivery_destination_drift(
            test.fixture.project.clone(), test.commit.task_id.clone(), test.fixture.run.id.clone(),
            preview.id.clone(), verification.id.clone(),
        ).await?,
        DestinationDriftOutcome::Accepted { acceptance_id: saved, .. } if saved == acceptance_id
    ));

    let second = test.advance("second-advance.txt", "again")?;
    let moved = DestinationDriftOutcome::DestinationMoved {
        expected_destination_sha: actual,
        actual_sha: Some(second.clone()),
    };
    let before_retry = test.snapshots()?;
    assert_eq!(test.accept(&preview.id, &verification.id).await?, moved);
    assert_eq!(test.prepare(&second).await?, moved);
    assert!(test.authorize(&second, &acceptance_id).await.is_err());
    assert_eq!(
        test.store
            .verify_delivery_merge(test.request(Some(acceptance_id)))
            .await?,
        DeliveryMergeOutcome::DestinationDrift { recovery: moved }
    );
    assert_eq!(test.snapshots()?, before_retry);
    Ok(())
}

#[tokio::test]
async fn publishing_uses_the_accepted_drift_tree_and_exact_parents() -> Result<()> {
    let test = setup_drift().await?;
    enable_preparation(&test).await?;
    let actual = test.advance("destination.txt", "destination revision")?;
    git(&test.destination, &["switch", "--detach"])?;
    fs::write(
        test.destination.join("local-draft.txt"),
        "destination draft",
    )?;
    fs::write(test.source_root.join("local-draft.txt"), "source draft")?;
    let before = test.snapshots()?;
    let DestinationDriftOutcome::RequiresAcceptance {
        preview,
        verification,
    } = test.prepare(&actual).await?
    else {
        anyhow::bail!("a clean update did not request renewed acceptance");
    };
    let mut bypass = test.request(None);
    bypass.expected_destination_sha.clone_from(&actual);
    assert!(test.store.verify_delivery_merge(bypass).await.is_err());
    let DestinationDriftOutcome::Accepted { acceptance_id, .. } =
        test.accept(&preview.id, &verification.id).await?
    else {
        anyhow::bail!("the explicit acceptance was not recorded");
    };
    let result = test
        .store
        .verify_delivery_merge(test.request(Some(acceptance_id)))
        .await?;
    let DeliveryMergeOutcome::Verified { evidence } = result else {
        anyhow::bail!(
            "freshly checked and accepted destination update did not publish: {result:?}"
        );
    };
    assert_eq!(evidence.previous_destination_sha, actual);
    assert_eq!(
        evidence.original_destination_sha.as_deref(),
        Some(test.initial.as_str())
    );
    assert_eq!(
        git(&test.destination, &["rev-parse", "refs/heads/integration"])?,
        evidence.integration_sha
    );
    let observed = git(
        &test.destination,
        &["show", "-s", "--format=%T %P", &evidence.integration_sha],
    )?;
    assert_eq!(
        observed,
        format!("{} {} {}", preview.tree, actual, test.commit.sha)
    );
    assert_eq!(test.snapshots()?, before);
    Ok(())
}

#[tokio::test]
async fn the_attached_destination_accepts_only_the_exact_external_update_after_fresh_review()
-> Result<()> {
    let test = setup_drift().await?;
    let actual = test.advance("destination.txt", "destination revision")?;
    let DestinationDriftOutcome::RequiresAcceptance {
        preview,
        verification,
    } = test.prepare_requested(&actual, true).await?
    else {
        anyhow::bail!("the requested destination update was not checked");
    };
    let DestinationDriftOutcome::Accepted { acceptance_id, .. } =
        test.accept(&preview.id, &verification.id).await?
    else {
        anyhow::bail!("the explicit acceptance was not recorded");
    };
    assert!(matches!(
        test.store
            .verify_delivery_merge(test.request(Some(acceptance_id.clone())))
            .await?,
        DeliveryMergeOutcome::DestinationCheckedOut { .. }
    ));
    git(
        &test.destination,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@omniagent.invalid",
            "merge",
            "--no-ff",
            "--no-gpg-sign",
            "-m",
            "merge prepared destination update",
            &test.commit.sha,
        ],
    )?;
    let sha = git(&test.destination, &["rev-parse", "HEAD"])?;
    let mut external = test.request(None);
    external.external_sha = Some(sha.clone());
    assert!(
        test.store
            .verify_delivery_merge(external.clone())
            .await
            .is_err()
    );
    external.drift_acceptance_id = Some(acceptance_id);
    let outcome = test.store.verify_delivery_merge(external).await?;
    let DeliveryMergeOutcome::Verified { evidence } = outcome else {
        anyhow::bail!("an exact accepted external merge was not verified: {outcome:?}");
    };
    assert_eq!(evidence.previous_destination_sha, actual);
    assert_eq!(
        evidence.original_destination_sha.as_deref(),
        Some(test.initial.as_str())
    );
    assert_eq!(evidence.integration_sha, sha);
    Ok(())
}

#[tokio::test]
async fn a_conflicted_destination_update_preserves_both_worktrees() -> Result<()> {
    let test = setup_drift().await?;
    enable_preparation(&test).await?;
    let actual = test.advance("first.txt", "destination changes on the same path")?;
    fs::write(
        test.destination.join("local-draft.txt"),
        "destination draft",
    )?;
    fs::write(test.source_root.join("local-draft.txt"), "source draft")?;
    let before = test.snapshots()?;
    let recovery = test.prepare(&actual).await?;
    let DestinationDriftOutcome::Conflicted {
        preview_id,
        destination_sha,
        paths,
    } = &recovery
    else {
        anyhow::bail!("Git conflict was not surfaced: {recovery:?}");
    };
    assert_eq!(destination_sha, &actual);
    assert_eq!(paths, &["first.txt"]);
    assert!(test.accept(preview_id, "no-checks").await.is_err());
    assert_eq!(test.prepare(&actual).await?, recovery);
    assert_eq!(test.snapshots()?, before);
    Ok(())
}

#[tokio::test]
async fn a_required_integration_check_failure_blocks_acceptance_after_policy_changes() -> Result<()>
{
    let test = setup_drift().await?;
    enable_preparation(&test).await?;
    let actual = test.advance("destination.txt", "destination revision")?;
    let DestinationDriftOutcome::RequiresAcceptance {
        preview,
        verification,
    } = test.prepare(&actual).await?
    else {
        anyhow::bail!("initial destination checks did not pass");
    };
    let DestinationDriftOutcome::Accepted { acceptance_id, .. } =
        test.accept(&preview.id, &verification.id).await?
    else {
        anyhow::bail!("initial destination acceptance was not saved");
    };

    // A changed project policy cannot silently drop the prior units' required gate.
    let new_revision = test
        .store
        .set_candidate_gate_policy(test.fixture.project.clone(), vec![check("exit 17", false)?])
        .await?;
    assert_ne!(new_revision, verification.policy_revision);
    assert!(test.authorize(&actual, &acceptance_id).await.is_err());
    assert_eq!(
        test.accept(&preview.id, &verification.id).await?,
        DestinationDriftOutcome::ChecksChanged {
            preview_id: preview.id.clone()
        }
    );
    let DestinationDriftOutcome::ChecksFailed {
        preview: current,
        verification: failed,
    } = test.prepare(&actual).await?
    else {
        anyhow::bail!("new project checks did not run on the same pinned tree");
    };
    assert_eq!(current, preview);
    assert_ne!(failed.id, verification.id);
    assert_eq!(failed.policy_revision, new_revision);
    assert!(matches!(
        failed.gates.as_slice(),
        [gate] if gate.gate.required
        && matches!(&gate.outcome, Some(CandidateGateOutcome::Failed { exit_code: 17, .. }))
    ));
    assert!(test.accept(&preview.id, &failed.id).await.is_err());
    assert!(test.authorize(&actual, &acceptance_id).await.is_err());
    Ok(())
}

struct DriftTest {
    _temp: tempfile::TempDir,
    database: PathBuf,
    store: Store,
    fixture: ApprovedDeliveryFixture,
    destination: PathBuf,
    source_root: PathBuf,
    initial: String,
    commit: VerifiedCommit,
}

impl DriftTest {
    fn advance(&self, path: &str, contents: &str) -> Result<String> {
        fs::write(self.destination.join(path), contents)?;
        user_commit(&self.destination, path, "advance destination")
    }

    fn snapshots(&self) -> Result<Vec<(String, String, Vec<u8>)>> {
        [&self.destination, &self.source_root]
            .into_iter()
            .map(|root| {
                Ok((
                    git(root, &["rev-parse", "HEAD"])?,
                    git(root, &["status", "--porcelain=v1"])?,
                    fs::read(root.join("local-draft.txt"))?,
                ))
            })
            .collect()
    }

    async fn prepare(&self, actual: &str) -> Result<DestinationDriftOutcome> {
        self.prepare_requested(actual, false).await
    }

    async fn prepare_requested(
        &self,
        actual: &str,
        prepare_drift: bool,
    ) -> Result<DestinationDriftOutcome> {
        assert_eq!(
            git(&self.destination, &["rev-parse", "refs/heads/integration"])?,
            actual
        );
        let mut request = self.request(None);
        request.prepare_drift = prepare_drift;
        let outcome = self.store.verify_delivery_merge(request).await?;
        let DeliveryMergeOutcome::DestinationDrift { recovery } = outcome else {
            anyhow::bail!("merge engine did not surface destination drift: {outcome:?}");
        };
        Ok(recovery)
    }

    fn request(&self, drift_acceptance_id: Option<String>) -> DeliveryMergeRequest {
        DeliveryMergeRequest {
            project_id: self.fixture.project.clone(),
            task_id: self.commit.task_id.clone(),
            workflow_run_id: self.fixture.run.id.clone(),
            unit_id: self.commit.unit_id.clone(),
            commit_evidence_id: self.commit.id.clone(),
            expected_source_branch: self.commit.branch.clone(),
            expected_source_sha: self.commit.sha.clone(),
            destination_branch: "integration".into(),
            expected_destination_sha: self.initial.clone(),
            external_sha: None,
            drift_acceptance_id,
            prepare_drift: false,
            perform: true,
        }
    }

    async fn accept(&self, preview: &str, verification: &str) -> Result<DestinationDriftOutcome> {
        self.store
            .accept_delivery_destination_drift(
                self.fixture.project.clone(),
                self.commit.task_id.clone(),
                self.fixture.run.id.clone(),
                preview.into(),
                verification.into(),
            )
            .await
    }

    async fn authorize(
        &self,
        actual: &str,
        acceptance: &str,
    ) -> Result<crate::repository::destination_drift::DestinationDriftPreview> {
        let (commit, run, initial, actual, acceptance) = (
            self.commit.clone(),
            self.fixture.run.id.clone(),
            self.initial.clone(),
            actual.to_owned(),
            acceptance.to_owned(),
        );
        self.store
            .execute(move |db| {
                authorized_destination_drift(
                    db,
                    &commit,
                    &run,
                    "integration",
                    &initial,
                    &actual,
                    &acceptance,
                )
            })
            .await
    }
}

fn check(script: &str, required: bool) -> Result<CandidateGate> {
    let executable = PathBuf::from(std::env::var_os("SystemRoot").context("missing SystemRoot")?)
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    Ok(CandidateGate {
        name: "check exact integration".into(),
        required,
        action: CandidateGateAction::Command {
            executable,
            arguments: ["-NoProfile", "-NonInteractive", "-Command", script]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            timeout_seconds: Some(30),
        },
    })
}

async fn setup_drift() -> Result<DriftTest> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("delivery.sqlite");
    let store = Store::open(database.clone())?;
    let fixture = approved_delivery_fixture(&store, temp.path()).await?;
    let [first, last] = fixture.units.as_slice() else {
        anyhow::bail!("expected ordered delivery units");
    };
    let worktree = store
        .live_managed_worktree(fixture.project.clone(), first.task_id.clone())
        .await?
        .context("approved worktree is unavailable")?
        .worktree;
    store.set_candidate_gate_policy(fixture.project.clone(), vec![check(
        "if (Test-Path -LiteralPath 'destination.txt') { if ((Get-Content -LiteralPath 'second.txt' -Raw).Trim() -ne 'second.txt' -or (Get-Content -LiteralPath 'first.txt' -Raw).Trim() -ne 'first.txt') { exit 18 }; Write-Output 'CHECKED SOURCE AND ADVANCED DESTINATION' }; if ((Get-Content -LiteralPath 'first.txt' -Raw).Trim() -ne 'first.txt') { exit 17 }",
        true,
    )?]).await?;
    let root = Path::new(&fixture.checkout.repository_root);
    let initial = git(root, &["rev-parse", "HEAD"])?;
    let destination = temp.path().join("destination");
    git(
        root,
        &[
            "worktree",
            "add",
            "-b",
            "integration",
            destination.to_str().context("invalid fixture path")?,
            &initial,
        ],
    )?;
    store
        .set_delivery_milestone_policy(
            fixture.project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedCommit,
                destination_branch: Some("integration".into()),
            },
        )
        .await?;
    let first_accepted =
        capture_and_review(&store, &fixture, &worktree, first, "first.txt").await?;
    commit(&store, &fixture, first, &first_accepted, None).await?;
    let last_accepted = capture_and_review(&store, &fixture, &worktree, last, "second.txt").await?;
    let outcome = commit(&store, &fixture, last, &last_accepted, None).await?;
    let commit = verified(&store, &fixture, last, outcome).await?;
    let preview = store
        .delivery_merge_preview(
            fixture.project.clone(),
            commit.task_id.clone(),
            fixture.run.id.clone(),
            commit.unit_id.clone(),
            commit.id.clone(),
        )
        .await?;
    assert_eq!(preview.expected_destination_sha, initial);
    Ok(DriftTest {
        _temp: temp,
        database,
        store,
        fixture,
        destination,
        source_root: worktree.root,
        initial,
        commit,
    })
}

async fn enable_preparation(test: &DriftTest) -> Result<()> {
    test.store
        .set_delivery_destination_drift_policy(
            test.fixture.project.clone(),
            DestinationDriftPolicy::PrepareAndRecheck,
        )
        .await?;
    Ok(())
}
