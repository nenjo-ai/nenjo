# nenjo-models

LLM provider trait and implementations for the Nenjo agent platform.

## Supported providers

- **Anthropic** — Claude models (Opus, Sonnet, Haiku)
- **OpenAI** — GPT-4o, o1, o3, o4 series
- **Google Gemini** — Gemini Pro, Flash
- **OpenRouter** — access 200+ models through a single API
- **Ollama** — local model inference
- **OpenAI-compatible** — any API that follows the OpenAI chat completions format

## Reliability

Wrap any provider with `ReliableProvider` for automatic:
- Exponential backoff retries with configurable max attempts
- Rate limit handling (429 detection with Retry-After parsing)
- API key rotation on rate limits
- Provider fallback chains
- Per-model fallback configurations

OpenAI-compatible providers, including vLLM, default to a 10-second connection
timeout and a 300-second idle read timeout, with no total request deadline.

Every HTTP provider exposes `with_http_client` for SDK callers to configure
transport timeouts before wrapping it in `ReliableProvider`. For local inference:

```rust,ignore
let client = reqwest::Client::builder()
    .connect_timeout(std::time::Duration::from_secs(10))
    .read_timeout(std::time::Duration::from_secs(600))
    .build()?;
let provider = nenjo_models::OllamaProvider::new(None).with_http_client(client);
```

This client has no total request deadline; the idle read deadline still detects
stalls. The worker exposes these settings through `[reliability]` in its config.

## Usage

```rust,ignore
use nenjo_models::{ModelProvider, AnthropicProvider};

let provider = AnthropicProvider::new(Some("sk-ant-..."));
let response = provider.chat(request, "claude-sonnet-4-20250514", 0.7).await?;
println!("{}", response.text);
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.
