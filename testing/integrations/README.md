# Provider end-to-end tests

The vLLM Responses suite runs real inference through `nenjo-models` and includes
an SDK turn that executes a local tool, materializes its image artifact, and
completes the follow-up model request. It checks text deltas against the final
answer, final usage, both wire delivery modes, function-call IDs and replay,
UTF-8 text artifacts, image attachments, reasoning controls, detailed usage, and
typed output-limit errors in both wire modes. Test failures are never treated as
successful skips. Each inference or SDK turn has a 120-second deadline.
The suite uses the provider's default Responses API without explicitly selecting it.

```sh
NENJO_VLLM_BASE_URL=http://vllm.apps.boonlabs.internal/v1 \
  cargo test -p nenjo-integration-tests --test vllm_responses \
  -- --ignored --test-threads=1 --nocapture
```

Set `NENJO_VLLM_MODEL` when the server advertises multiple models. `VLLM_API_KEY`
is optional for endpoints that require Bearer authentication. The suite requires
a vision/reasoning model with automatic function calling enabled. For a text-only stock
vLLM deployment, run individual text, buffered, or function-call tests using the
test-name filter. Normal workspace tests leave these network tests ignored.

The checked-in `tests/fixtures/vllm/color.png` is a synthetic 64×64 red PNG,
generated locally without external assets. Tests do not send production prompts,
bookmarks, or user artifacts. The tool-artifact test derives the color from image
bytes, not from the tool's text result.

The client targets the standard `/v1/responses` wire contract and sends full
history with `store: false`. Image input uses `input_image`; function results use
`function_call_output` with the original `call_id`. Video/audio extension parts
are not part of the Responses input schema on the tested deployment. Those
modalities continue to use Nenjo's Chat Completions mode. Raw documents use the
host's existing artifact extraction/analysis path.

Validated on 2026-09-11 against the Mia Labs server above: model
`GLM-5.3-Flash-EXL3`, vLLM `0.1.dev20051+g487ecf187`; all eight live tests passed.
A separate stock vLLM server has not been live-tested. Its compatible Responses
schema is the intended portability boundary, not a requirement for Mia patches.

The reasoning test verifies actual reasoning deltas with `low` and their absence
with `none`. The tested Mia build reports `reasoning_tokens: 0` even when it emits
reasoning deltas. The suite requires the usage breakdown to be present and checks
that reported reasoning tokens fit within output totals; it does not invent a
positive count for the server. A one-token output cap must return a typed
`OutputLimit` error retaining response ID and usage in both SSE and JSON modes.

Mia's GLM template distinguishes `low`, `high`, and `max`; other enabled effort
values fall back to `max`. Effort choices on stock vLLM depend on its model template.

References used to verify this contract:

- [Mia Labs GLM 5.3 Flash deployment](https://github.com/MiaAI-Lab/GLM-5.3-Flash-EXL3-2x-DGX-Sparks)
- [Mia GLM reasoning template](https://github.com/MiaAI-Lab/GLM-5.3-Flash-EXL3-2x-DGX-Sparks/blob/main/files/chat_template.jinja)
- [vLLM Responses protocol at the tested server revision](https://github.com/vllm-project/vllm/blob/487ecf187/vllm/entrypoints/openai/responses/protocol.py)
- [vLLM Responses history conversion](https://github.com/vllm-project/vllm/blob/487ecf187/vllm/entrypoints/openai/responses/utils.py)
- [OpenAI image input contract](https://developers.openai.com/api/docs/guides/images-vision)
