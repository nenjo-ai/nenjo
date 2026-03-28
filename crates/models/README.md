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

## Usage

```rust,ignore
use nenjo_models::{ModelProvider, AnthropicProvider};

let provider = AnthropicProvider::new(Some("sk-ant-..."));
let response = provider.chat(request, "claude-sonnet-4-20250514", 0.7).await?;
println!("{}", response.text);
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.
