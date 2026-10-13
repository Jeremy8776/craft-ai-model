# craft-ai-model

**What this is for:** one AI connection across every Craft app. A person sets up a tool and its model
once, in the launcher or natively in any app, and every app that has that tool uses it with no further
setup. The shared crates own connections, models, the tool to model binding, the model picker, consent
and jobs; each app keeps its own interface for each tool. Where the suggested models or providers do not
suit someone, they can add their own connection, model or tool route and plug it in.

Interim implementation of the shared AI layer for the Craft suite: connections, models,
capabilities, roles, requests, jobs, consent and the launcher's shared store, plus provider
adapters, request history and reusable conformance checks.

Small on purpose: the model uses serde only, no I/O, no async runtime, and compiles for wasm.
Jeremy is driving the shared layer implementation in this interim workspace. We will
propose the smallest common contract to craft-libs for technical review; neither this API nor
its placement there has been accepted yet. See `crates/ai-model/src/lib.rs` for the module map.

A second crate, `craft-ai-native`, holds the desktop-only half: secret stores (OS keychain,
passphrase-encrypted file, environment, layered) and the reader and locked atomic writer for the
launcher's shared store file. The model never depends on it, so the model stays wasm-safe. Real
keychain test: `cargo test -p craft-ai-native -- --ignored keychain_round_trip`.

A third crate, `craft-ai-openai`, is the first real provider: an OpenAI-compatible adapter (text with
streaming, tools and structured output; image generate, edit and inpaint). It passes the shared
conformance checks against a scripted localhost server. Not yet run against a live server or TLS.

A fourth crate, `craft-ai-client`, is what an app holds: `AiClient` with `status`, `plan`, consent,
`start` and `Job`, plus a `ProviderFactory` that new adapters register into. For local Select subject,
`local_route_for` resolves the suite binding to the selected connection, model, provider (ComfyUI or
Candle) and optional address. `endpoint_for` serves ComfyUI only and fails if another backend is
selected; an unavailable adapter is never presented as Ready. This is shared route resolution,
not PhotoCraft or LightCraft inference adoption. Neither app executes its Select subject command
through this binding yet; Candle has no shared production adapter registered in this crate.

Connections are added, edited, tested and removed through `ConnectionDraft` (rules, in the model crate) and
`ConnectionManager` plus `TestRunner` (in the client crate); the launcher's Connections screen and the
`ConnectionPanel` an app shows when there is no launcher both sit on them.

A fifth crate, `craft-ai-egui`, has the shared egui widgets: model status and picker, consent prompt,
and job progress with cancel. Needs Rust 1.95 (egui 0.36).

`craft-ai-comfyui` is a **local-only workflow adapter** with scripted generic-workflow tests
and live-tested SAM 3 point, text and prepared-hover clients. Its shared
`WorkflowLibrary` stores API-format JSON graphs for all apps that use `craft-ai-client`.
It maps a prompt field, and for `image.inpaint` requires explicitly mapped `LoadImage` source
and `LoadImageMask` mask fields connected to the output. Source PNG/JPEG and mask PNG are
uploaded separately to the loopback server using `/upload/image`; the returned safe filenames
are substituted before `/prompt`. Image/video generation workflows remain supported. The adapter
polls `/history/{id}` and retrieves one mapped output through `/view`. Cancellation is off by
default. A caller may opt into targeted cancellation only after confirming that server supports
prompt-scoped interrupts, since an older server might treat it as a global interrupt.
Each configured workflow is offered as one model only after `/object_info` confirms its node
classes and static selector choices, including checkpoint names. Submission repeats the preflight;
it does not guarantee execution will succeed. By default outputs stay inline with a 32 MiB
limit. An app can opt in through `ProviderFactory::with_comfyui_output_dir` to stream output
into its own existing directory, with a 1 GiB maximum and a local `file:` payload handle.
The app owns retention and cleanup. Partial, oversize and redirected transfers leave no asset.
No input file handles, arbitrary image formats, general parameter mapping or generic output
MIME/content verification yet. Generic image/video/inpaint execution and app mounting remain
separate live gates; the dedicated SAM 3 image/session path is already live-tested.

```sh
cargo test -p craft-ai-comfyui -j 8
cargo test -p craft-ai-client -j 8
cargo clippy -p craft-ai-comfyui -p craft-ai-client -j 8 --all-targets -- -D warnings
cargo fmt --all -- --check
```

Layout and rules mirror craft-libs (workspace, edition 2024, MIT OR Apache-2.0, never-crash lints,
no unsafe). This is a public first-draft repository on `Jeremy8776/craft-ai-model`, not merged into
craft-libs. PhotoCraft dev consumes the dedicated SAM 3 client, not the complete shared
role/connection/consent flow; adoption by the other apps remains open.

Which crate does what, what stays in the apps, and how apps consume this repo: [docs/LAYOUT.md](docs/LAYOUT.md).

## Tools and the local runtime

`craft-ai-features` (wasm-safe) is what a launcher or app shows a person: AI tools such as select subject
or generative fill, where each would appear, how it can run on this computer (model files with size, SHA-256,
source and licence; ComfyUI node packs), and hosted options that have an adapter. `plan` compares a tool with
an `Inventory` and says what is left: ready, a download of a stated size, a licence to accept, or a reason it
cannot run. `craft-ai-comfy-env` (native) finds an existing ComfyUI Desktop install, inventories its model files
and node packs, and downloads missing model files into it: resumable, checked against the recipe's size and
SHA-256 before it takes its real name, token sent only to its own host. It reads an existing install and never
starts, installs or repairs ComfyUI. Only select subject (SAM 3) has a local recipe so far. `ToolDesk` (craft-ai-client) lets a launcher choose which connected model does each tool, stored as the launcher default binding that apps inherit, and run a small generated sample to prove it; hosted samples ask first. The existing headless ComfyUI and SAM 3 segmentation/session graphs have passed real tests.
No model download from Hugging Face or automatic node-pack installation has been verified.
The SAM 3 recipe is usable for the current PhotoCraft integration; other app uses and unused
audited capabilities are not automatically shipped features.

## Media-provider coverage

The provider contract accepts open capability IDs, typed prompts, sources, masks and references,
large-media handles, and asynchronous jobs. Scripted-server adapters now cover OpenAI-compatible
text/image, BFL FLUX image/video, Volcengine Ark Seedance video, and Vertex express-mode Gemini
image generation. The older direct SAM 3 detector bridge remains fake-predictor tested.
Separately, `Sam3Point` and `Sam3Session` execute the installed SAM 3 checkpoint through real
headless ComfyUI and are used by PhotoCraft dev. Local ComfyUI workflows
are registered in `ProviderFactory` through the shared library. Vertex express-mode keys are
not full-project Vertex credentials and have no verified free key probe. None of these adapters
has been exercised against its real hosted service. This does not negate the separate
live local SAM 3 clients.

Hosted providers return links that expire. `craft-ai-media` is an opt-in wrapper (`ProviderFactory::with_media_dir`)
that copies such a link into an app-owned directory as the job finishes and returns a `file:` handle. It sends no
credentials, accepts https only (loopback only for a connection the user marked local), re-checks every redirect,
refuses private addresses, bounds the size, and takes the file type from the bytes, not a header. A failed copy keeps
the original link and adds a notice rather than failing the job. Scripted-server tested only: no real delivery host,
TLS, retry, resume or expiry tracking, and no input-side upload cache.

There is no production-ready input-side media upload or subscription-entitlement
route. `craft-ai-artcraft` covers only the ArtCraft Omni API-key route (prompt-only image and
video, scripted-server tested); ArtCraft account or subscription access has no adapter because no
official programmatic route is documented. The ComfyUI adapter's opt-in local output persistence is a narrow offline-tested exception,
not a hosted-asset transfer solution. A saved connection does not mean its image or video operations
are proven live. For a media-capable V1, remaining work includes ArtCraft's separate Omni API route,
generic ComfyUI image/video/inpaint live execution and large-media input handles, hosted
image-edit inputs, robust asset transfer and real-service compatibility checks when authorised. API-key access to ArtCraft's
Omni API is not evidence that a user's ArtCraft subscription covers those requests. Standalone apps
must mount the shared ConnectionPanel in Preferences; the panel alone does not integrate an app.
The living acceptance checklist is `../ai-framework-handoff/index.html` (generated from `build.py`).

## Status

A shared layer in development, with a provisional contract pending craft-libs technical review.
Core/native/client pieces and hosted adapters have scripted tests. Local SAM 3 has real headless
execution and a PhotoCraft dev integration; this does not complete the media V1 or other app
adapters. Hosted compatibility, TLS and the proposed shared-store location remain unverified.
Further work in this slice is local and uncommitted; no publication is authorised.

## Roadmap and feature gates

The canonical suite task list remains `../ai-framework-handoff/index.html`, generated from
`build.py` and `handoff_tasks.py`. The [SAM 3 audit](docs/research/sam3-headless/README.md)
records backend capabilities and limits, not a promise that every audited feature is in an app.

1. **sg1, done:** bounded SAM 3 instance-mask retrieval, explicit union versus individual
   results, count/byte/pixel/dimension limits, fail-whole on a bad later mask, cache-safe repeats.
   Verified live; see `docs/research/sam3-headless/verification/`.
2. **sg2/sg3, done:** typed grounding instances (mask, detector box, score, exact bounds,
   pick-under-pixel); the alternative masks for a point and/or box prompt with scores and 0 to 10
   refinement passes; bounded, exclusive server cache slots per prepared image (an OS file lock
   shared across processes, freed if the holder crashes), revisions that mark stale results, and
   cancellation that touches only the request's own prompt. All live-verified. No UI until the
   crate path works.
3. **p4/p10:** durable assets and long-job reliability. Built for the local ComfyUI adapter: outputs
   and source/mask inputs are checked by their bytes, and inputs may be bounded local file handles
   (scripted tests only), and the media download tries a busy host again with a bounded, doubling
   pause. Open: retries for provider API calls, input handles for the hosted adapters, expiry
   tracking. A saved link is not a retained asset; an input handle is not an upload.
4. **p5/p7/p8:** real source/reference/edit inputs for the distinct ArtCraft, Vertex and BFL API
   families, with scripted conformance first and live hosted tests only when authorised.
5. **u1 to u5 (added 12/10/2026):** custom connections to unlisted providers (u1) and user-chosen
   model sources (u3: Hugging Face, GitHub releases, Civitai, local files; u4: node packs from a
   GitHub address) are built as crates and proven against scripted servers only, with own-host
   tokens, no redirect key leaks, pickle formats consented and unverified checksums flagged.
   The data-only declarative adapter for non-OpenAI API shapes (u2) is designed
   (`docs/design/u2-declarative-adapter.md`) but not built. Launcher screens (u5) are not started.
   Nothing here has run against a real provider, download or repository.
   **Phase 2e v1 (12/10/2026):** a client test shows two app ids reading the one stored
   `select-subject` binding with no per-app setup, and that a tool with no model reports "no model
   is chosen" rather than a fault. No app reads the role yet; the picker and popup (v2, v3) and
   bring your own anywhere (v4) are proposed, not approved.
   **Phase 2e v2 (12/10/2026):** the provider-grouped model picker is built as data
   (`AiClient::picker`) and a theme-neutral widget (`craft_ai_egui::model_picker`), shown on
   sample data with `cargo run -p craft-ai-egui --example picker_demo`. No app uses it yet; the
   in-app missing-model popup (v3) is built the same way: `ToolDesk::missing_model` returns why a
   tool cannot run and the picker data, `craft_ai_egui::missing_model_popup` is a neutral fallback,
   and a choice binds for every app through `ToolDesk::choose`. Each host draws its own view.
6. **sg4/sg5:** per-app object picking, layer-mask/extraction/edit destinations and cropped detail
   selection with explicit coordinate mapping. Other apps remain proposed until adopted.
7. **sg6/sg7/sg8:** native SAM 3.1 evaluation, separate soft-alpha matting, then bounded video
   propagation. No new packs/models by default, and no segmentation-as-matting claim.

V1 still requires a usable configured image operation for each named route, real local image
and video workflows, one hosted long-running video route, consent/auth/unsupported-operation
handling and app-owned asset persistence through the common job path. Audit, crate implementation,
live execution and app adoption are separate milestones. The in-app setup popup and standalone
Preferences mounting remain open; a working SAM 3 test is not proof that those journeys are built.

Audit constraints carried into the roadmap: interactive negative boxes are ignored; grounding
exemplar boxes are not strict ROIs; the custom still-image helper consumes one input image;
hover JPEGs are not exact masks; candidate order is not a semantic label; refinement is not
guaranteed improved quality. Runtime catalogue entries stay executable-only.

## Licence

MIT or Apache-2.0, at your option (`LICENSE-MIT`, `LICENSE-APACHE`).
