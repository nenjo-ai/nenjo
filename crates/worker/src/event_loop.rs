use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use dashmap::DashMap;
use nenjo_events::{Response, StreamEvent};
use nenjo_secure_envelope::{DecodingError, ReceivedInput, SecureEnvelopeBus};
use serde_json::json;
use thiserror::Error;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::runtime::WorkerRuntime;

#[derive(Debug, Clone)]
pub(crate) struct RoutedResponse {
    target: ResponseTarget,
    response: Response,
}

#[derive(Debug, Clone, Copy)]
enum ResponseTarget {
    Actor(Uuid),
    System { org_id: Uuid },
}

#[derive(Clone)]
pub struct ResponseSender {
    tx: tokio::sync::mpsc::UnboundedSender<RoutedResponse>,
    target: ResponseTarget,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("response channel closed")]
pub struct ResponseSenderError;

impl ResponseSender {
    fn for_actor(
        tx: tokio::sync::mpsc::UnboundedSender<RoutedResponse>,
        actor_user_id: Uuid,
    ) -> Self {
        Self {
            tx,
            target: ResponseTarget::Actor(actor_user_id),
        }
    }

    fn system(tx: tokio::sync::mpsc::UnboundedSender<RoutedResponse>, org_id: Uuid) -> Self {
        Self {
            tx,
            target: ResponseTarget::System { org_id },
        }
    }

    pub fn send(&self, response: Response) -> Result<(), ResponseSenderError> {
        self.tx
            .send(RoutedResponse {
                target: self.target,
                response,
            })
            .map_err(|_| ResponseSenderError)
    }
}

impl nenjo_harness::handlers::ResponseSender for ResponseSender {
    fn send(&self, response: Response) -> nenjo_harness::Result<()> {
        ResponseSender::send(self, response).map_err(|error| {
            nenjo_harness::HarnessError::response_transport(anyhow::anyhow!(error))
        })?;
        Ok(())
    }
}

pub(crate) type SeenMessageIds = Arc<DashMap<Uuid, Instant>>;

pub(crate) const SEEN_MESSAGE_TTL: Duration = Duration::from_secs(600);

pub(crate) fn new_seen_message_ids() -> SeenMessageIds {
    Arc::new(DashMap::new())
}

fn mark_message_seen(seen: &SeenMessageIds, message_id: Uuid) -> bool {
    let now = Instant::now();
    seen.retain(|_, inserted_at| now.duration_since(*inserted_at) <= SEEN_MESSAGE_TTL);
    seen.insert(message_id, now).is_none()
}

fn response_for_decode_failure(failure: &DecodingError) -> Option<Response> {
    let session_id = failure.session_id?;
    Some(Response::AgentResponse {
        session_id: Some(session_id),
        payload: StreamEvent::Error {
            message: "Execution failed".to_string(),
            payload: Some(json!({
                "code": failure.code,
                "message": failure.message,
            })),
            encrypted_payload: None,
        },
    })
}

#[derive(Debug, Clone)]
pub struct WorkerEventLoopContext {
    pub org_id: Uuid,
    pub capabilities: Vec<nenjo_events::Capability>,
}

pub(crate) async fn run<T>(
    runtime: &WorkerRuntime,
    mut bus: SecureEnvelopeBus<T>,
    ctx: WorkerEventLoopContext,
) -> Result<()>
where
    T: nenjo_eventbus::Transport + 'static,
{
    let worker_id = bus.transport().worker_id();
    let capabilities = ctx.capabilities.clone();

    info!(
        org_id = %ctx.org_id,
        %worker_id,
        ?capabilities,
        "Subscribing to eventbus"
    );

    let (response_tx, mut response_rx) = tokio::sync::mpsc::unbounded_channel::<RoutedResponse>();
    let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel::<ReceivedInput>();
    let system_response_tx = ResponseSender::system(response_tx.clone(), ctx.org_id);

    let app_version = Some(env!("CARGO_PKG_VERSION").to_string());
    let _ = system_response_tx.send(Response::WorkerRegistered {
        worker_id,
        capabilities: capabilities.clone(),
        version: app_version.clone(),
    });
    let _ = system_response_tx.send(Response::WorkerHeartbeat {
        worker_id,
        capabilities: capabilities.clone(),
        version: app_version.clone(),
    });

    let restore_ctx = runtime.command_context(Uuid::nil(), system_response_tx.clone());
    runtime.recover_reconcilable_sessions(restore_ctx).await;

    let heartbeat_tx = system_response_tx.clone();
    let heartbeat_shutdown = runtime.shutdown_token();
    let heartbeat_caps = capabilities;
    let seen_message_ids = runtime.seen_message_ids();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(45));
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if heartbeat_tx.send(Response::WorkerHeartbeat {
                        worker_id,
                        capabilities: heartbeat_caps.clone(),
                        version: app_version.clone(),
                    }).is_err() {
                        break;
                    }
                }
                _ = heartbeat_shutdown.cancelled() => break,
            }
        }
    });

    let response_bus = bus.publisher();
    let org_id = ctx.org_id;
    let response_shutdown = runtime.shutdown_token();
    let response_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = response_rx.recv() => {
                    match msg {
                        Some(routed) => {
                            let result = match routed.target {
                                ResponseTarget::Actor(actor_user_id) => {
                                    response_bus.send_response_for(org_id, actor_user_id, routed.response)
                                        .await
                                }
                                ResponseTarget::System { org_id } => {
                                    response_bus.send_system_response(org_id, routed.response).await
                                }
                            };
                            if let Err(e) = result {
                                warn!(error = %e, "Failed to send response");
                            }
                        }
                        None => break,
                    }
                }
                _ = response_shutdown.cancelled() => break,
            }
        }
    });

    let command_shutdown = runtime.shutdown_token();
    let command_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = bus.recv_command() => {
                    match result {
                        Ok(Some(item)) => {
                            if command_tx.send(item).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {
                            warn!("Event bus stream ended");
                            break;
                        }
                        Err(e) => {
                            warn!(error = %e, "Error receiving command");
                        }
                    }
                }
                _ = command_shutdown.cancelled() => break,
            }
        }
    });

    info!("Nenjo harness event loop started");

    while let Some(received) = command_rx.recv().await {
        match received {
            ReceivedInput::Command(received) => {
                let command = received.command.clone();
                let actor_user_id = received.envelope.user_id;
                let message_id = received.envelope.message_id;
                let source = received.source().cloned();
                if !mark_message_seen(&seen_message_ids, message_id) {
                    if let Err(e) = received.ack().await {
                        warn!(error = %e, %message_id, "Failed to ack duplicate command");
                    }
                    warn!(
                        actor_user_id = %actor_user_id,
                        %message_id,
                        command = %command,
                        source = ?source,
                        "Dropping duplicate worker command"
                    );
                    continue;
                }
                info!(
                    actor_user_id = %actor_user_id,
                    %message_id,
                    command = %command,
                    source = ?source,
                    "Received worker command"
                );
                if let Err(e) = received.ack().await {
                    warn!(error = %e, "Failed to ack command");
                }

                let ctx = runtime.command_context(
                    actor_user_id,
                    ResponseSender::for_actor(response_tx.clone(), actor_user_id),
                );

                tokio::spawn(async move {
                    if let Err(e) = crate::handlers::route_command(command, ctx).await {
                        error!(error = %e, "Error handling command");
                    }
                });
            }
            ReceivedInput::DecodeFailure(received) => {
                let actor_user_id = received.envelope.user_id;
                let message_id = received.envelope.message_id;
                let source = received.source().cloned();
                let failure = received.failure.clone();
                if !mark_message_seen(&seen_message_ids, message_id) {
                    if let Err(e) = received.ack().await {
                        warn!(error = %e, %message_id, "Failed to ack duplicate decode failure");
                    }
                    warn!(
                        actor_user_id = %actor_user_id,
                        %message_id,
                        code = failure.code,
                        source = ?source,
                        "Dropping duplicate decode failure"
                    );
                    continue;
                }
                if let Err(e) = received.ack().await {
                    warn!(error = %e, "Failed to ack decode failure");
                }
                if let Some(response) = response_for_decode_failure(&failure) {
                    let _ = response_tx.send(RoutedResponse {
                        target: ResponseTarget::Actor(actor_user_id),
                        response,
                    });
                } else {
                    warn!(
                        actor_user_id = %actor_user_id,
                        code = failure.code,
                        "Dropping user-facing decode failure without session context"
                    );
                }
            }
        }
    }

    runtime.cancel_active_executions();
    drop(response_tx);
    let _ = response_handle.await;
    let _ = command_handle.await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use dashmap::DashMap;
    use uuid::Uuid;

    use super::{SEEN_MESSAGE_TTL, SeenMessageIds, mark_message_seen};

    #[test]
    fn message_dedupe_rejects_seen_message_ids() {
        let seen: SeenMessageIds = Arc::new(DashMap::new());
        let message_id = Uuid::new_v4();

        assert!(mark_message_seen(&seen, message_id));
        assert!(!mark_message_seen(&seen, message_id));
    }

    #[test]
    fn message_dedupe_expires_old_entries() {
        let seen: SeenMessageIds = Arc::new(DashMap::new());
        let old_message_id = Uuid::new_v4();
        let new_message_id = Uuid::new_v4();
        seen.insert(
            old_message_id,
            Instant::now() - SEEN_MESSAGE_TTL - Duration::from_secs(1),
        );

        assert!(mark_message_seen(&seen, new_message_id));
        assert!(!seen.contains_key(&old_message_id));
    }
}
