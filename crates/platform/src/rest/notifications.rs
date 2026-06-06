use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

fn session_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "format": "uuid",
        "description": "The notification session id."
    })
}

/// Return REST/event-backed notification tool definitions.
pub fn notification_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_notification_sessions".into(),
            description: "List notification sessions visible to you. Use this before reading notification messages when you do not know a session id.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer"},
                    "offset": {"type": "integer"}
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "list_notifications".into(),
            description: "List messages in one notification session.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "session_id": session_id_schema(),
                    "limit": {"type": "integer"},
                    "before": {
                        "type": "string",
                        "description": "Optional RFC3339 timestamp cursor. Returns notifications older than this timestamp."
                    }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "send_notification".into(),
            description: "Send an org-scoped encrypted push notification from this agent. Use this only for short user-facing status, completion, or action-needed notifications.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "body": {
                        "type": "string",
                        "description": "Notification body shown to users after local decryption."
                    },
                    "tag": {
                        "type": "string",
                        "description": "Optional collapse tag so newer notifications can replace older ones."
                    }
                },
                "required": ["body"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
