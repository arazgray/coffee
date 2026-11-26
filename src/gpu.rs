use crate::engine::Engine;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DecodeChoice {
    #[default]
    Auto,
    Gpu(usize),
    Software,
}

enum Backend {
    Vaapi { device: PathBuf, direct: bool },
    Nvdec,
}

pub struct DecoderGpu {
    key: String,
    pub name: String,
    backend: Backend,
}

#[derive(Default)]
pub struct Devices {
    pub renderer: Option<String>,
    pub decoders: Vec<DecoderGpu>,
}

struct VulkanGpu<'a> {
    name: &'a str,
    vendor: &'a str,
    id: &'a str,
    kind: &'a str,
}

fn vulkan_gpus(summary: &str) -> impl Iterator<Item = VulkanGpu<'_>> {
    summary.split("\nGPU").skip(1).filter_map(|section| {
        let field = |key: &str| {
            section
                .lines()
                .filter_map(|line| line.trim().split_once('='))
                .find_map(|(name, value)| (name.trim() == key).then_some(value.trim()))
        };
        Some(VulkanGpu {
            name: field("deviceName")?,
            vendor: field("vendorID")?,
            id: field("deviceID")?,
            kind: field("deviceType")?.trim_start_matches("PHYSICAL_DEVICE_TYPE_"),
        })
    })
}

fn pci_id(device: &Path) -> Option<(String, String)> {
    let read = |name| {
        fs::read_to_string(device.join(name))
            .ok()
            .map(|s| s.trim().to_owned())
    };
    Some((read("vendor")?, read("device")?))
}

fn display_gpu(drm: &Path, gpus: &[VulkanGpu<'_>]) -> Option<String> {
    let mut outputs: Vec<_> = fs::read_dir(drm)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let label = entry.file_name().into_string().ok()?;
            let (card, connector) = label.split_once('-')?;
            let path = entry.path();
            if fs::read_to_string(path.join("status")).ok()?.trim() != "connected"
                || fs::read_to_string(path.join("enabled")).ok()?.trim() != "enabled"
            {
                return None;
            }
            let (vendor, id) = pci_id(&drm.join(card).join("device"))?;
            let external = !connector.starts_with("eDP") && !connector.starts_with("LVDS");
            Some((external, vendor, id))
        })
        .collect();
    outputs.sort();
    outputs
        .iter()
        .find_map(|(_, vendor, id)| gpus.iter().find(|g| g.vendor == vendor && g.id == id))
        .map(|gpu| gpu.name.to_owned())
}

impl Devices {
    pub fn discover() -> Self {
        let listing = Command::new("vulkaninfo")
            .arg("--summary")
            .output()
            .ok()
            .filter(|r| r.status.success())
            .map(|r| String::from_utf8_lossy(&r.stdout).into_owned())
            .unwrap_or_default();
        let gpus: Vec<_> = vulkan_gpus(&listing).collect();
        let drm = Path::new("/sys/class/drm");
        let wayland = env::var_os("WAYLAND_DISPLAY").is_some_and(|name| !name.is_empty());
        let renderer = if !wayland {
            None
        } else {
            match env::var("KAHVE_VULKAN_DEVICE") {
                Ok(device) => (!device.is_empty()).then_some(device),
                Err(_) => display_gpu(drm, &gpus),
            }
        };
        let mut render_nodes = fs::read_dir(drm)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        render_nodes.sort();
        let mut decoders = Vec::new();
        let mut nvidia = Vec::new();
        for path in render_nodes {
            let device = path.join("device");
            let Some((vendor, id)) = pci_id(&device) else {
                continue;
            };
            let pci = fs::canonicalize(&device)
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
            let gpu = gpus.iter().find(|g| g.vendor == vendor && g.id == id);
            let node = path.file_name().unwrap().to_string_lossy();
            let key = pci.as_deref().unwrap_or(&node);
            if vendor == "0x10de" {
                nvidia.push(gpu.map_or("NVIDIA dedicated GPU", |g| g.name).to_owned());
                continue;
            }
            let brand = match vendor.as_str() {
                "0x8086" => "Intel GPU",
                "0x1002" => "AMD GPU",
                _ => continue,
            };
            let name = gpu.map(|g| g.name).unwrap_or(brand);
            let role = match gpu.map(|g| g.kind) {
                Some("INTEGRATED_GPU") => "Integrated · ",
                Some("DISCRETE_GPU") => "Dedicated · ",
                _ => "",
            };
            let direct = wayland
                && gpu.is_some_and(|g| {
                    renderer.as_deref() == Some(g.name)
                        && gpus.iter().filter(|other| other.name == g.name).count() == 1
                });
            decoders.push(DecoderGpu {
                key: format!("vaapi:{key}"),
                name: if gpu.is_some() {
                    format!("{role}{name}")
                } else {
                    format!("{name} · {node}")
                },
                backend: Backend::Vaapi {
                    device: Path::new("/dev/dri").join(node.as_ref()),
                    direct,
                },
            });
        }
        if !nvidia.is_empty() || gpus.iter().any(|g| g.vendor == "0x10de") {
            let name = if nvidia.len() == 1 {
                format!("Dedicated · {}", nvidia[0])
            } else {
                "Dedicated · NVIDIA (driver default)".into()
            };
            decoders.push(DecoderGpu {
                key: "nvdec".into(),
                name,
                backend: Backend::Nvdec,
            });
        }
        Self { renderer, decoders }
    }

    pub fn apply(&self, mpv: &Engine, choice: DecodeChoice) -> Result<(), String> {
        let mode = match choice {
            DecodeChoice::Auto => "auto-safe",
            DecodeChoice::Software => "no",
            DecodeChoice::Gpu(i) => match &self
                .decoders
                .get(i)
                .ok_or("Decoder GPU is unavailable")?
                .backend
            {
                Backend::Vaapi { device, direct } => {
                    mpv.set("vaapi-device", &device.to_string_lossy())?;
                    if *direct {
                        "vaapi,vaapi-copy"
                    } else {
                        "vaapi-copy"
                    }
                }
                // Decode with NVIDIA while leaving the Wayland renderer on
                // the display GPU; nvdec-copy avoids cross-GPU interop.
                Backend::Nvdec => "nvdec-copy",
            },
        };
        mpv.set("hwdec", mode)
    }

    pub fn saved(&self) -> DecodeChoice {
        let Some(setting) = config_file().and_then(|p| fs::read_to_string(p).ok()) else {
            return DecodeChoice::Auto;
        };
        match setting.trim() {
            "software" => DecodeChoice::Software,
            key => self
                .decoders
                .iter()
                .position(|d| d.key == key)
                .map_or(DecodeChoice::Auto, DecodeChoice::Gpu),
        }
    }

    pub fn save(&self, choice: DecodeChoice) -> std::io::Result<()> {
        let Some(path) = config_file() else {
            return Ok(());
        };
        let key = match choice {
            DecodeChoice::Auto => "auto",
            DecodeChoice::Software => "software",
            DecodeChoice::Gpu(i) => &self.decoders[i].key,
        };
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, format!("{key}\n"))
    }
}

fn config_file() -> Option<PathBuf> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("kahve-native").join("decoder"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vulkan_gpu_identity_and_type() {
        let sample = "Devices:\nGPU0:\n vendorID = 0x10de\n deviceID = 0x1d10\n deviceType = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU\n deviceName = Discrete\nGPU1:\n vendorID = 0x8086\n deviceID = 0x5917\n deviceType = PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU\n deviceName = Integrated";
        let gpus = vulkan_gpus(sample).collect::<Vec<_>>();
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].kind, "DISCRETE_GPU");
        assert_eq!(gpus[1].kind, "INTEGRATED_GPU");
        assert_eq!(gpus[1].vendor, "0x8086");
        assert_eq!(gpus[1].id, "0x5917");
        assert_eq!(gpus[1].name, "Integrated");
    }
}
