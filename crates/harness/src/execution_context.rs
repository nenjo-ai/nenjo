//! Provider execution helpers shared by harness chat and task flows.

use crate::preview::truncate_preview;

/// Convert an optional project slug into the memory namespace segment.
pub(crate) fn project_slug(project: Option<&nenjo::Slug>) -> String {
    project
        .map(|project| project.to_string())
        .unwrap_or_default()
}

/// Summarize a turn event for trace/session metadata.
pub(crate) fn summarize_turn_event(event: &nenjo::TurnEvent) -> String {
    match event {
        nenjo::TurnEvent::ModelRequestStarted {
            request_id, model, ..
        } => format!("model_request_started(request={request_id}, model={model})"),
        nenjo::TurnEvent::AssistantTextDelta { request_id, delta } => {
            format!(
                "assistant_text_delta(request={request_id}, len={})",
                delta.len()
            )
        }
        nenjo::TurnEvent::AssistantResponse { message, status } => {
            format!("assistant_response(status={status}, len={})", message.len())
        }
        nenjo::TurnEvent::ModelRequestCompleted { request_id, .. } => {
            format!("model_request_completed(request={request_id})")
        }
        nenjo::TurnEvent::AbilityStarted {
            call_id,
            ability_tool_name,
            ability_name,
            task_input,
            caller_history,
        } => format!(
            "ability_started(call={call_id}, tool={ability_tool_name}, ability={ability_name}, task_len={}, caller_messages={})",
            task_input.len(),
            caller_history.len()
        ),
        nenjo::TurnEvent::ToolCallStart {
            batch_id,
            parent_tool_name,
            calls,
        } => format!(
            "tool_call_start(batch={batch_id}, parent={}, tools=[{}], count={})",
            parent_tool_name.as_deref().unwrap_or("-"),
            calls
                .iter()
                .map(|call| call.tool_name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            calls.len()
        ),
        nenjo::TurnEvent::ToolCallEnd {
            batch_id,
            parent_tool_name,
            tool_name,
            tool_args,
            result,
            ..
        } => format!(
            "tool_call_end(batch={batch_id}, parent={}, tool={tool_name}, args_len={}, success={}, output_len={}, error={})",
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
            call_id,
            ability_tool_name,
            ability_name,
            success,
            final_output,
        } => format!(
            "ability_completed(call={call_id}, tool={ability_tool_name}, ability={ability_name}, success={success}, output_len={})",
            final_output.len()
        ),
        nenjo::TurnEvent::HookStarted {
            hook,
            hook_event,
            hook_type,
            source,
        } => format!(
            "hook_started(hook={hook}, event={hook_event}, type={hook_type}, source={source})"
        ),
        nenjo::TurnEvent::HookActivated {
            hook,
            hook_event,
            hook_type,
            source,
        } => format!(
            "hook_activated(hook={hook}, event={hook_event}, type={hook_type}, source={source})"
        ),
        nenjo::TurnEvent::HookCompleted {
            hook,
            hook_event,
            hook_type,
            source,
            success,
            blocked,
            exit_code,
            ..
        } => format!(
            "hook_completed(hook={hook}, event={hook_event}, type={hook_type}, source={source}, success={success}, blocked={blocked}, exit_code={})",
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_string())
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
        nenjo::TurnEvent::AsyncOperationEvent {
            operation_id,
            kind,
            signal,
            status,
            ..
        } => format!(
            "async_operation_event(id={operation_id}, kind={kind}, signal={signal}, status={status})"
        ),
        nenjo::TurnEvent::AsyncOperationTranscript {
            operation_id,
            kind,
            event,
            ..
        } => format!(
            "async_operation_transcript(id={operation_id}, kind={kind}, event={}, summary_len={})",
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
