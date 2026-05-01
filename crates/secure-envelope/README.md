# nenjo-secure-envelope

Secure envelope layer that sits between `nenjo-eventbus` and the worker harness.

It provides:

- `SecureEnvelopeBus<T>`: wraps a raw `nenjo-eventbus::EventBus<T>`
- `EnvelopeCodec`: trait for secure command/response transforms
- `SecureEnvelopeCodec`: the default codec used by the worker runtime
- shared encrypted payload helpers such as `encrypt_text_for_scope()` and `decrypt_text()`

## Usage

The intended composition is:

1. build a raw `nenjo-eventbus::EventBus`
2. build a key provider from `nenjo-crypto-auth`
3. build a `SecureEnvelopeCodec`
4. wrap the raw bus in `SecureEnvelopeBus`

```rust,ignore
use nenjo_crypto_auth::EnrollmentBackedKeyProvider;
use nenjo_eventbus::EventBus;
use nenjo_secure_envelope::{SecureEnvelopeBus, SecureEnvelopeCodec};

let raw_bus = EventBus::builder()
    .transport(transport)
    .build()
    .await?;

let key_provider = EnrollmentBackedKeyProvider::new(
    auth_provider,
    api_client,
    api_key_id,
    bootstrap_user_id,
);

let codec = SecureEnvelopeCodec::new(key_provider, org_id);
let mut secure_bus = SecureEnvelopeBus::new(raw_bus, codec);

while let Some(input) = secure_bus.recv_command().await? {
    // route input to the harness
}
```

## Boundaries

- `nenjo-eventbus` owns raw transport envelopes only
- `nenjo-secure-envelope` owns secure decoding/encoding and user-safe decode failures
- `nenjo-crypto-auth` owns the persisted wrapped-key and enrollment state
