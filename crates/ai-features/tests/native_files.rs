//! The built-in runtimes read their own checkpoint, not the ComfyUI one. These pin that the
//! catalogue says so with complete, checkable data.
use craft_ai_features::{Feature, Source};

fn select_subject() -> Feature {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap()
}

#[test]
fn every_built_in_sam3_runtime_names_its_own_complete_files() {
    let feature = select_subject();
    let recipe = feature.local.as_ref().unwrap();
    let comfy: Vec<&str> = recipe.models.iter().map(|m| m.filename.as_str()).collect();
    for route in recipe.native.iter().filter(|r| r.provider == "candle") {
        let weights = route.weights().unwrap();
        assert_eq!(weights.filename, "model.safetensors", "{}", route.provider);
        assert!(!comfy.contains(&weights.filename.as_str()));
        // The weights and the tokenizer files must sit in one folder: the loader finds them there.
        assert!(route.models.iter().all(|m| m.folder == weights.folder));
        let names: Vec<&str> = route.models.iter().map(|m| m.filename.as_str()).collect();
        assert!(names.contains(&"vocab.json") && names.contains(&"merges.txt"));
        for m in &route.models {
            assert!(m.size_bytes > 0, "{}", m.filename);
            assert_eq!(m.sha256.len(), 64, "{}", m.filename);
            assert!(
                m.sha256
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            );
            assert!(m.licence.accept_required);
            match &m.source {
                Source::HuggingFace { revision, .. } => {
                    assert_eq!(revision.as_deref().map(str::len), Some(40), "pinned");
                }
                Source::Url { .. } => panic!("expected a pinned Hugging Face file"),
            }
        }
        assert!(route.total_bytes() > 3_000_000_000);
    }
}

#[test]
fn a_route_without_files_of_its_own_has_no_weights() {
    let feature = select_subject();
    let mut route = feature.local.unwrap().native[0].clone();
    route.models.clear();
    assert!(route.weights().is_none());
    assert_eq!(route.total_bytes(), 0);
}

#[test]
fn a_runtime_with_no_code_yet_offers_no_download() {
    let feature = select_subject();
    let recipe = feature.local.as_ref().unwrap();
    let burn = recipe.native.iter().find(|r| r.provider == "burn").unwrap();
    assert!(burn.models.is_empty() && burn.weights().is_none());
    assert_eq!(burn.total_bytes(), 0);
}
