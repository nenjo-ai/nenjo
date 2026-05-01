use anyhow::Result;
use nenjo_crypto_auth::WrappedAccountContentKey as StoredWrappedAccountContentKey;
use nenjo_events::WrappedAccountContentKey;

use super::CommandContext;

pub async fn handle_worker_account_key_updated(
    ctx: &CommandContext,
    wrapped_ack: WrappedAccountContentKey,
) -> Result<()> {
    ctx.auth_provider
        .store_user_ack(
            ctx.actor_user_id,
            StoredWrappedAccountContentKey {
                key_version: wrapped_ack.key_version,
                algorithm: wrapped_ack.algorithm,
                ephemeral_public_key: wrapped_ack.ephemeral_public_key,
                nonce: wrapped_ack.nonce,
                ciphertext: wrapped_ack.ciphertext,
                created_at: wrapped_ack.created_at,
            },
        )
        .await?;
    Ok(())
}
