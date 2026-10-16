# 0070. Dictation is client capture and an injected transcriber

Status: Accepted
Date: 2026-10-14

## Context

The shared ARK page runs in WKWebView on macOS, WebView2 on Windows, WebKitGTK on Linux and a
browser for a local or remote runtime. Dictation must behave the same in each one. The browser
Speech Recognition API is not available in every engine and may send audio to a browser vendor.
Apple and Windows speech APIs have different packaging, permission and network rules. Linux has
no matching system API. A remote transcription API has good quality but sends work to a third
party and adds a recurring cost.

| Choice | Privacy | First result | Installed size | Offline | Cost | Maintenance |
|---|---|---:|---:|---|---|---|
| Browser Speech Recognition | browser decides, often remote | live | none | not reliable | browser service | low, uneven support |
| Native system APIs | OS decides | live | none | system dependent | none | three implementations, no Linux peer |
| Whisper in the page with WASM | on the client | slow on modest devices | model plus WASM | after download | none | browser memory and SIMD differences |
| Remote transcription provider | third party receives audio | fast | none | no | per minute | account, quota and provider handling |
| Native whisper.cpp at the runtime | selected ARK runtime only | after Stop | 148 MB model, small engine | after download | none | one implementation on three systems |

The multilingual whisper.cpp base model is 147,951,465 bytes. Its size rules out bundling it in
every ARK consumer. The model can instead be installed once, after the person first chooses the
microphone. The pinned artifact is
`https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin`,
SHA-256 `60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe`.
whisper.cpp and the OpenAI Whisper weights are MIT licensed. A model update is an ARK change
with a new pinned revision, hash and measured fixture run.

## Decision

The page captures mono speech and sends bounded 16 kHz PCM chunks to the selected ARK runtime.
`ark-ui` owns an injected `TranscriptionProvider` contract and authenticated upload lifecycle.
It does not depend on a speech engine. The optional `ark-transcribe` crate implements the first
provider with whisper.cpp. An application chooses whether to inject it.
The decoder explicitly selects language detection when no language is supplied, rather than
inheriting Whisper's English default. Explicit languages are checked before native decoding.

The first microphone action starts the model installation and recording. The download is
single-flight, streamed to a temporary file, checksum checked and atomically installed. It never
starts before that action. Desktop and a loopback browser keep audio on the same computer. A
remote HTTPS page sends audio to the runtime named by that chat, including an ARK Cloud runtime.
Audio is never sent to a browser speech service or a transcription vendor.

Each recording captures an explicit computer route before asking for microphone permission.
Every status, Begin, chunk, Finish and Cancel call uses that route. An empty place means this
runtime even when another chat is now shown. Pending places work before the first chat message.
Begin returns the runtime identity; later upload calls verify it before forwarding audio, so
reassigning a saved computer name cannot retarget a recording. Missing or changed links fail
without a local fallback. Only these five dictation calls enter the remote allowlist. Finish
has a bounded 15-minute limit for first-use preparation and inference, rather than the ordinary
eight-second call limit. The model download itself has a five-minute timeout.

Each upload has a server-made random id bound to its client. Chunks are at most 256 KiB, ordered
and capped at 15,360,000 bytes (exactly eight minutes). Identical retries are accepted;
conflicting retries are refused. Ownership comes from the authenticated session or native
window observed by the server, never a page-supplied identity. Web tabs sharing one credential deliberately share one
server-side upload allowance; the page lease is narrower and prevents two composers in one page.
Cancel and completion remove the audio. An unfinished upload expires after ten minutes and is
swept by the runtime once a minute. Only one unfinished upload is allowed per owner. Inference
runs off the async and UI threads, one at a time per runtime. Cancelled native inference may
finish, but its result is discarded. Provider status has four wire values: `not-installed`,
`downloading`, `ready` and `failed`.

The composer always shows one microphone affordance. It explains an insecure origin, missing
media APIs, missing provider, denied permission, missing input device and preparation or
inference failure. One page records at a time. Stop, submit, chat disposal and page hide release
the microphone. A completed transcript is inserted at the owning composer's remembered caret and
is never sent automatically.

The button is a 30 px icon beside Attach, with a 40 px hit target on a coarse pointer. It uses
the existing composer and theme tokens. Recording changes the microphone to the square Stop mark
and adds the word `Listening`, so colour is never the only sign. Focus uses the page's visible
focus ring. Status is one short line inside the composer, associated with the button and exposed
as a polite live region. It truncates before controls move or the page overflows at 320 to 390 px.
There is no pulse when reduced motion is requested.

| State | Button and action | Persistent line | Accessible state and announcement | Composer and exit |
|---|---|---|---|---|
| Ready | microphone, starts | none | `Start dictation`, not pressed | every control stays usable |
| Recording, model ready | Stop, stops | `Listening · m:ss` | `Stop dictation`, pressed; announce `Listening` once | typing stays usable; Stop ends capture |
| Recording, model downloading | Stop, stops | `Listening · downloading 148 MB speech model` | same pressed state; announce preparation once | Stop ends capture, shared download continues |
| Stopped, model downloading | Stop preparation, cancels this result | `Downloading 148 MB speech model` | `Cancel dictation`; progress changes are polite, not chatty | draft stays editable |
| Transcribing | Stop, cancels this result | `Transcribing` | `Cancel dictation`; announce once | draft stays editable |
| Complete | microphone, starts another | `Dictation added` briefly | announce only those two words, never the transcript | preserve focus and all controls |
| Unavailable: insecure or API missing | microphone with unavailable mark, explains | short reason after click | `Dictation unavailable`; reason is live | typing is the recovery path |
| Provider absent | same unavailable control | `Dictation is not included in this app` after click | same | typing is the recovery path |
| Permission denied | microphone, retries | `Microphone access is off. Allow it in system settings.` | announce the reason | existing draft stays |
| No device | microphone, retries | `No microphone was found.` | announce the reason | existing draft stays |
| Capture failure | microphone, retries | `The microphone stopped. Try again.` | announce the reason | existing draft stays |
| Upload or connection failure | microphone, retries | `Audio could not reach this chat's computer. Try again.` | announce the reason | existing draft stays |
| Preparation failure | microphone, retries preparation | `Speech recognition could not be prepared. Try again.` | announce the reason | existing draft stays |
| Inference failure | microphone, retries with a new recording | `That recording could not be transcribed. Try again.` | announce the reason | existing draft stays |
| Eight-minute limit | Stop while the final audio is kept | `Eight-minute limit reached · transcribing` | announce limit then transcription | no audio is discarded |

Pressing Send while recording stops capture and does not send. The transcript is added when it
is ready, and the person reviews it before pressing Send again. Recording never inserts interim
text. The owning composer remembers `selectionEnd` on its input, selection, pointer, key and focus
events. This is a textarea offset in UTF-16 code units, not a byte index. On completion, the
transcript is inserted with JavaScript string/textarea semantics at that offset, without replacing a
selection or stealing focus. If the textarea is unfocused, its last remembered position is used.
Another composer cannot become the owner while the page-wide recording lease is held. Disposal
invalidates the result. Edits anywhere in the draft therefore remain byte-for-byte; a caret that
moved while ARK worked controls where the speech is added. Smart whitespace separates the
inserted transcript from adjacent text.

Each composer owns its microphone and status nodes. The page reserves capture before awaiting
the permission prompt and holds that reservation through transcription. Stop releases the
microphone; Cancel, chat/computer changes and disposal invalidate the result. A late Begin is
cancelled on its original computer. A late transcript never enters a different chat. Begin
failure releases capture immediately and permits retry. Native inference retains its semaphore
until its blocking call ends, even if its async waiter is cancelled.

## Consequences

- Dictation has one capture and transport path across Desktop and the browser.
- A generic ARK application does not inherit a C++ build, model download or fixed provider.
- First use downloads 148 MB and batch transcription appears after Stop. The UI must say both.
- Remote use sends audio to that ARK runtime. A hosted runtime needs to state its own retention
  policy even though no transcription vendor receives it.
- An agent-facing speech-to-text action can later use `TranscriptionProvider`, but it is a
  separate capability. It never owns the person's microphone or composer state.
- Windows and Linux remain explicitly unverified until their native hardware smoke tests run.

## Check

The upload cap is the exact PCM limit the page and provider use (2026-10-15). The last sample
is accepted; the next one is refused without advancing the upload. Draft checks cover empty and
cleared drafts, chat round trips, same-chat selection and promotion of a split composer.

- The automated WebKit page test covers a delayed availability response, text-preserving
  insertion at a UTF-16 caret, microphone release on Stop and page hide, permission denial,
  Begin failure/retry, late Begin/Finish after a chat switch, pending computer availability,
  separate split controls, a shared permission lease and disposal during a permission prompt.
  Chromium uses the same scenario when `ARK_UI_BROWSER=chromium` is set.
- Rust tests cover lazy provider construction, installed-model discovery, upload ownership,
  ordered/idempotent chunks and expiry. A real authenticated two-runtime test covers status,
  pending-place Begin, local and remote upload/Finish after chat switches, owner spoofing,
  Cancel, unknown computers, changed runtime identity and Finish longer than eight seconds.
- Native microphone smoke tests and a fixed multi-minute quality fixture remain release gates on
  macOS, Windows and Linux. The fixture must record cold and warm latency, peak memory,
  punctuation and the exact model hash. These environments are explicit, not silent fallbacks.
