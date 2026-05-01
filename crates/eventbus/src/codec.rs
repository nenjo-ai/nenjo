use std::error::Error as StdError;

use async_trait::async_trait;
use nenjo_events::{Command, Response};

pub type CodecError = Box<dyn StdError + Send + Sync + 'static>;
pub type CodecResult<T> = Result<Option<T>, CodecError>;

#[async_trait]
pub trait EventCodec: Send + Sync + 'static {
    async fn encode_command(&self, command: Command) -> CodecResult<Command>;
    async fn decode_command(&self, command: Command) -> CodecResult<Command>;
    async fn encode_response(&self, response: Response) -> CodecResult<Response>;
    async fn decode_response(&self, response: Response) -> CodecResult<Response>;
}

#[derive(Debug, Default)]
pub struct NoopEventCodec;

#[async_trait]
impl EventCodec for NoopEventCodec {
    async fn encode_command(&self, command: Command) -> CodecResult<Command> {
        Ok(Some(command))
    }

    async fn decode_command(&self, command: Command) -> CodecResult<Command> {
        Ok(Some(command))
    }

    async fn encode_response(&self, response: Response) -> CodecResult<Response> {
        Ok(Some(response))
    }

    async fn decode_response(&self, response: Response) -> CodecResult<Response> {
        Ok(Some(response))
    }
}
