use craft_ai_comfyui::{Binding, Workflow};
use craft_ai_core::capability::CapabilityId;
use craft_ai_core::model::InputRole;
use craft_ai_core::{Payload, Request};
use serde_json::{Value, json};

fn request(capability: &str) -> Request {
    Request::new(CapabilityId::parse(capability).unwrap())
        .with_input(InputRole::Prompt, Payload::text("a red kite"))
}

fn inpaint_workflow() -> Workflow {
    Workflow::import_inpaint(
        "repair",
        json!({
            "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
            "2":{"class_type":"LoadImage","inputs":{"image":"old-source.png"}},
            "3":{"class_type":"LoadImageMask","inputs":{"image":"old-mask.png","channel":"red"}},
            "4":{"class_type":"SaveImage","inputs":{"images":["5",0]}},
            "5":{"class_type":"Inpaint","inputs":{"image":["2",0],"mask":["3",0],"prompt":["1",0]}}
        }),
        Binding::new("1", "CLIPTextEncode", "text"),
        "4",
        Binding::new("2", "LoadImage", "image"),
        Binding::new("3", "LoadImageMask", "image"),
    )
    .unwrap()
}

#[test]
fn inpaint_refuses_disconnected_mask_or_source_nodes() {
    let disconnected = json!({
        "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
        "2":{"class_type":"LoadImage","inputs":{"image":"old-source.png"}},
        "3":{"class_type":"LoadImageMask","inputs":{"image":"old-mask.png"}},
        "4":{"class_type":"SaveImage","inputs":{"images":["5",0]}},
        "5":{"class_type":"Inpaint","inputs":{"image":["2",0],"prompt":["1",0]}}
    });
    let err = Workflow::import_inpaint(
        "not-repair",
        disconnected,
        Binding::new("1", "CLIPTextEncode", "text"),
        "4",
        Binding::new("2", "LoadImage", "image"),
        Binding::new("3", "LoadImageMask", "image"),
    )
    .unwrap_err();
    assert!(err.message.contains("mask"), "{err}");
}

#[test]
fn inpaint_requires_typed_source_and_mask_before_any_upload() {
    let workflow = inpaint_workflow();
    let spec = workflow
        .info()
        .spec_for(workflow.capability())
        .unwrap()
        .clone();
    assert!(spec.requires.contains(&InputRole::Source));
    assert!(spec.requires.contains(&InputRole::Mask));
    let png = b"\x89PNG\r\n\x1a\n".to_vec();
    let no_mask = request("image.inpaint").with_input(
        InputRole::Source,
        Payload::inline("image/png", png.clone()).unwrap(),
    );
    assert!(
        workflow
            .validate_request(&no_mask)
            .unwrap_err()
            .message
            .contains("mask")
    );
    let valid = no_mask.with_input(InputRole::Mask, Payload::inline("image/png", png).unwrap());
    assert!(workflow.validate_request(&valid).is_ok());
    assert!(
        workflow.render(&valid).is_err(),
        "render cannot silently drop media inputs"
    );
    let rendered = workflow
        .render_with_uploads(&valid, "source.png", "mask.png")
        .unwrap();
    assert_eq!(rendered["1"]["inputs"]["text"], "a red kite");
    assert_eq!(rendered["2"]["inputs"]["image"], "source.png");
    assert_eq!(rendered["3"]["inputs"]["image"], "mask.png");
}

#[test]
fn imported_image_workflow_maps_prompt_without_changing_template() {
    let graph = json!({
        "1":{"class_type":"CLIPTextEncode","inputs":{"text":"original","clip":["2",0]}},
        "2":{"class_type":"CheckpointLoaderSimple","inputs":{"ckpt_name":"installed.safetensors"}},
        "3":{"class_type":"SaveImage","inputs":{"images":["4",0]}}
    });
    let original = graph.clone();
    let workflow = Workflow::import(
        "kite-image",
        "image.generate",
        graph,
        Binding::new("1", "CLIPTextEncode", "text"),
        "3",
    )
    .unwrap();
    let rendered = workflow.render(&request("image.generate")).unwrap();
    assert_eq!(rendered["1"]["inputs"]["text"], "a red kite");
    assert_eq!(rendered["1"]["inputs"]["clip"], json!(["2", 0]));
    assert_eq!(original["1"]["inputs"]["text"], "original");
}

#[test]
fn installation_rejects_an_unavailable_node_class() {
    let graph = json!({
        "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
        "2":{"class_type":"CustomSaver","inputs":{"images":["1",0]}}
    });
    let workflow = Workflow::import(
        "custom",
        "image.generate",
        graph,
        Binding::new("1", "CLIPTextEncode", "text"),
        "2",
    )
    .unwrap();
    let info = json!({"CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}}});
    assert!(workflow.validate_installation(&info).is_err());
}

#[test]
fn installation_rejects_a_missing_checkpoint_choice() {
    let graph = json!({
        "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
        "2":{"class_type":"CheckpointLoaderSimple","inputs":{"ckpt_name":"missing.safetensors"}}
    });
    let workflow = Workflow::import(
        "checkpoint",
        "image.generate",
        graph,
        Binding::new("1", "CLIPTextEncode", "text"),
        "2",
    )
    .unwrap();
    let info = json!({
        "CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}},
        "CheckpointLoaderSimple":{"input":{"required":{"ckpt_name":[["installed.safetensors"],{}]}}}
    });
    assert!(workflow.validate_installation(&info).is_err());
}

#[test]
fn video_workflow_rejects_stale_node_mapping_and_unbound_inputs() {
    let graph = json!({"7":{"class_type":"SaveVideo","inputs":{"video":["9",0]}},
        "8":{"class_type":"PromptNode","inputs":{"text":"old"}}});
    assert!(
        Workflow::import(
            "v",
            "video.generate",
            graph.clone(),
            Binding::new("8", "MissingNode", "text"),
            "7"
        )
        .is_err()
    );
    let workflow = Workflow::import(
        "v",
        "video.generate",
        graph,
        Binding::new("8", "PromptNode", "text"),
        "7",
    )
    .unwrap();
    let unsupported = request("video.generate").with_input(
        InputRole::Source,
        Payload::inline("image/png", vec![1]).unwrap(),
    );
    assert!(workflow.render(&unsupported).is_err());
    let rendered: Value = workflow.render(&request("video.generate")).unwrap();
    assert_eq!(rendered["8"]["inputs"]["text"], "a red kite");
}
