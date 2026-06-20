use std::sync::Arc;

use anyhow::Result;
use nenjo_events::{EncryptedPayload, Response};
use nenjo_platform::tools::{PlatformNotificationEmitter, PlatformNotificationRecipient};
use uuid::Uuid;

use super::ResponseSender;

pub(crate) fn platform_notification_emitter<S>(
    response_sink: S,
    session_id: Uuid,
) -> Arc<dyn PlatformNotificationEmitter>
where
    S: ResponseSender + 'static,
{
    Arc::new(PlatformResponseNotificationEmitter {
        response_sink,
        session_id,
    })
}

struct PlatformResponseNotificationEmitter<S> {
    response_sink: S,
    session_id: Uuid,
}

impl<S> PlatformNotificationEmitter for PlatformResponseNotificationEmitter<S>
where
    S: ResponseSender,
{
    fn send_push_notification(
        &self,
        agent: &str,
        current_session_id: Option<Uuid>,
        encrypted_payload: EncryptedPayload,
        recipient: Option<PlatformNotificationRecipient>,
    ) -> Result<()> {
        self.response_sink.send(Response::PushNotification {
            agent: agent.to_string(),
            session_id: current_session_id.unwrap_or(self.session_id),
            recipient_user_id: recipient.as_ref().and_then(|target| target.user_id),
            recipient_handle: recipient.and_then(|target| target.handle),
            encrypted_payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingResponseSink {
        responses: Mutex<Vec<Response>>,
    }

    impl ResponseSender for RecordingResponseSink {
        fn send(&self, response: Response) -> Result<()> {
            self.responses.lock().unwrap().push(response);
            Ok(())
        }
    }

    fn encrypted_payload() -> EncryptedPayload {
        EncryptedPayload {
            account_id: Uuid::new_v4(),
            encryption_scope: Some("org".to_string()),
            object_id: Uuid::new_v4(),
            object_type: "push.notification".to_string(),
            algorithm: "xchacha20poly1305".to_string(),
            key_version: 1,
            nonce: "nonce".to_string(),
            ciphertext: "ciphertext".to_string(),
        }
    }

    #[test]
    fn platform_notification_emitter_forwards_push_notification_response() {
        let sink = Arc::new(RecordingResponseSink::default());
        let fallback_session_id = Uuid::new_v4();
        let current_session_id = Uuid::new_v4();
        let recipient_user_id = Uuid::new_v4();
        let emitter = platform_notification_emitter(sink.clone(), fallback_session_id);

        emitter
            .send_push_notification(
                "triage-agent",
                Some(current_session_id),
                encrypted_payload(),
                Some(PlatformNotificationRecipient {
                    user_id: Some(recipient_user_id),
                    handle: Some("@casey".to_string()),
                }),
            )
            .expect("push notification should be forwarded");

        let responses = sink.responses.lock().unwrap();
        assert_eq!(responses.len(), 1);
        match &responses[0] {
            Response::PushNotification {
                agent,
                session_id,
                recipient_user_id: actual_user_id,
                recipient_handle,
                encrypted_payload,
            } => {
                assert_eq!(agent, "triage-agent");
                assert_eq!(*session_id, current_session_id);
                assert_eq!(*actual_user_id, Some(recipient_user_id));
                assert_eq!(recipient_handle.as_deref(), Some("@casey"));
                assert_eq!(encrypted_payload.object_type, "push.notification");
            }
            other => panic!("expected push notification response, got {other:?}"),
        }
    }
}
