# Custom connections and local sources

This page describes the current u1, u3 and u4 implementation. It records what the code does; it does not
claim that a real provider, download or GitHub repository has been tested.

## Custom OpenAI-compatible connections

A custom connection needs a name, an OpenAI-compatible server address, a provider identifier, and model
entries chosen by the person. Each model entry has an ID and one or more declared capabilities. Billing
can also be recorded. A key is optional; when present, the default header is `Authorization: Bearer`. A
server that expects another key header can use a validated header name instead.

Remote addresses must use HTTPS. HTTP is accepted only for a loopback address on the same computer. The
key is stored separately from the connection record.

The Test action lists models using the saved key. When a non-empty key is present, it repeats the same
model-list request without the key. An authentication refusal on that control request confirms that the
server enforced the key. If the request succeeds without a key, enforcement is unconfirmed. Other
control-request errors also leave key enforcement unconfirmed. The test does not prove billing or model
capabilities.

The model list is not a catalogue of verified features. A returned model ID may have a catalogue
suggestion, but the person must choose the model ID and declare its capabilities. The application does
not infer capabilities from a model name.

## Model sources

The resolver supports a specific file from Hugging Face, a GitHub release asset, a Civitai model version,
or a local file. Remote files are pinned to the resolved revision, tag or version before installation.

| Source | Size and checksum information |
| --- | --- |
| Hugging Face | The repository API supplies the file size. A published LFS SHA-256 is used when present. |
| GitHub release | The release asset metadata supplies the size. A published `sha256:` digest is used when present. |
| Civitai | The download response supplies the exact size, which is checked against the model metadata. A valid SHA-256 from the version metadata is used when present. |
| Local file | The file size is read from disk. The staged copy is hashed during installation and recorded as local. |

Some remote sources do not publish a SHA-256. In that case, installation is marked **observed, not
verified**: the app records the digest it calculates from the received bytes, but it cannot compare that
digest with one published by the source. The person must consent before this unverified install proceeds.
A digest of received bytes is a record of those bytes, not proof of who supplied them.

A licence must also be accepted before install when the source marks it as needing acceptance. That means
a gated Hugging Face repository, a Hugging Face or GitHub licence that is not on the short permissive
list (Apache-2.0, MIT, BSD-2-Clause, BSD-3-Clause, ISC, Unlicense, CC0-1.0), and always a Civitai file or
a local file. Acceptance is remembered for that exact version under those exact terms, so a new version
or changed terms ask again. Each local file is asked about separately.

Files with `.safetensors`, `.gguf` and `.onnx` extensions are treated as non-pickle formats. Files ending
in `.ckpt`, `.pt`, `.pth`, `.bin`, `.pkl` or `.pickle` can run code when loaded by another program, so
their installation requires explicit consent. The installer checks the filename against the first bytes
where the format has a recognisable signature. It does not load the model file. ONNX has no reliable
signature in this implementation.

## GitHub node packs

The node-pack resolver accepts an HTTPS address on `github.com` for a public repository, optionally with
a branch or tag. It resolves that reference to a full commit SHA and shows the commit before
installation. Installation consent is tied to that exact SHA. If the reference moves, consent for the
older commit does not approve the new one.

The archive reader treats the downloaded tarball as hostile. It refuses links, device and other special
files, paths outside a single top-level folder, invalid archive checksums, conflicting names, case-only
duplicate names, reserved Windows names, and archives that exceed the configured size, file-count or
depth limits. Files are created as ordinary files and are not marked executable. An existing destination
folder is not replaced.

Nothing in the pack is run. A top-level `requirements.txt` is only recorded as present; no packages are
installed. A top-level `install.py` or `setup.py` is only recorded as present; neither script is run. The
app does not install Python dependencies or execute a node-pack installer.

## Verification limits

These paths are covered by tests using scripted localhost servers and temporary folders. They have not
been tested against a real provider, a real model download, a real GitHub repository or an external
ComfyUI installation.
