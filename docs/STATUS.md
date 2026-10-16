# Status: what runs, what is tested, what is open

Three levels, kept apart on purpose. A saved key, a static catalogue, a passing scripted test and a live
job prove different things.

## Built and tested against scripted servers only

No real hosted service, download host or repository was contacted by these tests.

| Area | Crate | Notes |
|---|---|---|
| OpenAI-compatible text and images | `craft-ai-openai` | Streaming, tools, structured output; image generate, edit and inpaint. |
| FLUX image and video | `craft-ai-bfl` | |
| Seedance video through BytePlus ModelArk | `craft-ai-seedance` | The Volcengine China address is refused. |
| Gemini images on Vertex express mode | `craft-ai-vertex` | An express-mode key is not a full-project Vertex credential and has no free key probe. |
| ArtCraft Omni API key | `craft-ai-artcraft` | Prompt-only image and video. Account or subscription access has no adapter: no official programmatic route is documented. An API key is not evidence that a subscription covers the requests. |
| Hosted media copies | `craft-ai-media` | https only (loopback only for a connection marked local), every redirect re-checked, private addresses refused, size bounded, file type taken from the bytes. A failed copy keeps the original link with a notice. A busy host is retried with a bounded, doubling pause. Not built: retries for provider API calls, expiry tracking, resume, and an input-side upload cache. |
| Generic ComfyUI workflows | `craft-ai-comfyui` | Maps a prompt field; `image.inpaint` needs mapped `LoadImage` and `LoadImageMask` fields. A workflow is offered as a model only after `/object_info` confirms its nodes and checkpoint names; submission repeats the check. Output stays inline (32 MiB) unless the app gives a directory (1 GiB, local `file:` handle, app owns cleanup). Targeted cancel is off by default because an older server may treat it as a global interrupt. No input file handles, general parameter mapping or generic output content verification yet. |
| Declarative adapter | `craft-ai-declarative` | Request/response and queue flows from a data file; prompt-only inputs. No launcher screen to add a spec yet. |
| Custom connections, model sources, node packs | `craft-ai-core`, `craft-ai-comfy-env` | See `docs/custom-connections-and-sources.md`. Own-host tokens only, no key sent across a redirect, pickle formats need consent, unverified checksums are flagged. |
| Consent, history, shared store, key handling | `craft-ai-core`, `craft-ai-native`, `craft-ai-client` | Consent is scoped to app, connection, destination and capability, per kind of data, and may expire. `craft-ai-egui`'s prompt does not offer the consent terms yet. |

## Run for real

Only the local SAM 3 clients (`Sam3Point`, `Sam3Session`): point, box, text, candidate masks, grounding
instances and prepared hover sessions, through a headless ComfyUI on one Windows machine. The live-test
evidence is not kept in this repository. This does not make the generic ComfyUI image or video workflows
live-tested.

## Not verified

- Any real hosted provider, in any adapter.
- TLS against an intercepting proxy or antivirus HTTPS scanner. `craft-ai-http` trusts the platform's
  certificate store, so such software can read traffic, API keys included, and there is no switch back
  to bundled roots.
- macOS (CI runs the tests on Linux and Windows), and the macOS and Linux keychains. The real keychain
  test is `#[ignore]`d.
- Model download from Hugging Face and automatic node-pack installation against the real services.
- The built-in recommended-models catalogue (`catalog.json`) is empty. Which models to recommend per tool
  is a curation decision, so "Recommended" never appears until someone makes it.
- App adoption. PhotoCraft's dev build uses the SAM 3 clients directly, not the shared role, connection
  and consent flow. A route-resolution test or a Ready badge is not inference adoption.
- The keychain is no longer called while the shared file lock is held, but how long a slow real keychain
  takes is unmeasured.
- The encrypted-file secret store is not in `ConnectionManager::standard()`. It layers the environment and
  the OS keychain only, so on a machine with no keychain (a bare Linux session) a key cannot be saved
  until an app supplies its own passphrase and builds a `LayeredStore` with `EncryptedFileStore`.

## Design limits

- The runtime is in-process: apps link `craft-ai-client` and run adapters themselves. A daemon with IPC
  between apps and the adapters has been suggested by the maintainers and is a separate discussion; no
  wire format is defined.
- A registered tool is a declaration, not installed code. No dynamic adapter registry for arbitrary user
  code, no general tool executor and no automatic model installation.
- Interactive negative boxes are ignored, grounding exemplar boxes are not strict regions, hover JPEGs
  are not exact masks, candidate order is not a semantic label, and refinement is not guaranteed to
  improve quality. Runtime catalogue entries stay executable-only.
