//! Prints what detection finds on this computer and the engine advice for Select subject, as if an
//! app had every adapter. Read-only: it starts `nvidia-smi` and, on Windows, PowerShell.
use craft_ai_comfy_env::{ComfyInstall, system};
use craft_ai_features::{EngineOffer, advise};

fn main() {
    let comfy = ComfyInstall::discover().is_some();
    let profile = system::detect(comfy);
    println!("{profile:#?}");
    let offers: Vec<EngineOffer> = ["candle", "burn", "comfyui"]
        .iter()
        .map(|p| EngineOffer {
            provider: (*p).to_owned(),
            adapter_present: true,
            accelerators: None,
        })
        .collect();
    for a in advise(&profile, &offers, None) {
        println!(
            "{:<8} usable={} pace={:?} recommended={}  {}",
            a.provider, a.usable, a.pace, a.recommended, a.reason
        );
    }
}
