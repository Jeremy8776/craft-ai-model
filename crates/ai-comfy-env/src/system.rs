//! Looks at this computer and builds the [`SystemProfile`] that engine advice reads. Best effort:
//! a probe that fails or prints something unexpected finds nothing, and what was not found stays
//! out of the profile rather than being guessed. Slow (it starts a few programs), so call it off
//! the interface thread and keep the result.
//!
//! The probes are behind [`Probe`] so the logic runs in tests on fake output.

use std::path::Path;
use std::process::Command;

use craft_ai_features::{Gpu, GpuVendor, Os, SystemProfile};

/// What detection can ask the computer.
pub trait Probe {
    /// Runs a program and returns its output, or `None` if it is missing or failed.
    fn run(&self, program: &str, args: &[&str]) -> Option<String>;
    /// PCI vendor ids (`0x10de`) of the display adapters under `/sys/class/drm`. Linux only.
    fn drm_vendor_ids(&self) -> Vec<String>;
}

/// The real computer.
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        let mut command = Command::new(program);
        command.args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // no console window
        }
        let output = command.output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn drm_vendor_ids(&self) -> Vec<String> {
        read_drm_vendor_ids(Path::new("/sys/class/drm"))
    }
}

/// The PCI vendor id of every `cardN` under `root` (`/sys/class/drm` on Linux), sorted. Connector
/// entries such as `card0-DP-1` and anything unreadable are skipped.
pub fn read_drm_vendor_ids(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_card = name
            .strip_prefix("card")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !is_card {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path().join("device/vendor")) {
            ids.push(text.trim().to_owned());
        }
    }
    ids.sort();
    ids
}

/// The profile of this computer. `comfyui_found` comes from `ComfyInstall::discover`, which already
/// ran when the launcher scanned for it.
pub fn detect(comfyui_found: bool) -> SystemProfile {
    let os = match std::env::consts::OS {
        "windows" => Os::Windows,
        "linux" => Os::Linux,
        "macos" => Os::MacOs,
        _ => Os::Other,
    };
    detect_with(os, std::env::consts::ARCH, comfyui_found, &SystemProbe)
}

/// [`detect`] with the operating system, processor family and probes given.
pub fn detect_with(os: Os, arch: &str, comfyui_found: bool, probe: &dyn Probe) -> SystemProfile {
    let nvidia = probe
        .run(
            "nvidia-smi",
            &[
                "--query-gpu=name,memory.total",
                "--format=csv,noheader,nounits",
            ],
        )
        .map(|text| parse_nvidia_gpus(&text))
        .unwrap_or_default();
    let others = match os {
        Os::Windows => probe
            .run(
                "powershell",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "(Get-CimInstance Win32_VideoController).Name",
                ],
            )
            .map(|text| parse_adapter_names(&text))
            .unwrap_or_default(),
        Os::Linux => probe
            .drm_vendor_ids()
            .iter()
            .filter_map(|id| vendor_from_pci_id(id))
            .map(|vendor| Gpu {
                vendor,
                name: format!("{} graphics", vendor_label(vendor)),
                vram_mb: None,
            })
            .collect(),
        Os::MacOs if arch == "aarch64" => vec![Gpu {
            vendor: GpuVendor::Apple,
            name: "Apple silicon GPU".to_owned(),
            vram_mb: None,
        }],
        Os::MacOs => probe
            .run("system_profiler", &["SPDisplaysDataType"])
            .map(|text| parse_chipsets(&text))
            .unwrap_or_default(),
        Os::Other => Vec::new(),
    };
    // nvidia-smi knows the memory; the system lists would only repeat those cards without it.
    let mut gpus = nvidia.clone();
    gpus.extend(
        others
            .into_iter()
            .filter(|g| nvidia.is_empty() || g.vendor != GpuVendor::Nvidia),
    );
    SystemProfile {
        os,
        gpus,
        comfyui_found,
    }
}

/// `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits`: one `name, MiB`
/// line per card. A line without a readable memory figure keeps the card with `None`.
pub fn parse_nvidia_gpus(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (name, memory) = line.rsplit_once(',')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some(Gpu {
                vendor: GpuVendor::Nvidia,
                name: name.to_owned(),
                vram_mb: memory.trim().parse::<u32>().ok().filter(|mb| *mb > 0),
            })
        })
        .collect()
}

/// One adapter name per line (Windows `Win32_VideoController`). Software adapters are dropped.
pub fn parse_adapter_names(text: &str) -> Vec<Gpu> {
    text.lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            !lower.contains("basic render") && !lower.contains("remote display")
        })
        .map(|name| Gpu {
            vendor: vendor_from_name(name),
            name: name.to_owned(),
            vram_mb: None,
        })
        .collect()
}

/// `Chipset Model: ...` lines of macOS `system_profiler SPDisplaysDataType`.
pub fn parse_chipsets(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("Chipset Model:"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| Gpu {
            vendor: vendor_from_name(name),
            name: name.to_owned(),
            vram_mb: None,
        })
        .collect()
}

/// The maker named in an adapter's name.
pub fn vendor_from_name(name: &str) -> GpuVendor {
    let lower = name.to_ascii_lowercase();
    if lower.contains("nvidia") || lower.contains("geforce") || lower.contains("quadro") {
        GpuVendor::Nvidia
    } else if lower.contains("amd") || lower.contains("radeon") || lower.contains("ati ") {
        GpuVendor::Amd
    } else if lower.contains("intel") {
        GpuVendor::Intel
    } else if lower.contains("apple") {
        GpuVendor::Apple
    } else {
        GpuVendor::Other
    }
}

/// The maker for a PCI vendor id such as `0x10de`.
pub fn vendor_from_pci_id(id: &str) -> Option<GpuVendor> {
    match id.trim().to_ascii_lowercase().as_str() {
        "0x10de" => Some(GpuVendor::Nvidia),
        "0x1002" | "0x1022" => Some(GpuVendor::Amd),
        "0x8086" => Some(GpuVendor::Intel),
        _ => None,
    }
}

fn vendor_label(vendor: GpuVendor) -> &'static str {
    match vendor {
        GpuVendor::Nvidia => "Nvidia",
        GpuVendor::Amd => "AMD",
        GpuVendor::Intel => "Intel",
        GpuVendor::Apple => "Apple",
        GpuVendor::Other => "Other",
    }
}
