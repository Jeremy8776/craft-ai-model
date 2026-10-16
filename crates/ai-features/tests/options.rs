use craft_ai_core::PRESETS;
use craft_ai_features::{Route, builtin, setup_options};

#[test]
fn options_are_derived_from_the_features_not_written_twice() {
    let features = builtin();
    let options = setup_options(&features);
    // One local model (SAM 3), then hosted services.
    assert!(matches!(&options[0].route, Route::Local { feature } if feature == "select-subject"));
    assert_eq!(options[0].title, "SAM 3");
    assert_eq!(options[0].unlocks, ["select-subject"]);
    assert!(options[0].usable, "PhotoCraft runs SAM 3 today");
    assert!(options.iter().skip(1).all(|o| o.usable));
    // Ids are unique.
    let mut ids: Vec<_> = options.iter().map(|o| o.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), options.len());
}

#[test]
fn a_service_unlocks_every_tool_that_names_it_once() {
    let features = builtin();
    let options = setup_options(&features);
    let openai = options.iter().find(|o| o.id == "service:openai").unwrap();
    assert_eq!(openai.unlocks, ["generative-fill", "generate-image"]);
    assert_eq!(openai.title, "OpenAI");
    assert!(!openai.note.is_empty());
    let bfl = options.iter().find(|o| o.id == "service:bfl").unwrap();
    assert_eq!(
        bfl.unlocks,
        ["generate-image", "generate-video"],
        "named by two tools, listed once each"
    );
}

#[test]
fn every_tool_a_service_unlocks_really_lists_that_service() {
    let features = builtin();
    for option in setup_options(&features) {
        let Route::Service { preset } = &option.route else {
            continue;
        };
        assert!(PRESETS.iter().any(|p| p.key == preset), "{preset}");
        for id in &option.unlocks {
            let f = features.iter().find(|f| &f.id == id).unwrap();
            assert!(
                f.cloud.iter().any(|c| &c.preset == preset),
                "{id} -> {preset}"
            );
        }
    }
}

#[test]
fn every_hosted_route_in_the_catalogue_shows_up_as_an_option() {
    let features = builtin();
    let options = setup_options(&features);
    for f in &features {
        for c in &f.cloud {
            let found = options
                .iter()
                .find(|o| o.id == format!("service:{}", c.preset))
                .unwrap_or_else(|| panic!("{} -> {} has no option", f.id, c.preset));
            assert!(found.unlocks.contains(&f.id));
        }
    }
}

#[test]
fn a_feature_with_nothing_to_set_up_unlocks_from_nothing() {
    let features = builtin();
    let options = setup_options(&features);
    for id in ["generative-expand", "extend-sequence", "remove-object"] {
        assert!(
            options.iter().all(|o| !o.unlocks.iter().any(|u| u == id)),
            "{id} has no route yet, so nothing can unlock it"
        );
    }
}

#[test]
fn an_empty_catalogue_has_no_options() {
    assert!(setup_options(&[]).is_empty());
}

#[test]
fn a_local_model_says_how_much_it_downloads_and_a_hosted_service_downloads_nothing() {
    let options = setup_options(&builtin());
    let sam3 = options
        .iter()
        .find(|o| o.id == "local:select-subject")
        .unwrap();
    assert_eq!(sam3.size_bytes, Some(3_449_860_632));
    for service in options
        .iter()
        .filter(|o| matches!(o.route, Route::Service { .. }))
    {
        assert_eq!(service.size_bytes, None, "{}", service.id);
    }
}
