# Advanced files A0 evidence

Measured on 2026-10-15 on macOS 26.4.1, Apple silicon. These are development proofs, not a
released capability or a clean installed-app result.

| Proof | Result |
|---|---|
| Snapshot at 1 MiB | Open 1 ms, including copy and verification |
| Snapshot at 256 MiB | Open 415 ms, all chunks read by 433 ms. Source was a sparse zero-filled temporary file. Warm filesystem result, not a general storage benchmark |
| Replacement between chunks | Complete original version at every tested boundary |
| In-place overwrite and truncation between passes | Retry to the complete new version, or refuse after three changes |
| Path and cancellation | Outside paths, links outside the project, directories, source cap and cancelled copy refuse |
| Worker proof | Real WebKit module worker, malformed GLB error, main page remains usable, zero outbound requests |
| Near-limit STL preflight | 333,333 triangles, 16,666,734 bytes, 20 ms in WebKit. This is validation time, not rendering time |

Commands: `cargo test -p ark-files snapshot`,
`cargo test -p ark-files measured_snapshot_limits -- --ignored --nocapture`,
and Desktop `node --test ui/tests/file-preflight.test.mjs ui/tests/file-worker.webkit.test.mjs`.

## Snapshot design correction

ARK's typed writes and Restore already replace files atomically. Run code writes directly in
the project; it has no separate output-publication step. Its writes need the same double-pass
check as an external editor. The implementation uses no process-global lease or registry.
An open file handle, descriptor-relative traversal, two full hashes and identity/change
metadata protect each immutable copy. A copy is private and unnamed, so a process crash leaves
no named partial file to sweep. Unsupported systems refuse until they have equivalent primitives.

The Host owns pending requests, quotas and cancellation. Every operation retains the original
runtime, project, opening chat, authority and path. Blocking reads run on the blocking pool.
The API returns a request before copying, then reports copying, verifying, retrying, ready or
refused. Closing or cancelling the file stops both passes. Idle expiry is 120 seconds; maximum
lifetime is ten minutes. A lost client stops refreshing the request and expiry closes it.

## Implementation verification

Independent implementation QA approved the A1 to A3 code on 2026-10-15. Focused tests pass:
49 `ark-files` tests (one separate measurement ignored), 36 `ark-ui` unit tests and nine HTTP
integration tests. Two real WebKit file-viewer scenarios pass, including the shipped Desktop
frame through ARK snapshot transport, map selection, Source switching and STL wireframe.
Desktop's complete JavaScript suite passes 64 tests against the local contract.

HTTP tests prove that only the exact registered frame asset permits same-origin embedding,
with one complete CSP. Other assets and the root still refuse embedding. A sparse GeoJSON
above 25 MiB is refused before copy, quota reservation or chunks. The Desktop independently
checks source size before allocating a transfer buffer.

Full v0.24.0 workspace CI passes, including format, protocol, version, Clippy, normal tests,
55 real WebKit scenarios, the release build and simulated benchmark gate. Development and test
debug information and incremental output were disabled; test optimization was zero. The release
profile was unchanged. An earlier attempt stopped below 1 GiB free. Old unused build output was
then removed; source and dependencies were retained.

Desktop's revised resource probe includes newly spawned macOS WebKit services parented by
launchd. The old 109 to 111 MiB descendant-only estimate was incomplete and is superseded.
The revised hostile-worker peak was 681 MiB. See the consumer's evidence for cap-sized repeated
close cycles and the exact environment. Both repository gates pass: ARK includes 55 real WebKit
scenarios; Desktop includes 22 Rust and 64 JavaScript tests. A developer-packaged Mac app renders
the local map, GLB and CAD-generated STL. Killing its verified WebContent process restores one
Retry; the model renders again after Retry. A Linux ARM64 server build also serves a map and STL
to the native Mac window. Source edit/save/map changes the feature name on Linux. These are
isolated local fixtures, not a production deployment or a signed Desktop release.
The owner deferred five-person observation and physical trackpad testing on 2026-10-15. These
measurements do not prove observed 10/10 usability or broad professional capability.

## CAD feasibility

Used `/Applications/ARK.app/Contents/MacOS/uv` 0.12.23, a fresh virtual environment and fresh
managed Python 3.12.15. `uv pip compile --generate-hashes --only-binary :all:` locked CadQuery
2.8.0 and 43 dependencies. `uv pip sync --require-hashes --only-binary :all:` installed them.

| Measurement | Result |
|---|---|
| Package install | 14.72 seconds. Excludes lock resolution and managed Python download |
| Downloads | About 304 MiB of package downloads from uv's rounded progress output, plus 23.9 MiB Python. Not a packet-level byte count |
| Installed environment | 1,344,736 KiB allocated, plus 70,836 KiB managed Python |
| Retained uv cache | 1,357,020 KiB allocated. Includes hardlinks also counted by the environment; not additive physical disk usage |
| First import and STEP/STL export | 145.08 seconds, 929,103,872 bytes maximum RSS |
| First native sandbox run | 56.18 seconds, after the first import had started concurrently |
| Warm native sandbox and width change | 2.73 seconds, 498,515,968 bytes maximum RSS |
| Geometry | One valid solid, bounds 40 × 30 × 25 mm; regenerated at 50 × 30 × 25 mm |
| Containment | Real `ark_exec::Sandbox::wrap`; outside read, outside write and network denied |
| Wheels, resolution only | macOS arm64 installed. Linux x64/arm64 at glibc 2.31 and Windows x64 resolve with binary-only packages. No run on those systems |
| Linux minimum | Default manylinux 2.28 resolution fails for OCP. The candidate requires glibc 2.31 or later |

`ark-exec/examples/contained.rs` runs the supplied profile probe through the actual native
sandbox without a model. No CAD package enters Core or the ordinary Python environment.

Installed metadata reports CadQuery and OCP as Apache-2.0, VTK as BSD, and CasADi as LGPLv3+.
This is an inventory, not a completed transitive notice or distribution review. Primary
installation reference: <https://cadquery.readthedocs.io/en/stable/installation.html>.

Decision: continue map and model implementation. CAD installation is feasible on this Mac,
but A4 must warm imports during setup, expose the measured optional download, retain required
notices, and pass the isolated profile and clean installed-app proof. No Windows or Linux CAD
execution claim. IFC still waits for observation with a real architecture user.
