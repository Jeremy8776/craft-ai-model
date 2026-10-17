//! One real run of the hosted flow against OpenAI, using the key the launcher saved in the OS
//! keychain. Costs real (small) money for `text` and `image`; `probe` only lists models.
//!
//! ```text
//! cargo run -p craft-ai-client --example live_openai -- probe
//! cargo run -p craft-ai-client --example live_openai -- text
//! cargo run -p craft-ai-client --example live_openai -- image <out.png>
//! ```
//!
//! It works on a copy of the shared store in a temporary folder, so the person's real connections,
//! bindings and history are never written. The key is read through the same keychain code the apps
//! use and goes only into the adapter. Everything printed passes through `Say`, which replaces the
//! key if it ever appears, and a leak check at the end reads back every file this run wrote.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use craft_ai_client::{AiClient, ProviderFactory, RoleStatus, ToolDesk};
use craft_ai_core::{
    Auth, CapabilityId, ConsentLedger, Event, InputRole, JobState, Payload, Request, Role, RoleId,
    SecretString,
};
use craft_ai_native::HistoryFile;
use craft_ai_native::secrets::{KeychainStore, SecretStore};
use craft_ai_native::shared::SharedStore;

/// Prints, after making sure the key is not in the text.
struct Say {
    key: String,
    seen: std::cell::RefCell<Vec<String>>,
}

impl Say {
    fn line(&self, text: impl AsRef<str>) {
        let mut text = text.as_ref().to_owned();
        if !self.key.is_empty() && text.contains(&self.key) {
            text = text.replace(&self.key, "[key removed]");
        }
        self.seen.borrow_mut().push(text.clone());
        println!("{text}");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("probe");

    // The real store, read only. Its bytes are compared at the end to prove it was not touched.
    let real = SharedStore::at_default_location()?;
    let before = std::fs::read(real.path())?;
    let store = real.load()?;
    let connection = store
        .connections
        .iter()
        .find(|c| c.provider == "openai" && c.enabled)
        .ok_or("no OpenAI connection is saved in the launcher")?
        .clone();
    let Auth::ApiKey { secret: secret_ref } = &connection.auth else {
        return Err("the OpenAI connection does not use a key".into());
    };

    let keychain = KeychainStore::new();
    let Some(key) = keychain.get(secret_ref)? else {
        println!("key in the OS keychain: no (nothing to run)");
        return Ok(());
    };
    let say = Say {
        key: key.expose().to_owned(),
        seen: Default::default(),
    };
    say.line("key in the OS keychain: yes");

    // Step 1, free: ask OpenAI which models this key can use.
    let provider =
        ProviderFactory::probing().build(&connection, Some(SecretString::new(key.expose())))?;
    let models = provider.models()?;
    say.line(format!(
        "models this key offers that the layer understands: {}",
        models.len()
    ));
    let wanted: &[&str] = match mode {
        "image" => &["gpt-image-1-mini", "gpt-image-1"],
        _ => &["gpt-4o-mini", "gpt-4.1-mini", "gpt-4o"],
    };
    let model = wanted
        .iter()
        .find_map(|w| models.iter().find(|m| m.id.as_str() == *w))
        .cloned();
    if mode == "probe" {
        let mut ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        ids.truncate(12);
        say.line(format!("first models: {ids:?}"));
        return finish(&say, &real, &before, &[]);
    }
    let Some(model) = model else {
        say.line(format!("none of {wanted:?} is available to this key"));
        return finish(&say, &real, &before, &[]);
    };
    say.line(format!("using model {}", model.id));

    // A temporary copy of the store, with the models just found and a tool for this run.
    let dir = tempfile::tempdir()?;
    let shared = SharedStore::new(dir.path().join("connections.json"));
    shared.replace(&store)?;
    let connection_id = connection.id.clone();
    let listed = models.clone();
    shared.update(|s| {
        if let Some(c) = s.connections.iter_mut().find(|c| c.id == connection_id) {
            c.models = listed;
        }
    })?;
    let (capability, label, inputs) = if mode == "image" {
        ("image.generate", "Smoke: image", vec![InputRole::Prompt])
    } else {
        ("text.complete", "Smoke: text", vec![InputRole::Prompt])
    };
    let role = Role {
        id: RoleId::new("smoke-test")?,
        label: label.into(),
        capability: CapabilityId::parse(capability)?,
        inputs,
    };
    let history_file = HistoryFile::new(dir.path().join("jobs.jsonl"));
    let client = AiClient::new(
        craft_ai_core::AppId::new("smoke")?,
        shared.clone(),
        Arc::new(KeychainStore::new()),
    )
    .with_history(history_file.clone());
    let desk = ToolDesk::new(client, shared.clone(), None);
    desk.register_tool(role.clone())?;
    desk.choose(&role, &connection.id, &model.id)?;

    let client = AiClient::new(
        craft_ai_core::AppId::new("smoke")?,
        shared.clone(),
        Arc::new(KeychainStore::new()),
    )
    .with_history(history_file.clone());
    match client.status(&role) {
        RoleStatus::Ready(r) => say.line(format!(
            "status: ready ({} / {})",
            r.connection_label, r.model_label
        )),
        RoleStatus::Unavailable { reason } => {
            say.line(format!("status: unavailable: {reason}"));
            return finish(&say, &real, &before, &[dir.path().to_owned()]);
        }
    }

    let request = if mode == "image" {
        Request::new(role.capability.clone())
            .with_input(
                InputRole::Prompt,
                Payload::text("a plain red circle on a white background"),
            )
            .with_param("size", serde_json::json!("1024x1024"))
            .with_param("quality", serde_json::json!("low"))
    } else {
        Request::new(role.capability.clone())
            .with_input(
                InputRole::Prompt,
                Payload::text("Reply with exactly one word: pong"),
            )
            .with_param("max_tokens", serde_json::json!(8))
    };
    let plan = client.plan(&role, request)?;
    say.line(format!(
        "consent needed before sending: {} (destination {})",
        plan.needs_consent(&ConsentLedger::default()),
        plan.disclosure().destination
    ));
    // The person said go to this run, so the run grants consent for exactly this plan.
    let mut ledger = ConsentLedger::default();
    ledger.grant(plan.disclosure());
    let job = client.start(&plan, &ledger)?;
    let done = job.wait(Duration::from_millis(500), Duration::from_secs(180))?;
    say.line(format!("job ended: {:?}", short(&done.state)));
    let mut wrote: Vec<PathBuf> = vec![dir.path().to_owned()];
    for event in &done.events {
        match event {
            Event::TextDelta { text } => say.line(format!("text: {text}")),
            Event::Notice { message } => say.line(format!("notice: {message}")),
            Event::Output(o) => match &o.payload {
                Payload::Text { text } => say.line(format!("output text: {text}")),
                Payload::Inline { media_type, data } => {
                    say.line(format!("output: {} bytes of {media_type}", data.len()));
                    if let Some(out) = args.get(1) {
                        std::fs::write(out, data)?;
                        say.line(format!("saved to {out}"));
                    }
                }
                Payload::Handle(h) => say.line(format!("output link of type {}", h.media_type)),
            },
            _ => {}
        }
    }
    if let Some(usage) = done.usage {
        say.line(format!("usage: {usage:?}"));
    }
    let entries = client.history().map(|h| h.entries.len()).unwrap_or(0);
    say.line(format!("history entries written: {entries}"));
    wrote.push(dir.path().to_owned());
    finish(&say, &real, &before, &wrote)
}

fn short(state: &JobState) -> String {
    match state {
        JobState::Failed { error } => format!("failed ({:?}: {})", error.kind, error.message),
        other => format!("{other:?}"),
    }
}

/// Proves the real store was not written and that no file this run wrote contains the key.
fn finish(
    say: &Say,
    real: &SharedStore,
    before: &[u8],
    folders: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    let untouched = std::fs::read(real.path())? == before;
    let mut leaked = say.seen.borrow().iter().any(|l| l.contains(&say.key));
    for folder in folders {
        leaked |= folder_contains(folder, say.key.as_bytes());
    }
    println!("real store untouched: {untouched}");
    println!(
        "leak check over output and every file written: {}",
        if leaked { "KEY FOUND" } else { "clean" }
    );
    if leaked || !untouched {
        return Err("the run did not keep its promises".into());
    }
    Ok(())
}

fn folder_contains(dir: &Path, needle: &[u8]) -> bool {
    let Ok(read) = std::fs::read_dir(dir) else {
        return false;
    };
    read.flatten().any(|e| {
        let p = e.path();
        if p.is_dir() {
            folder_contains(&p, needle)
        } else {
            std::fs::read(&p)
                .is_ok_and(|b| !needle.is_empty() && b.windows(needle.len()).any(|w| w == needle))
        }
    })
}
