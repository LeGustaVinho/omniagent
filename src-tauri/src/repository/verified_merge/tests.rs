use std::{fs, path::Path};

use anyhow::{Result, bail};

use crate::repository::{git_path, git_query, inspect, verified_git::CommitSource};

use super::{DeliveryMergeRequest, MergeRepository, Plan, Publication, VerifiedCommit};

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
    assert_eq!(
        fixture.repo.branch_tip("integration")?,
        fixture.request.expected_destination_sha
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
