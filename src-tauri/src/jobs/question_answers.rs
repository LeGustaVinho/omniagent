use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    conversations::{
        AttemptPlan, LinkedQuestionInput, RecordedQuestionAnswer, SessionScope,
        close_saved_question, insert_linked_question_job_attempt, load_conversation,
        save_question_event, validate_saved_question,
    },
    protection,
    providers::{
        ProviderEvent, ProviderEventData,
        questions::{QuestionAction, QuestionOrigin, QuestionReply},
    },
    store::Store,
};

use super::{
    Job, QueueState, change_pending_job_state, pipeline::load_attempt_job,
    question_waits::saved_checkpoint_is_current, subagents::SubagentAllocation,
};

pub(super) struct QuestionContinuation {
    pub run: String,
    pub conversation: String,
    pub context: String,
}

pub(super) fn load_continuation(
    db: &Connection,
    job: &Job,
) -> Result<Option<QuestionContinuation>> {
    Ok(db.query_row(
        "SELECT run_id, conversation_id, context_id FROM ready_question_continuations WHERE job_id = ?1",
        [&job.id],
        |row| Ok(QuestionContinuation {
            run: row.get(0)?,
            conversation: row.get(1)?,
            context: row.get(2)?,
        }),
    ).optional()?)
}

pub(super) fn current(
    db: &Connection,
    continuation: &QuestionContinuation,
    scope: Option<&SessionScope>,
) -> Result<bool> {
    let Some(scope) = scope else {
        return Ok(false);
    };
    let previous_scope: String = db.query_row(
        "SELECT s.scope FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
         WHERE a.run_id = ?1",
        [&continuation.run],
        |row| row.get(0),
    )?;
    if serde_json::to_string(scope)? != previous_scope {
        return Ok(false);
    }
    let mut questions =
        db.prepare("SELECT request_event_seq FROM open_agent_questions WHERE run_id = ?1")?;
    for seq in questions.query_map([&continuation.run], |row| row.get::<_, i64>(0))? {
        if !saved_checkpoint_is_current(db, &continuation.run, seq?)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn insert_attempt(
    db: &Connection,
    continuation: &QuestionContinuation,
    scope: SessionScope,
    downstream: Option<String>,
    instruction_revision: Option<String>,
    allocation: SubagentAllocation,
) -> Result<AttemptPlan> {
    let mut statement = db.prepare(
        "SELECT q.request_event_seq, answer.payload FROM open_agent_questions q
         JOIN recorded_question_answers answer ON answer.request_event_seq = q.request_event_seq AND answer.accepted = 1
         WHERE q.run_id = ?1 ORDER BY q.request_event_seq",
    )?;
    let rows = statement.query_map([&continuation.run], |row| {
        Ok((row.get(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    let mut answers = Vec::new();
    let mut route = None;
    for row in rows {
        let (seq, payload) = row?;
        let answer: RecordedQuestionAnswer =
            serde_json::from_slice(&protection::unprotect(&payload)?)?;
        let selected = validate_saved_question(
            db,
            &continuation.conversation,
            &answer.origin,
            &answer.reply,
        )?;
        ensure!(
            selected.seq == seq
                && selected.context == continuation.context
                && selected.answer == answer
                && answer.origin.run_id == continuation.run
                && answer.reply.action == QuestionAction::Accept,
            "The recorded answer no longer matches the originating question"
        );
        ensure!(
            route.is_none_or(|route| route == selected.route),
            "Question routes differ within the saved turn"
        );
        route = Some(selected.route);
        answers.push((seq, answer));
    }
    let conversation = load_conversation(db, &continuation.conversation)?;
    let input = LinkedQuestionInput {
        context: continuation.context.clone(),
        route: route.context("Missing question route")?,
        answers,
        earlier_replies: earlier_replies(db, &continuation.run)?,
        downstream,
        instruction_revision,
    };
    insert_linked_question_job_attempt(db, conversation, scope, input, allocation)
}

fn earlier_replies(db: &Connection, run: &str) -> Result<Vec<RecordedQuestionAnswer>> {
    let mut statement = db.prepare(
        "SELECT question.payload, reply.payload, recorded.payload
         FROM agent_questions q JOIN agent_events question ON question.seq = q.request_event_seq
         LEFT JOIN agent_events reply ON reply.seq = q.reply_event_seq AND q.delivery = 'submitted'
         LEFT JOIN recorded_question_answers recorded ON recorded.request_event_seq = q.request_event_seq AND recorded.accepted = 0
         WHERE q.run_id = ?1 AND (reply.seq IS NOT NULL OR recorded.request_event_seq IS NOT NULL)
         ORDER BY q.request_event_seq",
    )?;
    let rows = statement.query_map([run], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, Option<Vec<u8>>>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (question, reply, recorded) = row?;
        if let Some(recorded) = recorded {
            let answer: RecordedQuestionAnswer =
                serde_json::from_slice(&protection::unprotect(&recorded)?)?;
            ensure!(
                answer.reply.action != QuestionAction::Accept,
                "A dismissed question has an accepted answer"
            );
            return Ok(answer);
        }
        let question: ProviderEvent = serde_json::from_slice(&protection::unprotect(&question)?)?;
        let reply: ProviderEvent = serde_json::from_slice(&protection::unprotect(
            &reply.context("Missing submitted reply")?,
        )?)?;
        let ProviderEventData::Question {
            request_id,
            form: schema,
            normalized: Some(form),
            ..
        } = question.data
        else {
            anyhow::bail!("Missing submitted question form")
        };
        let ProviderEventData::QuestionReplyPrepared {
            request_id: replied_to,
            reply,
            ..
        } = reply.data
        else {
            anyhow::bail!("Missing submitted question reply")
        };
        ensure!(
            request_id == replied_to,
            "Submitted reply belongs to another question"
        );
        form.validate(&reply)?;
        Ok(RecordedQuestionAnswer {
            origin: QuestionOrigin {
                run_id: run.into(),
                request_id,
            },
            form,
            schema,
            reply,
        })
    })
    .collect()
}

impl Store {
    pub async fn answer_saved_question(
        &self,
        conversation: String,
        origin: QuestionOrigin,
        reply: QuestionReply,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let job = load_attempt_job(&tx, &origin.run_id)?.context("The question has no scheduled job")?;
            ensure!(
                job.queue_state != QueueState::Cancelled
                    && job.latest_attempt.as_ref().is_some_and(|attempt| attempt.run_id == origin.run_id && attempt.status.is_terminal())
                    && job.queued_recovery.is_none(),
                "The originating attempt is no longer waiting for an answer"
            );
            let selected = validate_saved_question(&tx, &conversation, &origin, &reply)?;
            let prior: Option<Vec<u8>> = tx.query_row(
                "SELECT payload FROM recorded_question_answers WHERE request_event_seq = ?1",
                [selected.seq],
                |row| row.get(0),
            ).optional()?;
            if let Some(prior) = prior {
                let prior: RecordedQuestionAnswer = serde_json::from_slice(&protection::unprotect(&prior)?)?;
                ensure!(prior == selected.answer, "The question already has a recorded answer");
            } else {
                let payload = protection::protect(&serde_json::to_vec(&selected.answer)?)?;
                tx.execute(
                    "INSERT INTO recorded_question_answers(request_event_seq, accepted, payload) VALUES (?1, ?2, ?3)",
                    params![selected.seq, reply.action == QuestionAction::Accept, payload],
                )?;
                if reply.action != QuestionAction::Accept {
                    save_question_event(&tx, &origin.run_id, ProviderEvent {
                        location: Default::default(),
                        data: ProviderEventData::StructuredQuestionDismissed { origin: origin.clone(), action: reply.action },
                    })?;
                    let remaining: u32 = tx.query_row(
                        "SELECT count(*) FROM open_agent_questions WHERE run_id = ?1", [&origin.run_id], |r| r.get(0),
                    )?;
                    if remaining == 1 {
                        change_pending_job_state(&tx, &job.project_id, &job.id, QueueState::Cancelled)?;
                    }
                    close_saved_question(&tx, selected.seq)?;
                }
            }
            tx.commit()?;
            Ok(())
        }).await?;
        self.job_signal.send_replace(());
        Ok(())
    }
}
