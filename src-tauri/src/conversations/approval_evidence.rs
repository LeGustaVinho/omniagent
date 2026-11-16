use anyhow::{Result, bail};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{
    protection,
    providers::{ApprovalDecision, ProviderEvent, ProviderEventData, ProviderLocation},
    store::Store,
};

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDelivery {
    Pending,
    Prepared,
    Submitted,
    Unconfirmed,
    Closed,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationApprovalEvidence {
    pub event_sequence: String,
    pub operation: String,
    pub description: Option<String>,
    pub location: ProviderLocation,
    pub decision: Option<ApprovalDecision>,
    pub delivery: ApprovalDelivery,
}

impl Store {
    pub async fn conversation_approvals(
        &self,
        conversation_id: String,
        run_id: String,
    ) -> Result<Vec<ConversationApprovalEvidence>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT e.seq, e.payload, json_quote(p.decision), json_quote(p.delivery)
                 FROM agent_permissions p JOIN agent_events e ON e.seq = p.request_event_seq
                    AND e.run_id = p.run_id AND e.source = 'provider'
                 JOIN agent_attempts a ON a.run_id = p.run_id AND a.turn_id = p.turn_id
                 WHERE a.conversation_id = ?1 AND a.run_id = ?2
                 ORDER BY e.seq",
            )?;
            let mut rows = query.query(params![conversation_id, run_id])?;
            let mut evidence = Vec::new();
            while let Some(row) = rows.next()? {
                let sequence: i64 = row.get(0)?;
                let payload: Vec<u8> = row.get(1)?;
                let decision: String = row.get(2)?;
                let delivery: String = row.get(3)?;
                let event: ProviderEvent =
                    serde_json::from_slice(&protection::unprotect(&payload)?)?;
                let ProviderEventData::Approval {
                    operation,
                    description,
                    ..
                } = event.data
                else {
                    bail!("Approval history references a different provider event");
                };
                evidence.push(ConversationApprovalEvidence {
                    event_sequence: sequence.to_string(),
                    operation,
                    description,
                    location: event.location,
                    decision: serde_json::from_str(&decision)?,
                    delivery: serde_json::from_str(&delivery)?,
                });
            }
            Ok(evidence)
        })
        .await
    }
}
