//! System detection on fake probes: what the profile says for each operating system, and that a
//! missing or garbled probe finds nothing instead of inventing a card.
use craft_ai_comfy_env::system::{
    Probe, detect_with, parse_adapter_names, parse_chipsets, parse_nvidia_gpus,
    read_drm_vendor_ids, vendor_from_name, vendor_from_pci_id,
};
use craft_ai_features::{GpuVendor, Os};

struct Fake {
    nvidia: Option<&'static str>,
    powershell: Option<&'static str>,
    profiler: Option<&'static str>,
    drm: Vec<&'static str>,
}

impl Fake {
    fn none() -> Self {
        Self {
            nvidia: None,
            powershell: None,
            profiler: None,
            drm: Vec::new(),
        }
    }
}

impl Probe for Fake {
    fn run(&self, program: &str, _args: &[&str]) -> Option<String> {
        match program {
            "nvidia-smi" => self.nvidia,
            "powershell" => self.powershell,
            "system_profiler" => self.profiler,
            _ => None,
        }
        .map(str::to_owned)
    }
    fn drm_vendor_ids(&self) -> Vec<String> {
        self.drm.iter().map(|s| (*s).to_owned()).collect()
    }
}

#[test]
fn nvidia_smi_lines_give_name_and_memory() {
    let gpus = parse_nvidia_gpus("NVIDIA GeForce RTX 3090 Ti, 24564\r\nNVIDIA T400, 2048\r\n");
    assert_eq!(gpus.len(), 2);
    assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 3090 Ti");
    assert_eq!(gpus[0].vram_mb, Some(24_564));
    assert_eq!(gpus[1].vram_mb, Some(2_048));
    assert!(gpus.iter().all(|g| g.vendor == GpuVendor::Nvidia));
}

#[test]
fn garbled_nvidia_smi_output_keeps_no_invented_memory() {
    assert!(parse_nvidia_gpus("").is_empty());
    assert!(parse_nvidia_gpus("no devices were found").is_empty());
    assert!(parse_nvidia_gpus(", 8192").is_empty());
    let unknown = parse_nvidia_gpus("NVIDIA RTX, [N/A]");
    assert_eq!(unknown[0].vram_mb, None);
    assert_eq!(parse_nvidia_gpus("NVIDIA RTX, 0")[0].vram_mb, None);
}

#[test]
fn adapter_names_become_vendors_and_software_adapters_are_dropped() {
    let gpus = parse_adapter_names(
        "AMD Radeon RX 7800 XT\r\nIntel(R) UHD Graphics 770\r\nMicrosoft Basic Render Driver\r\n\r\n",
    );
    let vendors: Vec<GpuVendor> = gpus.iter().map(|g| g.vendor).collect();
    assert_eq!(vendors, [GpuVendor::Amd, GpuVendor::Intel]);
    assert_eq!(
        vendor_from_name("NVIDIA GeForce GTX 1650"),
        GpuVendor::Nvidia
    );
    assert_eq!(vendor_from_name("Apple M2 Pro"), GpuVendor::Apple);
    assert_eq!(vendor_from_name("Something Else"), GpuVendor::Other);
}

#[test]
fn pci_ids_map_and_unknown_ids_are_ignored() {
    assert_eq!(vendor_from_pci_id("0x10de\n"), Some(GpuVendor::Nvidia));
    assert_eq!(vendor_from_pci_id("0x1002"), Some(GpuVendor::Amd));
    assert_eq!(vendor_from_pci_id("0x8086"), Some(GpuVendor::Intel));
    assert_eq!(vendor_from_pci_id("0x1234"), None);
}

#[test]
fn macos_chipset_lines_are_read() {
    let text = "Graphics/Displays:\n\n    AMD Radeon Pro 5500M:\n\n      Chipset Model: AMD Radeon Pro 5500M\n      Type: GPU\n";
    let gpus = parse_chipsets(text);
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0].vendor, GpuVendor::Amd);
}

#[test]
fn windows_merges_the_system_list_with_nvidia_smi_without_listing_the_nvidia_card_twice() {
    let probe = Fake {
        nvidia: Some("NVIDIA GeForce RTX 3090 Ti, 24564"),
        powershell: Some("NVIDIA GeForce RTX 3090 Ti\r\nIntel(R) UHD Graphics 770\r\n"),
        ..Fake::none()
    };
    let p = detect_with(Os::Windows, "x86_64", true, &probe);
    assert_eq!(p.os, Os::Windows);
    assert!(p.comfyui_found);
    let names: Vec<&str> = p.gpus.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(
        names,
        ["NVIDIA GeForce RTX 3090 Ti", "Intel(R) UHD Graphics 770"]
    );
    assert_eq!(p.gpus[0].vram_mb, Some(24_564));
}

#[test]
fn windows_without_nvidia_smi_still_finds_an_nvidia_card_by_name_with_unknown_memory() {
    let probe = Fake {
        powershell: Some("NVIDIA GeForce GTX 1650\r\n"),
        ..Fake::none()
    };
    let p = detect_with(Os::Windows, "x86_64", false, &probe);
    assert_eq!(p.gpus.len(), 1);
    assert_eq!(p.gpus[0].vendor, GpuVendor::Nvidia);
    assert_eq!(p.gpus[0].vram_mb, None);
}

#[test]
fn linux_reads_the_display_adapters_from_pci_ids() {
    let probe = Fake {
        drm: vec!["0x1002", "0x8086", "0xabcd"],
        ..Fake::none()
    };
    let p = detect_with(Os::Linux, "x86_64", false, &probe);
    let vendors: Vec<GpuVendor> = p.gpus.iter().map(|g| g.vendor).collect();
    assert_eq!(vendors, [GpuVendor::Amd, GpuVendor::Intel]);
}

#[test]
fn apple_silicon_has_an_apple_gpu_without_asking_anything() {
    let p = detect_with(Os::MacOs, "aarch64", false, &Fake::none());
    assert_eq!(p.gpus.len(), 1);
    assert_eq!(p.gpus[0].vendor, GpuVendor::Apple);
}

#[test]
fn a_computer_where_every_probe_fails_has_no_gpus_and_no_error() {
    for os in [Os::Windows, Os::Linux, Os::MacOs, Os::Other] {
        let p = detect_with(os, "x86_64", false, &Fake::none());
        assert!(p.gpus.is_empty(), "{os:?}");
    }
}

#[test]
fn the_drm_folder_reader_takes_cards_only_and_survives_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let card = |name: &str, vendor: Option<&str>| {
        let device = dir.path().join(name).join("device");
        std::fs::create_dir_all(&device).unwrap();
        if let Some(v) = vendor {
            std::fs::write(
                device.join("vendor"),
                format!(
                    "{v}
"
                ),
            )
            .unwrap();
        }
    };
    // Folder order (card0, card1) is the reverse of id order, so only a sort gives this answer.
    card("card0", Some("0x8086"));
    card("card1", Some("0x10de"));
    card("card2", None); // no vendor file: skipped
    card("card0-DP-1", Some("0x10de")); // a connector, not an adapter
    card("cardX", Some("0x1002")); // not a card number
    std::fs::write(dir.path().join("version"), b"1").unwrap();
    assert_eq!(read_drm_vendor_ids(dir.path()), ["0x10de", "0x8086"]);
    assert!(read_drm_vendor_ids(&dir.path().join("nope")).is_empty());
}
