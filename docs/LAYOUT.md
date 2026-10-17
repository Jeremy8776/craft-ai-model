# Layout: what lives where

This repository is the runtime half of the shared AI layer for the Craft apps: one connection, set up once, used by
every app that has the tool. It owns connections, models, the tool to model binding, the picker
data, consent and jobs. It does not own any app's interface. Each app decides how a tool looks
(a prompt bar, a one-click button, a click on the image), in its own repository.

## In this repository

| Group | Crates | Owns |
|---|---|---|
| Core | `ai-core`, `ai-native`, `ai-client`, `ai-features` | The data model and provider trait (wasm-safe), secret stores, the shared store and its location, history and consent files, the app-facing client and tool desk, the tool catalogue and setup options. |
| Adapters | `ai-openai`, `ai-bfl`, `ai-vertex`, `ai-artcraft`, `ai-seedance`, `ai-comfyui`, `ai-sam3` | One crate per provider API shape. Each is registered in `ai-client`'s `ProviderFactory`. |
| HTTP | `ai-http` | The one TLS setting every adapter and downloader uses (the platform's certificate check), and a test that fails if any crate builds an agent without it. |
| Engines | `ai-engine`, `ai-engine-candle` | Models run on this computer: the runner contract, registry and shared provider (`ai-engine`), and one crate per engine that registers its models (`ai-engine-candle`, behind `ai-client`'s `candle` feature). A new model or engine never touches an app. |
| Local runtime | `ai-comfy-env`, `ai-media` | Finding an existing ComfyUI, model sources and checksummed downloads, node packs, and durable media transfer. |
| Widgets | `ai-egui` | Theme-neutral egui widgets (status, consent, progress, connection panel, history, model picker) for apps that have no chrome of their own. |

Crates keep the flat `crates/` folder on purpose. PhotoCraft and the launcher reach into it by
path today, so moving a crate breaks their builds.

`docs/design/` holds design notes. `docs/STATUS.md` says what is tested and what is open. The deliberate-bug
scripts are in `tools/mutation/`; everything a script saves goes through `tools/mutation/scrub.py`.

## Not in this repository

| What | Where it goes |
|---|---|
| PhotoCraft's Select subject panel, Generate bar, `ai_cmds`, hover session | `storytold/photocraft`, through its own PR and its maintainers |
| The launcher's Set up AI window, History page and ports | `storytold/craft-launcher` |
| LightCraft's, EffectCraft's and any other app's adapter | That app's repository |
| Anything that draws a toolbar or prompt bar | The app. The shared crates never do. |

The shared code knows an app only as an `AppId` string and as data in the catalogue's `uses`
list. If a change here needs to name an app's behaviour, it belongs in the app.

## How an app uses this

1. Declare a role with the tool's id (the catalogue's `Feature::role()`), for example
   `select-subject`. Do not invent an id of your own.
2. Call `AiClient::status` to grey the tool out, or to say that a model is needed. Never report a
   tool with no model as broken.
3. Depend on a pinned git revision (a tag on `main`), not a path. Path dependencies are for local
   work only and must go before an app PR.
4. A person's choice made for the whole suite is `ToolDesk::choose` (the default every app reads).
   `AiClient::bind` is that one app's own override.

## Branches

- `main` is a state that builds and passes its tests. Apps pin tags on it.
- Work happens on short-lived branches off `main` and returns by pull request. A merged branch is
  deleted, local and remote.
- Review that matters happens at the boundaries: the shared contract in `craft-libs`, and each app
  in its own repository.

## Rules that apply to every change

Test first. A deliberate-bug check for safety rules, with no survivor ignored. Cargo targeted with
`-p`, one process at a time, `-j 8`; no workspace-wide builds. Files under 500 lines. Nothing runs
against a real provider, download or ComfyUI without an explicit go. Say plainly what was run and
what was not.
