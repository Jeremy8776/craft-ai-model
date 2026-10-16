use craft_ai_comfyui::{Binding, WorkflowLibrary, WorkflowSpec};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn graph(prompt_text: &str) -> serde_json::Value {
    json!({
        "1":{"class_type":"CLIPTextEncode","inputs":{"text":prompt_text}},
        "2":{"class_type":"SaveImage","inputs":{"images":["1",0]}}
    })
}

fn spec(id: &str) -> WorkflowSpec {
    WorkflowSpec::import(
        id,
        "image.generate",
        graph("original"),
        Some(Binding::new("1", "CLIPTextEncode", "text")),
        Some("2".to_owned()),
    )
    .unwrap()
}

fn library() -> (WorkflowLibrary, TempDir) {
    let dir = TempDir::new().unwrap();
    let lib = WorkflowLibrary::at(dir.path().join("workflows")).unwrap();
    (lib, dir)
}

fn path_in(dir: &TempDir, name: &str) -> PathBuf {
    dir.path().join("workflows").join(name)
}

#[test]
fn inpaint_bindings_survive_the_shared_workflow_library() {
    let (lib, _dir) = library();
    let bytes = json!({
        "id":"repair", "capability":"image.inpaint",
        "graph":{
            "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
            "2":{"class_type":"LoadImage","inputs":{"image":"old-source.png"}},
            "3":{"class_type":"LoadImageMask","inputs":{"image":"old-mask.png"}},
            "4":{"class_type":"SaveImage","inputs":{"images":["5",0]}},
            "5":{"class_type":"Inpaint","inputs":{"image":["2",0],"mask":["3",0],"prompt":["1",0]}}
        },
        "prompt":{"node":"1","class_type":"CLIPTextEncode","input":"text"},
        "source":{"node":"2","class_type":"LoadImage","input":"image"},
        "mask":{"node":"3","class_type":"LoadImageMask","input":"image"},
        "output_node":"4"
    })
    .to_string();
    let spec = WorkflowSpec::parse(bytes.as_bytes(), None).unwrap();
    lib.save(&spec).unwrap();
    let loaded = lib.load().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].capability().as_str(), "image.inpaint");
    assert!(
        loaded[0].info().capabilities[0]
            .requires
            .contains(&craft_ai_core::model::InputRole::Mask)
    );
}

#[test]
fn a_json_file_lands_in_the_library_and_loads_back() {
    let (lib, dir) = library();
    let file = dir.path().join("kite.json");
    fs::write(
        &file,
        json!({
            "id":"kite-image",
            "capability":"image.generate",
            "graph":graph("original"),
            "prompt":{"node":"1","class_type":"CLIPTextEncode","input":"text"},
            "output_node":"2"
        })
        .to_string(),
    )
    .unwrap();
    lib.import_file(&file).unwrap();
    let workflows = lib.load().unwrap();
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0].id().as_str(), "kite-image");
    let rendered = workflows[0]
        .render(
            &craft_ai_core::Request::new(
                craft_ai_core::capability::CapabilityId::parse("image.generate").unwrap(),
            )
            .with_input(
                craft_ai_core::model::InputRole::Prompt,
                craft_ai_core::payload::Payload::text("a kite"),
            ),
        )
        .unwrap();
    assert_eq!(rendered["1"]["inputs"]["text"], "a kite");
}

#[test]
fn a_bare_api_graph_gets_defaults_and_a_derived_id() {
    let (lib, dir) = library();
    let file = dir.path().join("bare.json");
    fs::write(&file, graph("bare template").to_string()).unwrap();
    let stored = lib.import_file(&file).unwrap();
    assert_eq!(stored, "bare");
    let workflows = lib.load().unwrap();
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0].id().as_str(), "bare");
    assert_eq!(workflows[0].capability().as_str(), "image.generate");
    // Default binding: the only text-input node found.
    let rendered = workflows[0]
        .render(
            &craft_ai_core::Request::new(
                craft_ai_core::capability::CapabilityId::parse("image.generate").unwrap(),
            )
            .with_input(
                craft_ai_core::model::InputRole::Prompt,
                craft_ai_core::payload::Payload::text("a kite"),
            ),
        )
        .unwrap();
    assert_eq!(rendered["1"]["inputs"]["text"], "a kite");
}

#[test]
fn a_ui_format_export_is_refused_with_a_clear_message() {
    let (lib, dir) = library();
    let file = dir.path().join("ui.json");
    fs::write(
        &file,
        json!({"nodes":[{"id":1,"type":"CLIPTextEncode"}],"links":[[1,1,0,2,0,"*"]]}).to_string(),
    )
    .unwrap();
    let err = lib.import_file(&file).unwrap_err();
    assert!(err.message.contains("API Format"), "message was: {err}");
    assert!(lib.load().unwrap().is_empty());
}

#[test]
fn a_second_import_with_the_same_id_replaces_the_first() {
    let (lib, dir) = library();
    let first = dir.path().join("one.json");
    fs::write(
        &first,
        json!({
            "id":"same","capability":"image.generate","graph":graph("old"),
            "prompt":{"node":"1","class_type":"CLIPTextEncode","input":"text"},
            "output_node":"2"
        })
        .to_string(),
    )
    .unwrap();
    let second = dir.path().join("two.json");
    fs::write(
        &second,
        json!({
            "id":"same","capability":"video.generate","graph":graph("new"),
            "prompt":{"node":"1","class_type":"CLIPTextEncode","input":"text"},
            "output_node":"2"
        })
        .to_string(),
    )
    .unwrap();
    lib.import_file(&first).unwrap();
    lib.import_file(&second).unwrap();
    let workflows = lib.load().unwrap();
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0].capability().as_str(), "video.generate");
}

#[test]
fn remove_deletes_the_file_and_only_that_file() {
    let (lib, dir) = library();
    lib.save(&spec("keep")).unwrap();
    lib.save(&spec("drop")).unwrap();
    lib.remove("keep").unwrap();
    let ids: Vec<_> = lib
        .load()
        .unwrap()
        .iter()
        .map(|w| w.id().as_str().to_owned())
        .collect();
    assert_eq!(ids, ["drop"]);
    assert!(!path_in(&dir, "keep.json").exists());
}

#[test]
fn the_library_directory_is_created_on_demand() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("nested").join("workflows");
    let lib = WorkflowLibrary::at(&target).unwrap();
    lib.save(&spec("fresh")).unwrap();
    assert!(target.join("fresh.json").exists());
    assert_eq!(lib.load().unwrap().len(), 1);
}

#[test]
fn a_damaged_library_file_is_reported_and_skipped_not_fatal() {
    let (lib, dir) = library();
    lib.save(&spec("good")).unwrap();
    fs::write(path_in(&dir, "bad.json"), "{ not json").unwrap();
    let workflows = lib.load().unwrap();
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0].id().as_str(), "good");
}

#[test]
fn oversized_files_are_refused_before_reading_the_rest() {
    let (lib, dir) = library();
    let file = dir.path().join("huge.json");
    let mut big = String::from("{\"id\":\"big\",\"capability\":\"image.generate\",\"graph\":");
    big.push_str(&"0".repeat(3 * 1024 * 1024));
    big.push('}');
    fs::write(&file, big).unwrap();
    assert!(lib.import_file(&file).is_err());
}

#[test]
fn workflow_specs_round_trip_through_the_library_file() {
    let (lib, _dir) = library();
    let original = spec("round");
    lib.save(&original).unwrap();
    let loaded = lib.load().unwrap();
    assert_eq!(loaded.len(), 1);
    // The graph and binding survive the round trip, not just the id.
    assert_eq!(
        loaded[0]
            .render(
                &craft_ai_core::Request::new(
                    craft_ai_core::capability::CapabilityId::parse("image.generate").unwrap(),
                )
                .with_input(
                    craft_ai_core::model::InputRole::Prompt,
                    craft_ai_core::payload::Payload::text("checked"),
                )
            )
            .unwrap()["1"]["inputs"]["text"],
        "checked"
    );
}

#[test]
fn a_spec_with_an_unknown_capability_is_refused() {
    let err = WorkflowSpec::import(
        "weird",
        "mesh.sculpt",
        graph("x"),
        Some(Binding::new("1", "CLIPTextEncode", "text")),
        Some("2".to_owned()),
    )
    .unwrap_err();
    assert_eq!(err.kind, craft_ai_core::ErrorKind::Unsupported);
}
