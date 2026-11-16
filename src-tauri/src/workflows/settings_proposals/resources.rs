use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{
    RevisionedSetting, SettingsAppliedRevision, SettingsAutoGrant, SettingsChange, SettingsField,
    SettingsFieldDiff, SettingsPatchAllowlist, TemplateDisplayLabel, decode, encode, next_revision,
    valid_id,
};

const DEFAULT_LIVE_QUESTION_TIMEOUT: u32 = 10;
pub(super) const MAX_GRANTS: usize = 16;

pub(super) fn check_minutes(minutes: u32) -> Result<()> {
    ensure!(
        (1..=120).contains(&minutes),
        "Live question timeout must be between 1 and 120 minutes"
    );
    Ok(())
}

pub(super) fn check_label(label: Option<&str>) -> Result<()> {
    if let Some(label) = label {
        ensure!(
            !label.trim().is_empty() && label.len() <= 120 && !label.chars().any(char::is_control),
            "The template display label must contain 1 to 120 UTF-8 bytes"
        );
    }
    Ok(())
}

pub(crate) fn read_question_timeout_setting(
    db: &Connection,
    project: &str,
) -> Result<RevisionedSetting<u32>> {
    Ok(db.query_row(
        "SELECT coalesce(timeout.minutes, ?2), coalesce(revision.revision, 0)
         FROM projects project
         LEFT JOIN project_question_timeouts timeout ON timeout.project_id = project.id
         LEFT JOIN workflow_settings_timeout_revisions revision ON revision.project_id = project.id
         WHERE project.id = ?1",
        params![project, DEFAULT_LIVE_QUESTION_TIMEOUT],
        |row| {
            Ok(RevisionedSetting {
                value: row.get(0)?,
                revision: row.get(1)?,
            })
        },
    )?)
}

pub(crate) fn write_question_timeout_setting(
    db: &Transaction<'_>,
    project: &str,
    expected_revision: Option<i64>,
    minutes: u32,
) -> Result<RevisionedSetting<u32>> {
    check_minutes(minutes)?;
    let current = read_question_timeout_setting(db, project)?;
    ensure!(
        expected_revision.is_none_or(|expected| current.revision == expected),
        "The live question timeout changed. Review its current revision."
    );
    if current.value == minutes {
        return Ok(current);
    }
    let revision = next_revision(current.revision)?;
    db.execute(
        "INSERT INTO project_question_timeouts(project_id, minutes) VALUES (?1, ?2)
         ON CONFLICT(project_id) DO UPDATE SET minutes = excluded.minutes",
        params![project, minutes],
    )?;
    db.execute(
        "INSERT INTO workflow_settings_timeout_revisions(project_id, revision) VALUES (?1, ?2)
         ON CONFLICT(project_id) DO UPDATE SET revision = excluded.revision",
        params![project, revision],
    )?;
    Ok(RevisionedSetting {
        value: minutes,
        revision,
    })
}

pub(super) fn read_template_label(
    db: &Connection,
    project: &str,
    template_id: &str,
) -> Result<TemplateDisplayLabel> {
    let (template_revision, value, revision): (String, Option<Vec<u8>>, i64) = db.query_row(
        "SELECT template.head_revision, presentation.label, coalesce(presentation.revision, 0)
         FROM conversation_templates template
         LEFT JOIN workflow_template_display_labels presentation
            ON presentation.project_id = template.project_id AND presentation.template_id = template.id
         WHERE template.project_id = ?1 AND template.id = ?2",
        params![project, template_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(TemplateDisplayLabel {
        template_id: template_id.into(),
        template_revision,
        display_label: RevisionedSetting {
            value: value.as_deref().map(decode).transpose()?.flatten(),
            revision,
        },
    })
}

pub(super) fn write_template_label(
    db: &Transaction<'_>,
    project: &str,
    template_id: &str,
    template_revision: &str,
    expected_revision: i64,
    label: Option<String>,
) -> Result<TemplateDisplayLabel> {
    check_label(label.as_deref())?;
    let current = read_template_label(db, project, template_id)?;
    ensure!(
        current.template_revision == template_revision
            && current.display_label.revision == expected_revision,
        "The template or its display label changed. Review the current revisions."
    );
    if current.display_label.value == label {
        return Ok(current);
    }
    let revision = next_revision(current.display_label.revision)?;
    db.execute(
        "INSERT INTO workflow_template_display_labels(project_id, template_id, revision, label)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(project_id, template_id) DO UPDATE SET revision = excluded.revision, label = excluded.label",
        params![project, template_id, revision, encode(&label)?],
    )?;
    Ok(TemplateDisplayLabel {
        display_label: RevisionedSetting {
            value: label,
            revision,
        },
        ..current
    })
}

pub(super) fn review_diff(
    db: &Connection,
    project: &str,
    change: SettingsChange,
) -> Result<Option<SettingsFieldDiff>> {
    match change {
        SettingsChange::LiveQuestionTimeout {
            expected_revision,
            minutes,
        } => {
            check_minutes(minutes)?;
            let current = read_question_timeout_setting(db, project)?;
            ensure!(
                current.revision == expected_revision,
                "The live question timeout changed. Review its current revision."
            );
            Ok(
                (current.value != minutes).then_some(SettingsFieldDiff::LiveQuestionTimeout {
                    before: current,
                    after: minutes,
                }),
            )
        }
        SettingsChange::TemplateDisplayLabel {
            template_id,
            template_revision,
            expected_revision,
            label,
        } => {
            check_label(label.as_deref())?;
            let current = read_template_label(db, project, &template_id)?;
            ensure!(
                current.template_revision == template_revision
                    && current.display_label.revision == expected_revision,
                "The template or its display label changed. Review the current revisions."
            );
            Ok((current.display_label.value != label).then_some(
                SettingsFieldDiff::TemplateDisplayLabel {
                    template_id,
                    template_revision,
                    before: current.display_label,
                    after: label,
                },
            ))
        }
    }
}

pub(super) fn apply_diff(
    db: &Transaction<'_>,
    project: &str,
    diff: &SettingsFieldDiff,
) -> Result<SettingsAppliedRevision> {
    let (before_revision, after_revision) = match diff {
        SettingsFieldDiff::LiveQuestionTimeout { before, after } => (
            before.revision,
            write_question_timeout_setting(db, project, Some(before.revision), *after)?.revision,
        ),
        SettingsFieldDiff::TemplateDisplayLabel {
            template_id,
            template_revision,
            before,
            after,
        } => (
            before.revision,
            write_template_label(
                db,
                project,
                template_id,
                template_revision,
                before.revision,
                after.clone(),
            )?
            .display_label
            .revision,
        ),
    };
    Ok(SettingsAppliedRevision {
        field: diff.field(),
        template_id: diff.template_id().map(str::to_owned),
        before_revision,
        after_revision,
    })
}

pub(super) fn read_allowlist(db: &Connection, project: &str) -> Result<SettingsPatchAllowlist> {
    let (revision, grants): (i64, Option<Vec<u8>>) = db.query_row(
        "SELECT coalesce(policy.revision, 0), policy.grants
         FROM projects project LEFT JOIN workflow_settings_allowlist_revisions policy
            ON policy.project_id = project.id
         WHERE project.id = ?1 ORDER BY policy.revision DESC LIMIT 1",
        [project],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(SettingsPatchAllowlist {
        revision,
        grants: grants
            .as_deref()
            .map(decode)
            .transpose()?
            .unwrap_or_default(),
    })
}

pub(super) fn write_allowlist(
    db: &Transaction<'_>,
    project: &str,
    expected_revision: i64,
    grants: BTreeSet<SettingsAutoGrant>,
) -> Result<SettingsPatchAllowlist> {
    ensure!(
        grants.len() <= MAX_GRANTS,
        "The scope has too many automatic grants"
    );
    let current = read_allowlist(db, project)?;
    ensure!(
        current.revision == expected_revision,
        "The allowlist changed. Review its current revision."
    );
    for grant in &grants {
        if let Some(checkout_id) = &grant.checkout_id {
            valid_id(checkout_id)?;
            let exists: Option<i64> = db
                .query_row(
                    "SELECT 1 FROM checkouts WHERE project_id = ?1 AND id = ?2",
                    params![project, checkout_id],
                    |row| row.get(0),
                )
                .optional()?;
            ensure!(exists.is_some(), "The checkout is outside this project");
        }
        if let Some(template_id) = &grant.template_id {
            valid_id(template_id)?;
            ensure!(
                grant.field == SettingsField::TemplateDisplayLabel,
                "This field is outside the template scope"
            );
            read_template_label(db, project, template_id)?;
        }
    }
    if current.grants == grants {
        return Ok(current);
    }
    let revision = next_revision(current.revision)?;
    db.execute(
        "INSERT INTO workflow_settings_allowlist_revisions(project_id, revision, grants) VALUES (?1, ?2, ?3)",
        params![project, revision, encode(&grants)?],
    )?;
    Ok(SettingsPatchAllowlist { revision, grants })
}

pub(super) fn permits(
    allowlist: &SettingsPatchAllowlist,
    checkout: &str,
    diff: &SettingsFieldDiff,
) -> bool {
    allowlist.grants.iter().any(|grant| {
        grant.field == diff.field()
            && grant
                .checkout_id
                .as_deref()
                .is_none_or(|scope| scope == checkout)
            && grant
                .template_id
                .as_deref()
                .is_none_or(|scope| Some(scope) == diff.template_id())
    })
}
