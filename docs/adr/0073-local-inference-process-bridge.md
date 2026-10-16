# 0073. Local inference runs as an owned child process

Status: Accepted. Text-only transport proof. Tools and images are refused.
Date: 2026-10-15

## Context

ARK Inference is a separate C++ engine (`../ark-inference`). It exposes C++ objects and a CLI.
ARK consumes models through the asynchronous `Provider` trait and streamed text.
A foreign-function interface would share allocator, panic, thread and lifetime failures with ARK.
It would also put a C++ toolchain into the ARK Rust build.

The engine today is a correctness and transport proof, not a fast backend.
Measured on an Apple M4 Pro with Qwen3 4B Q4_K_M (2026-10-16, engine CLI, 448-token prompt):

| Backend | Prefill | Decode |
|---|---:|---:|
| Metal | 32 tokens/s | 8.5 tokens/s |
| CPU, 8 threads | 9 tokens/s | 5.8 tokens/s |
| CPU, 2 threads (the default, busy machine) | 2.5 tokens/s | not measured |

The engine's iteration 0008 record has 36 tokens/s Metal prefill. llama.cpp on the same file
prefilled about 480 tokens/s on Metal (engine iteration 0003, not a paired run), so ARK Inference
is more than 10 times behind. The bridge also re-runs the full prefill on every call.

## Decision

ARK runs the engine's `ark-inference-bridge` executable as a persistent child it owns.
They talk over stdin and stdout with a small framed protocol (version one, below).
`ark_model::Inference` is the consumer. `ARK_PROVIDER=inference` selects it, so `inference` is now a
reserved name: a named endpoint called `inference` is rejected.

| Rule | Detail |
|---|---|
| Explicit paths | Executable and model paths are absolute. No PATH search, no download, no catalog scan |
| No shell | Arguments are an array. The child environment is cleared (Windows keeps `SystemRoot`) |
| No network | The child opens no listener. Prompt text travels only through stdin |
| One request | One active request per provider. A second concurrent call fails at once; there is no queue |
| Warm on success | A checked `D` frame is the only state that returns the child for reuse. A child that exited while idle is replaced |
| Cancel kills | Dropping the completion future kills and evicts the child. The next call reloads the model |
| Text only | Nonempty tools, tool history, tool results and pictures fail before the child starts |
| Permanent refusals | Local refusals use `ProviderError::Api`, not `Refused`, because `Refused` is retried |
| Quiet stderr | Child stderr goes to null, so it cannot block or leak prompt text into logs |

### Protocol version one

Each frame is one tag byte, a four-byte little-endian payload length, then the payload.
The largest payload is 1048576 bytes. The receiver checks the length before it allocates.

| Tag | From | Payload |
|---|---|---|
| R | child | four bytes, version 1, once after the model loads |
| G | parent | four-byte message count, then per message: role byte (1 system, 2 user, 3 assistant), four-byte length, text |
| A | child | eight-byte logical request bytes, eight-byte actual array capacity |
| T | child | nonempty generated token bytes; a UTF-8 character may span frames |
| D | child | prompt tokens, output tokens, remaining staging bytes, remaining registrations (eight bytes each) |
| E | child | four-byte code: 1 malformed framing, 2 request refused, 3 computation failed |

A request is `G`, then `A`, then zero or more `T`, then `D`. `E` can replace `A` or `D`.
The parent evicts the child on any unknown tag, bad length, wrong order, wrong count or nonzero cleanup field in `D`.
It also evicts after every `E`. Message count is 1 to 1024.
The tokenizer inserts chat markers itself, so user text cannot forge a message boundary.
The parent emits only complete UTF-8 prefixes and keeps at most three pending bytes.
Invalid or incomplete final UTF-8 fails. Text streamed before a later error is provisional.

Deadlines default to 120 s for startup, 600 s from the request to the first token, 120 s for each
later frame and 600 s per call. All must be positive. The first-token deadline is separate because
it covers the whole prefill, which takes minutes on CPU. `ARK_INFERENCE_*_SECONDS` override them.

### Resource scope

Startup arguments bound model file bytes, request-array bytes, context, output tokens, input and output bytes.
These are payload and array limits, not a resident-memory limit.
Tokenizer work, metadata, device resources, allocator overhead and OS caches are outside them.
A total-process bound needs an OS sandbox or job limit, which this bridge does not install.

## Consequences

- ARK and the engine build and version independently. ARK has no C++ build dependency.
- A cancelled call costs a model reload. This buys cancellation without a kernel-interrupt API.
- An agent that needs tools gets a permanent capability error. ARK never silently drops tools.
- Every call re-runs prefill for the whole conversation. There is no prompt cache across calls.
- The provider is useful for a text-only proof, not for interactive agent work, until prefill speed improves.
- Windows and NVIDIA are unexecuted.

## Check

`crates/ark-model/tests/inference.rs` drives a fake worker through framing, UTF-8, order, limits,
cancellation and reuse. `crates/ark-runtime/tests/retry.rs`
(`local_text_inference_refuses_agent_tools_without_retry_or_spawn`) checks the Harness gets a
permanent tool refusal with no retry and no spawn. `crates/ark-model/tests/inference_engine.rs`
(`#[ignore]`) runs the real engine and requires byte-identical output to the engine CLI.
