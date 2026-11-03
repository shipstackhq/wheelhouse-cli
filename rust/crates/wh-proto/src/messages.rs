//! Messages the server sends over the event stream (`job.offer`, `job.cancel`, `settings.update`, `chat.message`, `chat.cancel`).
//! Field names follow `daemon/src/supervisor.mjs`. Unknown fields are ignored here, but signatures are checked over the raw JSON
//! value (see `verify_signed`), never over these structs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    pub id: Option<String>,
    pub clone_url: Option<String>,
    pub full_name: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct IssueRef {
    #[serde(rename = "ref")]
    pub issue_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub job_id: String,
    pub device_id: String,
    pub expires_at: String,
    pub repository: Option<Repository>,
    pub issue: Option<IssueRef>,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub push: Option<bool>,
    pub start_from: Option<String>,
    pub base_branch: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct McpGrant {
    pub token: Option<String>,
    pub write: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub message_id: Option<String>,
    pub device_id: String,
    pub thread_id: Option<String>,
    pub model: Option<String>,
    pub prompt: Option<String>,
    pub session_id: Option<String>,
    pub expires_at: Option<String>,
    pub repository: Option<Repository>,
    pub mcp: Option<McpGrant>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub device_id: Option<String>,
    pub pool_size: Option<i64>,
    pub chat_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobCancel {
    pub device_id: String,
    pub job_id: String,
    pub superseded: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatCancel {
    pub device_id: String,
    pub message_id: String,
}

/// A server message, told apart by its `type` field (which must equal the SSE event name).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "job.offer")]
    JobOffer(Offer),
    #[serde(rename = "job.cancel")]
    JobCancel(JobCancel),
    #[serde(rename = "settings.update")]
    SettingsUpdate(Settings),
    #[serde(rename = "chat.message")]
    ChatMessage(ChatMessage),
    #[serde(rename = "chat.cancel")]
    ChatCancel(ChatCancel),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_type() {
        let o: ServerMessage = serde_json::from_str(
            r#"{"type":"job.offer","jobId":"j1","deviceId":"d","expiresAt":"2030-01-01T00:00:00Z","branch":"b","extra":1,
                "repository":{"id":"r","cloneUrl":"u"},"issue":{"ref":"WH-1"},"push":false}"#,
        )
        .unwrap();
        match o {
            ServerMessage::JobOffer(o) => {
                assert_eq!(o.job_id, "j1");
                assert_eq!(o.push, Some(false));
                assert_eq!(o.issue.unwrap().issue_ref.as_deref(), Some("WH-1"));
            }
            _ => panic!("wrong variant"),
        }
        let s: ServerMessage = serde_json::from_str(r#"{"type":"settings.update","poolSize":3,"chatModel":null}"#).unwrap();
        assert!(matches!(s, ServerMessage::SettingsUpdate(Settings { pool_size: Some(3), .. })));
        let c: ServerMessage =
            serde_json::from_str(r#"{"type":"chat.message","deviceId":"d","messageId":"m","prompt":"hi","mcp":{"write":true}}"#).unwrap();
        assert!(matches!(c, ServerMessage::ChatMessage(_)));
        assert!(serde_json::from_str::<ServerMessage>(r#"{"type":"nope"}"#).is_err());
    }
}
