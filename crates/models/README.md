# nenjo-models

LLM provider trait and implementations for the Nenjo agent platform.

## Supported providers

- **Anthropic** — Claude models (Opus, Sonnet, Haiku)
- **OpenAI** — GPT-4o, o1, o3, o4 series
- **Google Gemini** — Gemini Pro, Flash
- **OpenRouter** — access 200+ models through a single API
- **Ollama** — local model inference
- **vLLM** — Responses by default, with streaming, artifact inputs, and optional Chat Completions
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

## Streaming

The first-class vLLM provider requests Responses SSE by default, including when
the caller uses the buffered `chat` API. It completes on `response.completed`.
To use Chat Completions, explicitly select `.with_api(VllmApi::ChatCompletions)`.
That transport completes on the server's
`data: [DONE]` marker, after accumulating text, tool arguments, and usage. A
choice's `finish_reason` can precede the final usage chunk and does not end the
stream. An HTTP body that ends before `[DONE]` returns an error.

xAI's `chat_stream` uses Responses SSE when provider-native tools are enabled.
It completes on `response.completed`, retaining the final response and usage.
Tool/item completion events do not end the response. Failed, incomplete,
malformed, or prematurely closed streams return errors. Both SSE consumers
preserve UTF-8 and event boundaries across HTTP chunks and ignore heartbeat
comments.

`VllmProvider::new(base_url, api_key)` uses the Responses API. It shares the
configured HTTP client and optional authentication with Chat Completions. There
is no automatic endpoint fallback. Full local history is sent with `store: false`.
Requests use `tool_choice: "auto"` and `parallel_tool_calls: true`.
`response.completed` is terminal; failed/incomplete responses, truncated SSE,
and malformed final function calls return errors. Reasoning deltas remain separate
from assistant text, and only final function calls are passed to the runtime.

Responses transports digest-verified image and UTF-8 text artifacts, including
multipart function outputs. Inline limits are 16 MiB per image and 256 KiB per
text artifact. It does not advertise Chat Completions audio/video extensions or
raw document files. Model capabilities and host artifact routing still apply.
The transport uses upstream vLLM's Responses contract, with no Mia-specific
request fields. See the [live test suite](../../testing/integrations/README.md)
for running against Mia Labs or stock vLLM.

Configure optional Responses generation defaults through the SDK:

```rust,ignore
use std::num::NonZeroU32;
use nenjo_models::{ReasoningEffort, ResponsesOptions, VllmProvider};

let provider = VllmProvider::new(Some("http://localhost:8000/v1"), None)
    .with_responses_options(ResponsesOptions {
        reasoning_effort: Some(ReasoningEffort::Low),
        max_output_tokens: NonZeroU32::new(4096),
    });
```

vLLM defaults to `max` reasoning when effort is unspecified, including when only
an output cap is configured. Explicit efforts override this default;
`Some(ReasoningEffort::None)` requests disabled thinking. The output cap is omitted
by default, preserving the server's generation limit. Effort support
depends on the model template. The output budget includes reasoning tokens.
The worker exposes the same defaults in `[vllm.responses]` and through
`NENJO_VLLM_REASONING_EFFORT` / `NENJO_VLLM_MAX_OUTPUT_TOKENS`.

Responses terminal failures return `ResponseTerminationError` with a typed
`ResponseTermination`, provider response ID, and available usage. `ReliableProvider`
passes these errors through without retry or model/provider fallback, including
when the server returns an output-limit or cancellation event before any text.
No incomplete response exposes executable function calls.

`ChatResponse.usage` retains optional `cached_input_tokens` and `reasoning_tokens`.
They are included in input/output totals, respectively, and must not be added
again. Missing details remain `None`; the SDK does not estimate them. These are
per-request details; existing session totals still aggregate input/output tokens.
Enable `RUST_LOG=info,nenjo_models::responses=debug` to correlate provider response IDs and HTTP request
IDs with model request spans and first-event/first-delta/total timings. Buffered
JSON replies have no incremental first-delta time. These events log counts and
identifiers without logging prompt or generated text.
The tested Mia revision emits reasoning deltas but reports `reasoning_tokens: 0`;
Nenjo preserves provider accounting rather than estimating missing reasoning counts.

The native OpenAI, OpenRouter, Anthropic, Gemini, and Ollama adapters currently
use buffered HTTP responses. Generic OpenAI-compatible providers and xAI calls
without provider-native tools also use buffered HTTP responses. Calling
`chat_stream` on these paths delegates to `chat`; their upstream support for
streaming does not enable streaming in these adapters.

## Usage

```rust,ignore
use nenjo_models::{ModelProvider, AnthropicProvider};

let provider = AnthropicProvider::new(Some("sk-ant-..."));
let response = provider.chat(request, "claude-sonnet-4-20250514", 0.7).await?;
println!("{}", response.text);
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.
