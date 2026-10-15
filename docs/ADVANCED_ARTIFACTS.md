# Advanced artifact surfaces

Plan for maps, 3D models, CAD and later domain files to open in ARK without adding a mode,
destination or setup flow. This is a proposal. It does not describe released behavior.

## Product contract

A person asks for work in the chat. The result opens in the existing side pane with the view
that fits the file. The person can inspect it, point at part of it and continue the same chat.

| Request | Result |
|---|---|
| "Map these sites and mark the two outside the service area" | A GeoJSON file opens as a map. The source file remains in the project |
| "Make a bracket for this load and these dimensions" | The contained CAD runtime writes editable source, STEP and a tessellated preview. The preview opens as a 3D model |
| "Review this building model and find the mechanical room" | The IFC file opens in the same 3D surface, with the selected element and properties available to the chat |

The normal interface gains no `Map`, `CAD`, `Architecture` or `Defense` mode. It gains no new
rail item. File cards, tabs, versions, remote routing and the conversation stay as they are.
Only a view-local toolbar appears when that file is open.

The words are ordinary: `Map`, `3D model`, `Fit`, `Measure`, `Layers`, `Source`, `Open in its
app`. Runtime names, renderers, formats and conversion steps stay out of the normal interface.

## Current gap

ARK decides a file kind once and already has one view per kind. Text, Markdown, sheets,
documents, PDFs and images follow that path. Apps can add lenses over stored artifacts, but not
over live project files. Binary file transfer is a single base64 reply capped at 25 MB. That is
not enough for a large model and it makes a third-party file viewer duplicate the file path.

The current advanced-file experience scores 4/10 under the workspace design diagnostic:

| Test | Today | Target |
|---|---|---|
| Discoverable | An unsupported file has no useful in-ARK action | It opens in the right view without a mode choice |
| Visible result | The person cannot evaluate a map or model in ARK | Geometry, state, selection and errors are visible |
| Recovery | `Open in its app` may work locally | Invalid, large and unsupported files keep the source and offer a useful recovery |
| Natural mapping | Files already open beside the chat | Controls stay on the map or model they affect |
| Constraints | Files do not execute as programs | Renderers also refuse external resources and bound decoded work |

The release target is an observed 10/10. Written acceptance checks are necessary but do not earn
that score. Five unfamiliar people must complete the first-use test below without instruction.

## One generic seam

Do not add a branch for each profession. Extend the existing file contract once, then prove it
with two different file families.

```text
project file
  -> ark-files decides kind once
  -> ark-ui routes the file to a registered file viewer
  -> viewer reads bounded bytes from the file's own runtime
  -> selection returns through Focus to the current chat
```

| Layer | Change | Boundary |
|---|---|---|
| `ark-files` | Add person-visible `map` and `model` kinds. Start with `.geojson`, `.glb` and `.stl`. Keep each source file as truth | No architecture, legal, military or vendor concept |
| `ark-ui` Rust host | Add immutable file snapshots and versioned, chunked reads. Keep project, runtime and chat authority on every request | No filesystem URL and no local fallback for a remote file |
| `ark-ui` page | Add `ark.fileViewer` beside `artifactViewer`. A viewer matches a reported kind, renders in the existing file tab and returns focus | No new navigation. Missing viewers retain the existing unavailable and external-app recovery |
| ARK Desktop extension | Register the offline map and 3D viewers and their pinned assets | Viewer calls use the narrow file-viewer API. The shipped same-page module is trusted and still has page authority |
| Consuming app capability | Register a contained CAD runtime and its skill | CAD packages and prompts do not enter ARK Core |

### Classification and immutable reads

`.geojson` is a text format with kind `map`. It must be UTF-8 JSON and its top-level value must be
a GeoJSON object. It stays editable through the existing text write path, including its version
guard. The first release keeps the current 25 MB structured-file limit.

`.glb` and `.stl` are view-only opaque formats with kind `model`. Add a view-only descriptor
beside `Format`: extension, kind, media type and a bounded magic preflight. It does not decode the
file and it does not implement `Edit`. `kind_of` and the file listing use this descriptor without
reading the bytes. `file_read` returns the kind, size and `view_only: true`; it does not try to
map the binary file into `Content`.

Do not hash the live file again for every chunk. Snapshot creation takes ARK's per-path shared
read lease, opens the authorized source, captures its platform file identity, size and change
metadata, then streams it into an ARK-owned temporary file while computing BLAKE3. It rewinds the
same handle and hashes the source a second time. It accepts the snapshot only when both hashes,
byte counts, identity, size and change metadata agree before, between and after the passes.

The lease blocks ARK's own edit, restore and run-output publication during the copy. Opening the
source handle keeps an atomic replacement on its original inode or file id. The second pass
detects an external in-place overwrite or truncation. On a mismatch, the host deletes the partial
snapshot and retries from a new handle up to three times. It then refuses with `The file is
changing; try again when it is saved`. A platform that cannot provide a stable identity and
change metadata must refuse the snapshot until it has an equivalent lock or snapshot primitive.
The complete immutable copy exists before its token is returned.

Initial additive calls:

| Call | Result |
|---|---|
| `file_snapshot_open {path}` | `{snapshot, kind, format, media_type, version, size, chunk_size, expires_ms}` |
| `file_snapshot_chunk {snapshot, offset}` | `{snapshot, offset, data, eof}` with at most `chunk_size` bytes as base64 |
| `file_snapshot_close {snapshot}` | Closes the token and removes its temporary file |

The random snapshot token is scoped to the current resource route, project and opening chat. It
cannot be used from another runtime, project or chat. Initial prove-or-change limits are 256 MiB
per model, 1 MiB per chunk, two in-flight chunk reads per viewer, eight open snapshots and 512
MiB of snapshot files per runtime. An idle token expires after two minutes and no token lives
past ten minutes. Closing the tab, switching its resource or cancelling loading closes it. The A0
measurements may lower these constants. Raising them requires new memory, disk and latency
evidence.

A file-change event closes the old token and starts a new snapshot. A late chunk names the old
token and is discarded. Tests replace the source between every possible pair of chunks and prove
that the assembled bytes equal either the complete old version or the complete new version,
never a mixture. Another test overwrites and truncates the same inode during
`file_snapshot_open`: the host retries or refuses, returns no token for the unstable copy and
removes every partial file. Interrupted copies leave no token and their temporary files are swept
at start.

The protocol later exposes these calls as optional capabilities for native clients. This work
does not overload `file_media` or increase its 25 MB base64 reply.

`ark.fileViewer` is the only new page extension:

```js
ark.fileViewer({
  id: "model",
  kinds: ["model"],
  render(el, file, api) { /* returns focus, update and dispose */ }
});
```

The API supplies metadata, `read(offset)`, `cancel`, `changed`, `setFocus` and ordinary DOM
helpers. It does not supply a filesystem URL or a way to mutate the file. The registered viewer
module still runs in the same page realm and can reach `window.ark`; it is trusted shipped code,
not a security sandbox. File bytes are untrusted and must be parsed in a worker or stronger
sandbox. Editing a format needs a separate typed `ark-files::Edit` design and a real second use
case.

Focus is also bounded input to the model. A viewer may report a stable selection id, a 120
character label, at most 16 scalar properties with keys up to 64 characters and values up to 256
characters, plus a geometry type, count and bounding box. The serialized selection is capped at
2 KiB. It never includes full geometry, a property blob or arbitrary markup. Host tests use
hostile feature properties and prove the bound before the focus enters a turn.

## Delivery order

### A0. Contract and hostile fixtures

Write proposed ADR 0071 before product code. Add small fixtures for valid, malformed, oversized
and externally-referencing GeoJSON, GLB and STL files. Record current open time and memory on a
small and a near-limit model. Decide limits from those measurements rather than intuition.

In parallel, run a throwaway CadQuery feasibility spike through the current bundled `uv` path on
a clean Apple-silicon profile. Record download bytes, installed bytes, setup time, first and warm
run time, package notices, STEP/STL export and whether pinned wheels exist for every supported
Desktop system. This spike may write fixtures but no product surface. If it cannot install from a
hash-locked environment, exceeds the agreed size, or cannot run inside the existing sandbox,
stop A4. The fallback is shipping map and model viewing while CAD generation waits for a reviewed
runtime or installed-application adapter.

Done means:

- Existing file and artifact tests stay byte-compatible.
- A remote project cannot be read through a local path or another chat's authority.
- A malformed file cannot execute script, fetch a URL or allocate without a bound.
- The file remains available when its renderer fails.
- The CadQuery spike has a recorded go or stop decision. Candidate documentation is not treated
  as proof that the runtime can ship.

### A1. Generic file viewer and byte transport

Add the `map` and `model` kinds, the chunk call and `ark.fileViewer`. Keep built-in text, sheet,
document, PDF and image views unchanged. Add one test viewer to ARK's WebKit fixture before the
real renderers.

Done means:

- A registered viewer opens a local or remote file through the same tab and resource route.
- Switching projects, chats or computers cancels late chunks from the former resource.
- Save or agent write reloads the viewer by version without losing the chat.
- Missing viewer, unsupported WebGL, invalid bytes and renderer crash each show one recovery.
- Loading shows bytes and percentage after one second, stays cancellable and never replaces the
  last good view. A version change says `File changed, loading the new version`.
- Keyboard, 420 px, light, dark and reduced-motion states pass. A page reload and native process
  restart recover without keeping a stale snapshot.

### A2. Offline map, GeoJSON first

Bundle a pinned MapLibre GL JS build in ARK Desktop and register one `map` file viewer. MapLibre
uses WebGL and accepts GeoJSON sources. The first release uses an offline neutral style with no
tile URL. A connected basemap is a later capability with its own provider, attribution, privacy
text and network boundary.

MapLibre code is trusted Desktop code. GeoJSON is untrusted and is validated and summarized in a
worker before it reaches MapLibre. CSP and a request interceptor deny every renderer-originated
network request. Worker creation, blob and image rules are the minimum required by the pinned
build and are covered by a packaged-app test.

The initial view has Fit, Reset, Layers and feature selection. It renders points, lines,
polygons and their declared styles. Selecting a feature returns its stable id, properties and
geometry summary through Focus, so "change this route" refers to the visible selection. The
agent edits the GeoJSON source through the existing text/file path. The map reloads from the new
version.

Cut from A2: geocoding UI, live tracking, satellite imagery, a tile marketplace, drawing tools,
KML, Shapefile and spatial databases.

Acceptance:

- A prompt creates a valid map, ARK opens it and Fit shows every feature.
- A point, line and polygon can be selected by mouse and keyboard.
- With the network disabled, the view makes zero outbound requests and remains useful.
- Invalid coordinates identify the feature and preserve the source.
- 50,000 simple features remain interactive at the measured target, or the measured lower cap
  is enforced with a plain recovery.
- A remote Linux runtime can produce the file and the Mac window renders those remote bytes.

### A3. 3D model, GLB and STL first

Bundle a pinned Three.js build and only the loaders needed for binary GLB and binary or ASCII
STL. Both are treated as single-file formats. Parse them in a worker after a header preflight.
For GLB, reject external buffer or image URIs, decoder extensions that are not bundled (including
Draco, Meshopt and KTX paths in the first release), oversized embedded images and decoded data
above the vertex, triangle, texture and memory caps measured in A0.

The first view has Orbit, Pan, Fit, Reset, Orthographic/Perspective, Wireframe and Measure. Orbit,
Pan and Measure are visible modes with one selected state. `Escape` leaves Measure or cancels the
current pick. Reset restores the initial camera and Orbit. It shows units only when the source or
companion metadata declares them. Tessellated measurements are labelled as measured from the
mesh. Reduced motion replaces camera interpolation with an immediate move.

The viewer reports the selected object or triangle, bounding box, declared units and measured
points through Focus. It disposes geometries, textures, workers and WebGL state when the tab
closes or changes version.

Cut from A3: animation editing, materials editor, external GLTF resources, OBJ/MTL, point clouds,
STEP parsing, section cuts and collaborative annotations.

Acceptance:

- Known GLB and STL fixtures open, frame correctly and keep orientation across a pane resize.
- Mouse, trackpad and keyboard can orbit, pan, zoom, fit and select.
- Two picked points show a repeatable distance and the stated units or `units not declared`.
- External URIs, data beyond the decoded cap, excessive triangles and oversized textures fail
  before resource exhaustion.
- WebGL loss has a recoverable state. Closing the tab returns memory near the measured baseline.
- Local and remote files use the same viewer and source version.
- A worker parse failure cannot crash the page. A forced worker failure and a forced native page
  process restart recover to the file card with one Retry action.

### A4. Contained parametric CAD generation

Do not teach the base prompt CAD syntax. ARK Desktop is the first consumer. After the A0 spike
passes, ARK Core may add only a neutral hash-locked Python runtime profile seam in `ark-exec`.
ARK Desktop owns `src/cad.rs`, `runtime/cad/pyproject.toml`, `runtime/cad/uv.lock` and
`skills/cad/SKILL.md`. It registers a runtime named `cad` with a focused skill. CadQuery is a
Python parametric CAD framework that exports STEP and STL, and assemblies can export glTF.

Desktop's bundled `uv` installs the exact profile lazily under its existing data root at
`exec/cad-v1`. The approval says the download and installed size measured in A0. Setup progress
and failure appear in the existing turn, not a new Settings section. Desktop owns cleanup: an
Advanced storage action removes an idle current environment, and an upgrade removes superseded
profiles only after no run holds them. Uninstall follows the app's existing user-data choice.
Before release, Desktop's third-party notices include CadQuery, OCP and every redistributed or
downloaded dependency required by their licenses. The current CadQuery project reports
Apache-2.0, but the transitive review remains a release gate.

Delivery is ARK first: implement and release the generic file-viewer, snapshot and optional
runtime-profile seams, then pin that immutable ARK tag in ARK Desktop. Desktop then adds the
viewer assets, CAD profile, skill and clean-machine proof. No committed `path` or `main` pin.

The runtime writes a predictable set:

| File | Purpose |
|---|---|
| `<name>.py` | Editable parametric source and named dimensions |
| `<name>.step` | Exchange and manufacturing file |
| `<name>.stl` or `<name>.glb` | In-ARK preview |
| `<name>.json` | Units, requested dimensions, measured bounds and generation checks |

The existing sandbox still removes network, limits time and writes only in the project. A
separate CAD profile avoids adding OCCT and CadQuery to every ordinary Python setup. The profile
is ARK Desktop application code, not an `if cad` branch in ARK Core.

The first proof is a dimensioned mounting bracket. The agent must create it, export STEP and STL,
open the preview, compare measured bounds with the request and report any mismatch. A follow-up
changes one dimension by editing the source and regenerating all outputs. Existing file versions
provide Restore.

Cut from A4: finite element claims, automatic code compliance, tool paths, slicer control,
manufacturing certification and unattended machine operation. ARK must say that generated
geometry requires professional review where safety depends on it.

Acceptance:

- A clean supported Mac prepares the pinned environment after one explicit run approval.
- The bracket proof produces source, STEP, preview and check metadata with no network in the run.
- Changing one dimension changes the measured preview and keeps unrelated source parameters.
- A failed build keeps the last good outputs and shows the useful compiler error.
- The runtime cannot read outside approved runtime files and the project, or write outside the
  project and its scratch directory.

### A5. IFC as the first professional model lens

After the generic 3D proof, add `.ifc` to `model` through a pinned `web-ifc` WebAssembly parser.
It reads geometry and properties in a worker. The same 3D surface gains a model tree, property
panel and storey/category filters only when an IFC file supplies them. No separate BIM mode.

Cut from A5: authoring IFC, clash detection, BCF collaboration, quantity certification and a
full BIM replacement. Those require real architecture users and separate checks.

Acceptance:

- Open one small and one near-limit public IFC fixture offline.
- Select an element from the model or tree and return its IFC id, type and bounded properties
  through Focus.
- Filter by storey and category without reparsing the file.
- A malformed or unsupported schema keeps the source and reports the exact parser limit.

## Safety and resource limits

| Risk | Required constraint |
|---|---|
| Untrusted parser input | Pinned renderer, worker boundary where available, fuzz fixtures and no HTML execution |
| Hidden network | Content Security Policy and request interception deny renderer-originated network. Maps start without a basemap |
| External model references | First release accepts single-file GLB and STL only. External URI resolution is refused |
| Memory and GPU exhaustion | Bound source bytes, decoded vertices, triangles, textures, features and concurrent viewers. Dispose on close |
| Path escape | Existing `ark_files::inside` and resource authority apply before every chunk |
| Stale or mixed content | Every chunk names one version. Any version change cancels the assembly |
| Wrong units | Never infer units silently. Show declared units or say they are not declared |
| Safety-critical reliance | No structural, legal, operational or manufacturing certification claim. Keep source, checks and review visible |

Exact limits are implementation decisions after the A0 measurement. They must be constants with
tests and plain errors, not undocumented renderer defaults.

## Interaction states and first-use evidence

| State | Person sees | Recovery or constraint |
|---|---|---|
| Preparing snapshot | File name, bytes copied and Cancel after one second | Cancel closes and removes the partial snapshot |
| Parsing | Determinate progress where the parser reports it, otherwise bounded `Opening` state | Cancel terminates the worker |
| File changed | `File changed, loading the new version` | Last good view stays until the replacement is ready |
| Invalid file | What could not be read, without parser internals | Source, Retry and Open in its app when local |
| Refused by limit | The exact size, feature or geometry limit | Source remains; no partial scene is shown |
| WebGL unavailable | `This model cannot be shown on this computer` | Open in its app; the chat and file stay available |
| WebGL context lost | Model pauses and says why | Retry rebuilds from the same immutable snapshot |
| Worker or page crash | File card and one Retry after reload | No stale token or duplicated write |
| 420 px | Canvas keeps the object; tools collapse into one labelled menu | Fit, selection, Cancel and recovery remain reachable |
| Keyboard | Tab reaches the canvas and toolbar; arrows manipulate the selected operation | Escape exits Measure or cancels a pending pick; Reset is always reachable |
| Dark or reduced motion | Contrast remains legible; camera changes are immediate under reduced motion | No information relies on animation or colour alone |

The observed first-use test uses five people who have not seen the design. Without instruction,
each must open the generated file from the chat, fit it, select a feature or part, ask ARK to
change that selection and recover from one supplied bad file. The observer records hesitation,
wrong actions and whether the person can state what changed. All five diagnostic rows pass only
when the behavior, not an explanation, supplies discoverability, feedback, recovery, mapping and
constraints. Until then, report the measured score below 10/10 and fix the failed row.

## What makes this repeatable

The artifact work should not wait for a new learning system. It should produce the evidence that
system will later reuse:

- the accepted source file and outputs;
- the person's corrections and selected features or parts;
- the exact runtime and pinned package versions;
- deterministic generation checks;
- the successful prompt and project rules.

After ARK has observed the same successful pattern twice, a later learning change may offer
`Do this again` and then `Run when...`. It must save an inspectable project routine, not silently
rewrite the base prompt. Maps, models and CAD use the existing schedule and proposed event
automation path. They do not get a second automation engine.

## Delivery gates and targets

These are engineering targets, not public dates or launch commitments.

| Target | Scope | Gate |
|---|---|---|
| First 48 hours | Prove-or-stop A0 and A1 spike, one GeoJSON and one STL fixture in a registered test viewer | Stop before product UI if immutable snapshot routing, worker isolation or bounded parsing is unresolved. Plan and design QA must pass |
| First useful slice | A1 and A2 | Full ARK checks, Desktop checks, real offline map dogfood, 10/10 design diagnostic |
| First visible 3D slice | A3 | Real GLB and STL dogfood, memory and hostile-file evidence |
| First generated CAD proof | A4 bracket workflow | Clean-machine setup, contained run, dimensions checked on outputs, source revised once |
| First professional expansion | A5 IFC | Real architect or BIM operator observes the task. Do not infer need from the demo alone |

Do not hold the existing launch for A5. A2 or A3 may become a launch proof only if the released
build, clean-machine run and truth pass finish before launch authority. Otherwise ship them as
the first product update.

## Market proof

The first external claim is narrow: ARK can create and inspect a map, or create a parametric CAD
part and let the person inspect the result without leaving the work. Do not claim broad domain
mastery.

For each proof, record:

- whether the person completed the task without choosing a mode;
- whether they opened or manipulated the result;
- whether they could request a correction by referring to a visible feature or part;
- whether the corrected output passed its deterministic checks;
- whether they returned with another task in the same workflow.

Two useful demonstrations are enough for the architecture gate: an offline operations map and a
dimensioned bracket. Expansion to another format requires a real task and a person who needs it.

## Dependency evidence reviewed 2026-10-15

| Candidate | Use | Current primary evidence |
|---|---|---|
| MapLibre GL JS | Offline GeoJSON map surface | WebGL map library, local GeoJSON support and BSD-3-Clause package: `https://maplibre.org/maplibre-gl-js/docs`, `https://github.com/maplibre/maplibre-gl-js/blob/main/package.json` |
| Three.js | GLB and STL view | Official STL loader accepts binary and ASCII STL from an `ArrayBuffer`: `https://threejs.org/docs/pages/STLLoader.html` |
| CadQuery | Parametric CAD generation candidate | Python CAD framework with STEP, STL and assembly glTF export; project reports Apache-2.0: `https://cadquery.readthedocs.io/en/stable/`, `https://github.com/CadQuery/cadquery` |
| web-ifc | Later IFC lens | Browser and Node WebAssembly parser, MPL-2.0: `https://thatopen.github.io/engine_web-ifc/docs/`, `https://github.com/ThatOpen/engine_web-ifc` |

Pin exact versions and preserve required notices only after an implementation spike passes the
security, bundle-size, platform and performance checks. A current project page is evidence of a
candidate, not approval to ship it.
