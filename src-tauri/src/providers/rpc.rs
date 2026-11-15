use std::collections::HashMap;

use serde_json::{Value, json};

use super::{RequestId, WireMessage};

pub(crate) struct Rpc<T> {
    next: i64,
    pending: HashMap<RequestId, T>,
    protocol_version: bool,
}

impl<T> Rpc<T> {
    pub(crate) fn new(protocol_version: bool) -> Self {
        Self {
            next: 1,
            pending: HashMap::new(),
            protocol_version,
        }
    }

    pub(crate) fn request(
        &mut self,
        method: &str,
        params: Value,
        kind: T,
    ) -> anyhow::Result<WireMessage> {
        let (_, frame) = self.request_with_id(method, params, kind)?;
        Ok(frame)
    }

    pub(crate) fn request_with_id(
        &mut self,
        method: &str,
        params: Value,
        kind: T,
    ) -> anyhow::Result<(RequestId, WireMessage)> {
        let id = RequestId::Number(self.next);
        let frame = self.encode(json!({ "id": id, "method": method, "params": params }))?;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("JSON-RPC request IDs exhausted"))?;
        self.pending.insert(id.clone(), kind);
        Ok((id, frame))
    }

    pub(crate) fn notify(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<WireMessage> {
        let mut frame = json!({ "method": method });
        if let Some(params) = params {
            frame["params"] = params;
        }
        self.encode(frame)
    }

    pub(crate) fn result(&self, id: &RequestId, result: Value) -> anyhow::Result<WireMessage> {
        self.encode(json!({ "id": id, "result": result }))
    }

    pub(crate) fn error(
        &self,
        id: &RequestId,
        code: i32,
        message: &str,
    ) -> anyhow::Result<WireMessage> {
        self.encode(json!({ "id": id, "error": { "code": code, "message": message } }))
    }

    pub(crate) fn take_request(&mut self, id: &RequestId) -> Option<T> {
        self.pending.remove(id)
    }

    fn encode(&self, mut frame: Value) -> anyhow::Result<WireMessage> {
        if self.protocol_version {
            frame["jsonrpc"] = json!("2.0");
        }
        Ok(WireMessage::json(&frame)?)
    }
}

pub(crate) fn id(frame: &Value) -> Option<RequestId> {
    frame["id"].as_i64().map(RequestId::Number).or_else(|| {
        frame["id"]
            .as_str()
            .map(|id| RequestId::Text(id.to_owned()))
    })
}
