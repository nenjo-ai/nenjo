//! Provider execution helpers shared by harness chat and task flows.

use nenjo::Manifest;
use uuid::Uuid;

use crate::preview::truncate_preview;

/// Resolve a project slug from a manifest, falling back to the project UUID.
pub(crate) fn project_slug(manifest: &Manifest, project_id: Uuid) -> String {
    if project_id.is_nil() {
        return String::new();
    }

    manifest
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| project.slug.clone())
        .unwrap_or_else(|| project_id.to_string())
}

/// Resolve an agent name from a manifest, falling back to the agent UUID.
pub(crate) fn agent_name(manifest: &Manifest, agent_id: Uuid) -> String {
    manifest
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .map(|agent| agent.name.clone())
        .unwrap_or_else(|| agent_id.to_string())
}

/// Summarize a turn event for trace/session metadata.
pub(crate) fn summarize_turn_event(event: &nenjo::TurnEvent) -> String {
    match event {
        nenjo::TurnEvent::AbilityStarted {
            ability_tool_name,
            ability_name,
            task_input,
            caller_history,
        } => format!(
            "ability_started(tool={ability_tool_name}, ability={ability_name}, task_preview={:?}, task_len={}, caller_messages={})",
            truncate_preview(task_input, 80),
            task_input.len(),
            caller_history.len()
        ),
        nenjo::TurnEvent::ToolCallStart {
            parent_tool_name,
            calls,
        } => format!(
            "tool_call_start(parent={}, tools=[{}], count={})",
            parent_tool_name.as_deref().unwrap_or("-"),
            calls
                .iter()
                .map(|call| call.tool_name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            calls.len()
        ),
        nenjo::TurnEvent::ToolCallEnd {
            parent_tool_name,
            tool_name,
            tool_args,
            result,
            ..
        } => format!(
            "tool_call_end(parent={}, tool={tool_name}, args_len={}, success={}, output_len={}, error={})",
            parent_tool_name.as_deref().unwrap_or("-"),
            tool_args.len(),
            result.success,
            result.output.len(),
            result
                .error
                .as_deref()
                .map(|err| truncate_preview(err, 80))
                .unwrap_or_else(|| "-".to_string())
        ),
        nenjo::TurnEvent::AbilityCompleted {
            ability_tool_name,
            ability_name,
            success,
            final_output,
        } => format!(
            "ability_completed(tool={ability_tool_name}, ability={ability_name}, success={success}, output_len={})",
            final_output.len()
        ),
        nenjo::TurnEvent::SubAgentEvent {
            slug,
            agent_name,
            kind,
            summary,
            ..
        } => format!(
            "sub_agent_event(slug={slug}, agent={agent_name}, kind={kind}, summary_len={})",
            summary.len()
        ),
        nenjo::TurnEvent::SubAgentTranscript {
            slug,
            agent_name,
            event,
        } => format!(
            "sub_agent_transcript(slug={slug}, agent={agent_name}, kind={}, summary_len={})",
            event.kind(),
            event.summary().len()
        ),
        nenjo::TurnEvent::MessageCompacted {
            messages_before,
            messages_after,
        } => format!("message_compacted({messages_before}->{messages_after})"),
        nenjo::TurnEvent::TranscriptMessage { message } => format!(
            "transcript_message(role={}, content_len={})",
            message.role,
            message.content.len()
        ),
        nenjo::TurnEvent::Paused => "paused".to_string(),
        nenjo::TurnEvent::Resumed => "resumed".to_string(),
        nenjo::TurnEvent::Done { output } => format!(
            "done(text_len={}, input_tokens={}, output_tokens={}, tool_calls={}, messages={})",
            output.text.len(),
            output.input_tokens,
            output.output_tokens,
            output.tool_calls,
            output.messages.len()
        ),
    }
}
