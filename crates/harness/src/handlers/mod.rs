//! Platform command handlers owned by the harness.

use nenjo_events::Response;

use crate::Result;

pub mod chat;
pub mod cron;
pub mod crypto;
pub mod domain;
pub mod heartbeat;
pub mod manifest;
pub mod repo;
pub mod task;

pub trait ResponseSender: Send + Sync {
    fn send(&self, response: Response) -> Result<()>;
}

impl<T> ResponseSender for std::sync::Arc<T>
where
    T: ResponseSender + ?Sized,
{
    fn send(&self, response: Response) -> Result<()> {
        self.as_ref().send(response)
    }
}
