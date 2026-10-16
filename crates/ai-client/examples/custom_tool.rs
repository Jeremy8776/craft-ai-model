//! Adding a tool to the shared AI layer, start to finish, with no app interface and no network.
//!
//! Run it: `cargo run -p craft-ai-client --example custom_tool -j 8`
//!
//! It uses a temporary connections file, so your real connections are not touched. The adapter is
//! a scripted stand-in (see `tests/support/describe_tool.rs`); a real one talks to a model.

#[path = "../tests/support/describe_tool.rs"]
mod describe_tool;

use std::sync::Arc;
use std::time::Duration;

use craft_ai_client::{AiClient, RoleStatus, ToolDesk};
use craft_ai_core::job::Event;
use craft_ai_core::{AppId, ConsentLedger};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;
use describe_tool::{Rig, connection, model, photo_request, tool};

fn main() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let secrets = Arc::new(MemoryStore::new());

    // The launcher (or any app) declares the tool, adds a connection and picks the model, once.
    shared
        .update(|s| s.connections.push(connection(false)))
        .expect("save the connection");
    let launcher = AiClient::new(
        AppId::new("launcher").unwrap(),
        shared.clone(),
        secrets.clone(),
    );
    let desk = ToolDesk::new(launcher, shared.clone(), None);
    desk.register_tool(tool()).expect("declare the tool");
    desk.choose(&tool(), &connection(false).id, &model().id)
        .expect("choose the model");
    println!(
        "declared \"{}\" and chose {} for every app",
        tool().label,
        model().label
    );

    // Each app ships its own adapter and finds the choice in the shared file. Nothing is set up
    // per app.
    for app in ["photocraft", "lightcraft"] {
        let rig = Rig::new();
        let client = AiClient::new(AppId::new(app).unwrap(), shared.clone(), secrets.clone())
            .with_factory(rig.factory());
        let RoleStatus::Ready(ready) = client.status(&tool()) else {
            println!("{app}: not ready");
            continue;
        };
        let plan = client.plan(&tool(), photo_request(4)).expect("plan");
        let job = client
            .start(&plan, &ConsentLedger::default())
            .expect("start");
        let done = job
            .wait(Duration::from_millis(2), Duration::from_secs(10))
            .expect("finish");
        let said: String = done
            .events
            .iter()
            .filter_map(|e| match e {
                Event::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        println!(
            "{app}: {} via {}, {:?}, said \"{said}\", adapter saw {} request",
            ready.model_label,
            ready.connection_label,
            done.state,
            rig.requests().len()
        );
    }
}
