use std::{fs, path::Path};

use anyhow::{Result, bail};

use crate::repository::github::RemoteMergeProof;
use crate::repository::{git_path, git_query, inspect, verified_git::CommitSource};

use super::{
    DeliveryMergeRequest, GithubSyncOutcome, MergeRepository, Plan, Publication, SourceSync,
    SourceSyncKind, VerifiedCommit,
};

struct Fixture {
    _scratch: tempfile::TempDir,
    root: std::path::PathBuf,
    repo: MergeRepository,
    request: DeliveryMergeRequest,
    commits: Vec<VerifiedCommit>,
}

impl Fixture {
    fn new() -> Result<Self> {
        let scratch = tempfile::tempdir()?;
        let root = dunce::canonicalize(scratch.path())?;
        run(&root, &["init", "--quiet", "--initial-branch=main"])?;
        fs::write(root.join("base.txt"), "base\n")?;
        let base = commit(&root, "Initial commit")?;
        run(&root, &["switch", "--quiet", "-c", "delivery"])?;
        fs::write(root.join("first.txt"), "first approved unit\n")?;
        let first = commit(&root, "First unit")?;
        fs::write(root.join("second.txt"), "second approved unit\n")?;
        let second = commit(&root, "Second unit")?;
        run(&root, &["switch", "--quiet", "-c", "integration", &base])?;
        fs::write(
            root.join("destination.txt"),
            "retained destination content\n",
        )?;
        let destination = commit(&root, "Independent destination work")?;
        run(&root, &["switch", "--quiet", "main"])?;
        let repo = MergeRepository::open(root.clone(), &inspect(&root)?.git_common_dir)?;
        let commits = [(&base, &first), (&first, &second)]
            .into_iter()
            .enumerate()
            .map(|(index, (parent, sha))| {
                let tree = run(&root, &["rev-parse", &format!("{sha}^{{tree}}")])?;
                let id = format!("unit-{index}");
                Ok(VerifiedCommit {
                    id: id.clone(),
                    project_id: "project".into(),
                    task_id: "task".into(),
                    unit_id: id,
                    acceptance_id: "acceptance".into(),
                    acceptance_artifact_id: None,
                    candidate_revision: "candidate".into(),
                    verification_id: "checks".into(),
                    capture_run_id: "run".into(),
                    checkout_id: "checkout".into(),
                    branch: "delivery".into(),
                    repository: root.to_string_lossy().into_owned(),
                    base_commit: parent.clone(),
                    tree,
                    sha: sha.clone(),
                    purpose: "Ship unit".into(),
                    verified_at: "2025-06-21T09:08:07.123Z".into(),
                    source: CommitSource::Automatic,
                    pending_changes_retained: false,
                })
            })
            .collect::<Result<_>>()?;
        let request = DeliveryMergeRequest {
            project_id: "project".into(),
            task_id: "task".into(),
            workflow_run_id: "workflow".into(),
            unit_id: "unit-1".into(),
            commit_evidence_id: "unit-1".into(),
            expected_source_branch: "delivery".into(),
            expected_source_sha: second,
            destination_branch: "integration".into(),
            expected_destination_sha: destination,
            external_sha: None,
            drift_acceptance_id: None,
            prepare_drift: false,
            perform: true,
        };
        Ok(Self {
            _scratch: scratch,
            root,
            repo,
            request,
            commits,
        })
    }

    fn prepare(&self) -> Result<String> {
        match self.repo.prepare(&self.request, &self.commits)? {
            Plan::Ready(sha) => Ok(sha),
            Plan::Conflicted => bail!("fixture branches should merge cleanly"),
        }
    }
}

#[test]
fn verifies_the_whole_chain_and_publishes_the_expected_refs_without_touching_files() -> Result<()> {
    let fixture = Fixture::new()?;
    let Fixture {
        root,
        repo,
        request,
        commits,
        ..
    } = &fixture;
    repo.verify_chain(commits)?;
    let mut wrong_receipts = commits.clone();
    wrong_receipts[0].tree = wrong_receipts[1].tree.clone();
    assert!(repo.verify_chain(&wrong_receipts).is_err());

    let integration = fixture.prepare()?;
    assert_eq!(fixture.prepare()?, integration);
    repo.verify_incorporation(request, &integration, commits)?;
    repo.verify_external(request, &integration)?;
    let expected_parents = format!(
        "{} {}",
        request.expected_destination_sha, request.expected_source_sha
    );
    assert_eq!(
        run(root, &["show", "-s", "--format=%P", &integration])?,
        expected_parents
    );
    for path in ["first.txt", "second.txt", "destination.txt"] {
        run(root, &["cat-file", "-e", &format!("{integration}:{path}")])?;
    }

    // A commit with the identical tree but a different history is inadequate.
    let integration_tree = run(root, &["rev-parse", &format!("{integration}^{{tree}}")])?;
    let same_tree = run(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit-tree",
            "--no-gpg-sign",
            &integration_tree,
            "-p",
            &request.expected_destination_sha,
            "-m",
            "Copied bytes",
        ],
    )?;
    assert!(
        repo.verify_incorporation(request, &same_tree, commits)
            .is_err()
    );

    let destination_tree = run(
        root,
        &[
            "rev-parse",
            &format!("{}^{{tree}}", request.expected_destination_sha),
        ],
    )?;
    let discarded = run(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit-tree",
            "--no-gpg-sign",
            &destination_tree,
            "-p",
            &request.expected_destination_sha,
            "-p",
            &request.expected_source_sha,
            "-m",
            "Discarded approved changes",
        ],
    )?;
    repo.verify_incorporation(request, &discarded, commits)?;
    assert!(repo.verify_external(request, &discarded).is_err());
    let descendant = run(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit-tree",
            "--no-gpg-sign",
            &destination_tree,
            "-p",
            &integration,
            "-m",
            "Reverted the integration",
        ],
    )?;
    let mut retrospective = request.clone();
    retrospective
        .expected_destination_sha
        .clone_from(&integration);
    repo.verify_external(&retrospective, &integration)?;
    retrospective
        .expected_destination_sha
        .clone_from(&descendant);
    repo.verify_incorporation(&retrospective, &descendant, commits)?;
    assert!(repo.verify_external(&retrospective, &descendant).is_err());

    // A different checkout remains dirty without affecting a ref-only merge.
    fs::write(root.join("base.txt"), "do not change local edits\n")?;
    assert!(matches!(
        repo.publish(request, &integration)?,
        Publication::Published
    ));
    assert_eq!(repo.branch_tip("integration")?, integration);
    assert_eq!(repo.branch_tip("delivery")?, request.expected_source_sha);
    assert_eq!(
        fs::read(root.join("base.txt"))?,
        b"do not change local edits\n"
    );
    assert!(!root.join("first.txt").exists());
    assert!(
        matches!(repo.publish(request, &integration)?, Publication::DestinationMoved(Some(actual)) if actual == integration)
    );
    Ok(())
}

#[test]
fn leaves_an_attached_destination_and_a_moved_source_untouched() -> Result<()> {
    let fixture = Fixture::new()?;
    let Fixture {
        root,
        repo,
        request,
        ..
    } = &fixture;
    let integration = fixture.prepare()?;
    let checkout = tempfile::tempdir()?;
    run(
        root,
        &[
            "worktree",
            "add",
            "--quiet",
            checkout.path().to_str().unwrap(),
            "integration",
        ],
    )?;
    let local = checkout.path().join("destination.txt");
    fs::write(&local, "unreviewed work\n")?;
    assert!(matches!(
        repo.publish(request, &integration)?,
        Publication::DestinationCheckedOut(_)
    ));
    assert_eq!(
        repo.branch_tip("integration")?,
        request.expected_destination_sha
    );
    assert_eq!(fs::read(&local)?, b"unreviewed work\n");
    run(
        root,
        &[
            "worktree",
            "remove",
            "--force",
            checkout.path().to_str().unwrap(),
        ],
    )?;
    run(
        root,
        &["update-ref", "refs/heads/delivery", &fixture.commits[0].sha],
    )?;
    assert!(
        repo.publish(request, &integration)
            .unwrap_err()
            .to_string()
            .contains("working branch moved")
    );
    assert_eq!(
        repo.branch_tip("integration")?,
        request.expected_destination_sha
    );
    Ok(())
}

#[test]
fn reports_conflicts_without_updating_the_destination() -> Result<()> {
    let mut fixture = Fixture::new()?;
    run(&fixture.root, &["switch", "--quiet", "integration"])?;
    fs::write(
        fixture.root.join("first.txt"),
        "competing destination addition\n",
    )?;
    fixture.request.expected_destination_sha = commit(&fixture.root, "Competing first file")?;
    run(&fixture.root, &["switch", "--quiet", "main"])?;
    assert!(matches!(
        fixture.repo.prepare(&fixture.request, &fixture.commits)?,
        Plan::Conflicted
    ));
    let last = &fixture.commits[1];
    let baseline = &fixture.commits[0].base_commit;
    let head = fixture.repo.verify_source_syncs(baseline, last, &[])?;
    assert!(matches!(
        fixture.repo.prepare_source_sync(
            &head,
            last,
            SourceSyncKind::Destination,
            &fixture.request.expected_destination_sha,
        )?,
        GithubSyncOutcome::Conflict
    ));
    assert_eq!(
        fixture.repo.branch_tip("integration")?,
        fixture.request.expected_destination_sha
    );
    Ok(())
}

fn source_sync(outcome: GithubSyncOutcome) -> Result<SourceSync> {
    match outcome {
        GithubSyncOutcome::Ready(receipt) => Ok(receipt),
        _ => bail!("fixture source sync should produce a merge object"),
    }
}

fn merge_commit(root: &Path, tree: &str, first: &str, second: &str) -> Result<String> {
    run(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit-tree",
            "--no-gpg-sign",
            tree,
            "-p",
            first,
            "-p",
            second,
            "-m",
            "Fixture merge",
        ],
    )
}

#[test]
fn github_source_sync_preserves_verified_and_destination_history_through_repair() -> Result<()> {
    let fixture = Fixture::new()?;
    let Fixture {
        root,
        repo,
        request,
        commits,
        ..
    } = &fixture;
    let approved = &commits[1];
    let original_base = &commits[0].base_commit;
    let destination = &request.expected_destination_sha;
    let original = repo.verify_source_syncs(original_base, approved, &[])?;
    assert_eq!(
        repo.prepare_source_sync(
            &original,
            approved,
            SourceSyncKind::Destination,
            original_base
        )?,
        GithubSyncOutcome::Unchanged
    );

    run(root, &["switch", "--quiet", "delivery"])?;
    fs::write(root.join("repair.txt"), "reviewed CI repair\n")?;
    let repaired_sha = commit(root, "Approved CI repair")?;
    let repaired = VerifiedCommit {
        id: "repair-unit".into(),
        unit_id: "repair-unit".into(),
        base_commit: approved.sha.clone(),
        tree: run(root, &["rev-parse", &format!("{repaired_sha}^{{tree}}")])?,
        sha: repaired_sha,
        ..approved.clone()
    };
    run(root, &["switch", "--quiet", "integration"])?;
    fs::write(root.join("other.txt"), "new destination file\n")?;
    let later_destination = commit(root, "Destination advances during repair")?;
    run(root, &["switch", "--quiet", "main"])?;
    fs::write(root.join("base.txt"), "leave this dirty\n")?;
    let refs = run(root, &["show-ref"])?;
    let index = fs::read(root.join(".git/index"))?;

    let sync_once = || {
        source_sync(repo.prepare_source_sync(
            &original,
            approved,
            SourceSyncKind::Destination,
            destination,
        )?)
    };
    let first = sync_once()?;
    fs::create_dir_all(root.join(".git/info"))?;
    fs::write(root.join(".git/info/attributes"), "* merge=ours\n")?;
    assert_eq!(sync_once()?, first);
    assert_eq!(
        run(root, &["show", "-s", "--format=%P", &first.head_sha])?,
        format!("{} {destination}", approved.sha)
    );
    let prior = [(&first, approved.clone())];
    let previous = repo.verify_source_syncs(original_base, &repaired, &prior)?;
    assert_eq!(previous.accepted_id, approved.id);
    assert_eq!(previous.sha, first.head_sha);
    assert!(
        repo.prepare_source_sync(
            &previous,
            approved,
            SourceSyncKind::Destination,
            original_base
        )
        .is_err()
    );
    assert!(
        repo.prepare_source_sync(
            &previous,
            approved,
            SourceSyncKind::Destination,
            &approved.sha
        )
        .is_err()
    );
    for tampered in [
        SourceSync {
            head_sha: merge_commit(root, &first.tree_sha, destination, &approved.sha)?,
            ..first.clone()
        },
        SourceSync {
            head_sha: merge_commit(root, &approved.tree, &approved.sha, destination)?,
            tree_sha: approved.tree.clone(),
            ..first.clone()
        },
        SourceSync {
            previous_head: commits[0].sha.clone(),
            ..first.clone()
        },
    ] {
        assert!(
            repo.verify_source_syncs(original_base, &repaired, &[(&tampered, approved.clone())])
                .is_err()
        );
    }
    assert!(
        repo.verify_source_syncs(original_base, &commits[0], &prior)
            .is_err()
    );

    let repair = source_sync(repo.prepare_source_sync(
        &previous,
        &repaired,
        SourceSyncKind::AcceptedRevision,
        &repaired.sha,
    )?)?;
    assert_eq!(
        run(root, &["show", "-s", "--format=%P", &repair.head_sha])?,
        format!("{} {}", first.head_sha, repaired.sha)
    );
    let after_repair = [(&first, approved.clone()), (&repair, repaired.clone())];
    let repaired_head = repo.verify_source_syncs(original_base, &repaired, &after_repair)?;
    assert!(
        repo.verify_synced_merge_base(&repaired_head, &later_destination)
            .is_err()
    );
    let latest = source_sync(repo.prepare_source_sync(
        &repaired_head,
        &repaired,
        SourceSyncKind::Destination,
        &later_destination,
    )?)?;
    let chain = [
        (&first, approved.clone()),
        (&repair, repaired.clone()),
        (&latest, repaired.clone()),
    ];
    let effective = repo.verify_source_syncs(original_base, &repaired, &chain)?;
    repo.verify_synced_merge_base(&effective, &later_destination)?;
    assert_eq!(effective.accepted_id, repaired.id);
    assert_eq!(effective.sha, latest.head_sha);
    assert_eq!(
        repo.github_effective_paths(&later_destination, &effective.sha)?,
        ["first.txt", "repair.txt", "second.txt"],
    );
    let merged = merge_commit(root, &latest.tree_sha, &later_destination, &effective.sha)?;
    let proof = RemoteMergeProof {
        pull_number: 3,
        head_sha: effective.sha,
        base_sha: later_destination.clone(),
        merge_sha: merged.clone(),
        tree_sha: latest.tree_sha,
        destination_sha: merged,
    };
    let repaired_request = DeliveryMergeRequest {
        expected_source_sha: repaired.sha.clone(),
        expected_destination_sha: later_destination,
        ..request.clone()
    };
    repo.verify_github_integration(
        &repaired_request,
        &proof,
        &[commits[0].clone(), approved.clone(), repaired],
    )?;
    assert_eq!(run(root, &["show-ref"])?, refs);
    assert_eq!(fs::read(root.join(".git/index"))?, index);
    assert_eq!(
        fs::read_to_string(root.join("base.txt"))?,
        "leave this dirty\n"
    );
    Ok(())
}

#[test]
fn github_merge_requires_the_tested_tree_and_exact_parents_in_the_destination() -> Result<()> {
    let fixture = Fixture::new()?;
    let integration = fixture.prepare()?;
    let request = &fixture.request;
    let tree = run(
        &fixture.root,
        &["rev-parse", &format!("{integration}^{{tree}}")],
    )?;
    let proof = RemoteMergeProof {
        pull_number: 7,
        head_sha: request.expected_source_sha.clone(),
        base_sha: request.expected_destination_sha.clone(),
        merge_sha: integration.clone(),
        tree_sha: tree,
        destination_sha: integration,
    };

    let verify = |proof: &RemoteMergeProof| {
        fixture
            .repo
            .verify_github_integration(request, proof, &fixture.commits)
    };
    verify(&proof)?;
    assert_eq!(
        fixture.repo.delivery_paths(&fixture.commits)?,
        ["first.txt", "second.txt"]
    );
    assert!(
        verify(&RemoteMergeProof {
            head_sha: proof.base_sha.clone(),
            ..proof.clone()
        })
        .is_err()
    );
    assert!(
        verify(&RemoteMergeProof {
            tree_sha: fixture.commits[0].tree.clone(),
            ..proof.clone()
        })
        .is_err()
    );
    assert!(
        verify(&RemoteMergeProof {
            destination_sha: proof.base_sha.clone(),
            ..proof
        })
        .is_err()
    );
    assert_eq!(
        fixture.repo.branch_tip("integration")?,
        request.expected_destination_sha
    );
    Ok(())
}

fn commit(root: &Path, message: &str) -> Result<String> {
    run(root, &["add", "--", "."])?;
    run(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--no-gpg-sign",
            "--quiet",
            "-m",
            message,
        ],
    )?;
    run(root, &["rev-parse", "HEAD"])
}

fn run(root: &Path, args: &[&str]) -> Result<String> {
    Ok(git_query(&git_path()?, root, args)?.trim().into())
}
