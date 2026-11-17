use anyhow::{Result, ensure};

use crate::repository::verified_merge::{GithubSyncOutcome, SourceSync, SourceSyncKind};

use super::{
    IsolatedGit, MergeRepository, VerifiedCommit, check, is_ancestor, merge_tree, read_commit,
    text_oid,
};

pub(in crate::repository::verified_merge) struct SourceSyncHead {
    pub sha: String,
    pub accepted_id: String,
    accepted_sha: String,
    destination: String,
}

impl MergeRepository {
    pub(in crate::repository::verified_merge) fn verify_source_syncs(
        &self,
        original_base: &str,
        final_acceptance: &VerifiedCommit,
        steps: &[(&SourceSync, VerifiedCommit)],
    ) -> Result<SourceSyncHead> {
        let view = self.merge_objects(original_base)?;
        let accepted = steps
            .first()
            .map_or(final_acceptance, |(_, accepted)| accepted);
        ensure!(
            is_ancestor(&view, original_base, &accepted.sha)?,
            "The approved source does not retain its registered worktree base"
        );
        let mut head = SourceSyncHead {
            sha: accepted.sha.clone(),
            accepted_id: accepted.id.clone(),
            accepted_sha: accepted.sha.clone(),
            destination: original_base.into(),
        };
        for (step, accepted) in steps {
            let recorded = read_commit(&view, &accepted.sha)?;
            ensure!(
                recorded.tree == accepted.tree
                    && recorded.parents == [accepted.base_commit.as_str()]
                    && is_ancestor(&view, &accepted.sha, &final_acceptance.sha)?,
                "The source sync cites a commit outside the retained approved history"
            );
            let rebuilt = prepare(&view, &head, accepted, step.kind, &step.second_parent_sha)?;
            ensure!(
                matches!(rebuilt, GithubSyncOutcome::Ready(proof) if &proof == *step),
                "The saved source synchronization does not match the deterministic Git proof"
            );
            head.sha.clone_from(&step.head_sha);
            head.accepted_id.clone_from(&accepted.id);
            head.accepted_sha.clone_from(&accepted.sha);
            if step.kind == SourceSyncKind::Destination {
                head.destination.clone_from(&step.second_parent_sha);
            }
        }
        Ok(head)
    }

    pub(in crate::repository::verified_merge) fn prepare_source_sync(
        &self,
        head: &SourceSyncHead,
        accepted: &VerifiedCommit,
        kind: SourceSyncKind,
        second_parent: &str,
    ) -> Result<GithubSyncOutcome> {
        prepare(
            &self.merge_objects(&head.sha)?,
            head,
            accepted,
            kind,
            second_parent,
        )
    }

    pub(in crate::repository::verified_merge) fn verify_synced_merge_base(
        &self,
        head: &SourceSyncHead,
        tested_base: &str,
    ) -> Result<()> {
        let view = self.merge_objects(&head.sha)?;
        ensure!(
            is_ancestor(&view, &head.destination, tested_base)?
                && is_ancestor(&view, tested_base, &head.sha)?,
            "The effective source does not retain the tested destination base and its synchronized history"
        );
        Ok(())
    }
}

fn prepare(
    view: &IsolatedGit,
    head: &SourceSyncHead,
    accepted: &VerifiedCommit,
    kind: SourceSyncKind,
    second_parent: &str,
) -> Result<GithubSyncOutcome> {
    let message = match kind {
        SourceSyncKind::Destination => {
            ensure!(
                head.accepted_id == accepted.id
                    && is_ancestor(view, &head.destination, second_parent)?,
                "The destination changed outside the trusted delivery history"
            );
            if is_ancestor(view, second_parent, &head.sha)? {
                return Ok(GithubSyncOutcome::Unchanged);
            }
            "OmniAgent verified source synchronization"
        }
        SourceSyncKind::AcceptedRevision => {
            ensure!(
                head.accepted_id != accepted.id
                    && head.accepted_sha != accepted.sha
                    && head.sha != second_parent
                    && second_parent == accepted.sha
                    && is_ancestor(view, &head.accepted_sha, &accepted.sha)?,
                "The repaired source must retain the prior approved commit and attach its new verified head"
            );
            "OmniAgent verified repair source reattachment"
        }
    };
    let Some(tree_sha) = merge_tree(view, &head.sha, second_parent)? else {
        return Ok(GithubSyncOutcome::Conflict);
    };
    let head_sha = text_oid(check(
        view.command()
            .env("GIT_AUTHOR_NAME", "OmniAgent")
            .env("GIT_AUTHOR_EMAIL", "automation@omniagent.invalid")
            .env("GIT_AUTHOR_DATE", &accepted.verified_at)
            .env("GIT_COMMITTER_NAME", "OmniAgent")
            .env("GIT_COMMITTER_EMAIL", "automation@omniagent.invalid")
            .env("GIT_COMMITTER_DATE", &accepted.verified_at)
            .args([
                "commit-tree",
                "--no-gpg-sign",
                &tree_sha,
                "-p",
                &head.sha,
                "-p",
                second_parent,
                "-m",
                message,
            ])
            .output()?,
        "write the deterministic delivery synchronization commit",
    )?)?;
    let recorded = read_commit(view, &head_sha)?;
    ensure!(
        recorded.tree == tree_sha && recorded.parents == [head.sha.as_str(), second_parent],
        "The synchronization object does not match its pinned parents and tree"
    );
    Ok(GithubSyncOutcome::Ready(SourceSync {
        kind,
        accepted_commit_evidence_id: accepted.id.clone(),
        previous_head: head.sha.clone(),
        second_parent_sha: second_parent.into(),
        head_sha,
        tree_sha,
    }))
}
