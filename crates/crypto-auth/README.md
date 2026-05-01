# nenjo-crypto-auth

Worker crypto enrollment and wrapped-key state primitives.

It provides:

- `WorkerAuthProvider`: persisted local worker identity and enrollment state
- `EnrollmentBackedKeyProvider`: key-provider implementation for secure-envelope codecs
- wrapped key types and worker certificate types
- `ContentScope` and `ContentKey`

## Usage

Typical worker usage:

```rust,ignore
use std::sync::Arc;

use nenjo_crypto_auth::{EnrollmentBackedKeyProvider, WorkerAuthProvider};

let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(state_dir.join("crypto"))?);

let key_provider = EnrollmentBackedKeyProvider::new(
    auth_provider.clone(),
    api_client,
    api_key_id,
    bootstrap_user_id,
);
```

`WorkerAuthProvider` is responsible for:

- loading or creating the worker’s local identity
- persisting enrollment state from the backend
- unwrapping cached user-routed `ACK` / org-scoped `OCK` material for runtime use
- storing per-user wrapped `ACK`s delivered through enrollment or account-key sync

## Boundaries

- this crate owns trust-state and wrapped-key persistence
- `nenjo-secure-envelope` consumes these primitives to decrypt and encrypt envelopes
- `nenjo-worker` composes both crates into the running harness
