# 0105. Dictation shows words while listening

Status: Accepted
Date: 2026-11-06

Amends [0070](0070-dictation-is-client-capture-and-an-injected-transcriber.md).

## Context

ADR 0070 sent the whole recording after Stop and showed nothing until the transcript was ready.
The person saw `Listening · m:ss`, `Transcribing` and `Dictation added` and no words. A long
recording waited as long as it was long. The same model (whisper.cpp base) reads audio faster
than it is spoken, so the words can be shown while the person talks.

## Decision

| Part | Change |
|---|---|
| Upload | The page sends each 700 ms of audio as a chunk while recording. Stop sends the rest. The five calls of 0070 are unchanged. |
| `dictation_preview {id}` | New sixth call, answers `{text}`: the words of the audio so far. It crosses to a held chat's computer like the other five. An older computer refuses it, and the page then shows the words at Stop as before. |
| Window | A preview reads at most 30 s. Once the unsettled audio is 12 s long, every stretch of speech that ended 3 s before its end is settled: its words are kept and its audio is not read again. Finish reads only the unsettled rest. |
| Provider | `TranscriptionProvider::segments` returns timed stretches. The default answers one stretch for the whole audio, which is never settled. `LocalWhisper` keeps the loaded model for 45 s and drops `[BLANK_AUDIO]` and `(music)` markers from previews. |
| One at a time | One preview runs per recording. A call that arrives meanwhile, or before half a second of audio, answers the last text. |
| Composer | The words stand in the draft at the caret and are replaced by each preview and by the final text. The draft is read-only while listening, so a preview never overwrites typing. Stop, Send and cancel keep their meaning. Cancel removes the interim words. |
| Words on screen | None while listening or finishing. A line shows only for `Downloading speech model` and for a failure. The microphone becomes a square while listening. `Listening · m:ss`, `Preparing speech recognition`, `Transcribing` and `Dictation added` are gone. |

## Consequences

- A transcript can differ from the preview. The final text replaces the preview, and is the only one the person keeps.
- The model stays in memory 45 s after the last use (about 150 MB).
- Cost: a preview is one inference of at most 30 s of audio each 0.5 s after the last one ends. A computer that reads slower than it hears falls behind but never queues.
- A computer without `segments` shows words only at Stop.
