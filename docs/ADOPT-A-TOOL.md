# Add a service or a tool, and use it from an app

For someone building on this layer: you ship a service, a kind of tool, or an app that uses them.
You should not need to edit this repository or anyone else's app. This page is the path; the
runnable version of every step is `crates/ai-client/tests/third_party_adoption.rs`, which CI runs,
so if this page and the code disagree the test is right.

The jobs, each independent of the others:

| You want to | You write | You touch nothing else |
|---|---|---|
| Offer a hosted service of a known shape | One settings file (`craft-ai-declarative`, see `docs/design/declarative-adapter.md`) | No Rust |
| Offer any other service | A `Provider`, registered by name | The apps that do not ship it just report it unavailable |
| Offer a new kind of tool | A `Role`: id, label, one capability, the inputs it needs | Any model that offers that capability and those inputs can be chosen for it |
| Use a tool in an app | `status`, `plan`, consent, `start`, poll | No keys, no stores, no consent logic, no setup screen of your own |

## 1. A service

Implement `craft_ai_core::Provider` (models, submit, poll, cancel). Prove it with the shared
checks, which any provider must pass:

```rust,ignore
use craft_ai_core::conformance::{run_lifecycle, run_cancel, run_rejects, run_unknown_job};
run_lifecycle(&provider, &model, &request)?;   // every transition legal, terminal state stable
run_cancel(&provider, &model, &request)?;      // cancel ends in Cancelled, twice is fine
run_unknown_job(&provider)?;                   // an unknown job is an error, not a hang
run_rejects(&provider, &model, &bad, ErrorKind::InvalidRequest)?; // refused before any send
```

A provider is built with its key already resolved, so it never touches a keychain. Register it by
name where the app builds its client:

```rust,ignore
let mut factory = ProviderFactory::standard();
factory.register("acme", |connection, key| Ok(Arc::new(AcmeProvider::new(connection, key)?)));
let client = AiClient::new(app, shared_store, keychain).with_factory(factory);
```

An app that does not register `acme` is not broken: for that service it reports "This app can't
run the chosen model yet. Choose another in Set up AI.", asks for no consent and reads no key.

## 2. A tool

A tool is a declaration, not code:

```rust,ignore
let role = Role {
    id: RoleId::new("describe-photo")?,
    label: "Describe photograph".into(),
    capability: CapabilityId::parse("image.describe")?,
    inputs: vec![InputRole::Source],
};
desk.register_tool(role.clone())?;                       // once, in the shared store
desk.choose(&role, &connection_id, &model_id)?;          // refused if that model cannot do the job
```

Every process that reads the same store sees it. Do not give an existing id a different meaning:
an id's capability and inputs are its contract, and a mismatch is refused. Built-in tools live in
the catalogue (`craft-ai-features`); one entry there adds a tool with a setup recipe too.

## 3. An app

```rust,ignore
if let RoleStatus::Unavailable { reason } = client.status(&role) { /* say reason, stop */ }
let plan = client.plan(&role, request)?;     // checks the request and that this app can run the choice
if plan.needs_consent(&ledger) { /* show plan.disclosure(); ledger.grant(plan.disclosure()) */ }
let job = client.start(&plan, &ledger)?;     // re-checks consent; reads the key only now
let done = job.wait(interval, timeout)?;     // or poll(); job.cancel() at any time
```

What the layer does for you, and the test pins it: consent is per app, connection, destination and
kind of data, so agreeing in one app is not agreeing in another; a service this app cannot run is
refused at `plan`, before anyone is asked to agree; the key is read after consent and never for an
adapter that is not there; a store file written before custom tools existed still loads and its
content survives the next write.

## Checking you did it right

Run `cargo test -p craft-ai-client --test third_party_adoption` for the whole story, and read it
once cold: if a step needs something this page does not say, that is a bug in this page, so please
fix the page.
