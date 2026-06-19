use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

/// Return REST/event-backed notification tool definitions.
pub fn notification_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "search_notification_recipients".into(),
            description: "Search users in the current organization who have configured notification handles. Use this before sending a notification when the prompt names a person ambiguously.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional handle or display-name query. Handles may include or omit the leading @."
                    },
                    "limit": {"type": "integer"}
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "list_notifications".into(),
            description: "List recent notification messages visible to you. Use limit and before for pagination.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer"},
                    "before": {
                        "type": "string",
                        "description": "Optional RFC3339 timestamp cursor. Returns notifications older than this timestamp."
                    }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "send_notification".into(),
            description: "Send a push notification from this agent. Use recipient_handle for a specific user, otherwise the notification is sent to the organization.".into(),
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
                    },
                    "recipient_handle": {
                        "type": "string",
                        "description": "Optional username or notification handle for a specific recipient, with or without the leading @."
                    }
                },
                "required": ["body"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
