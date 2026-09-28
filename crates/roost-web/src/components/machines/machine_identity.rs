//! The safe presentation of a machine's static identity: icon, label, title,
//! Apple chip badge and Linux distribution brand, from the worker's advertised
//! platform and host identity only — never from its mutable label. Ports
//! `apps/web/src/lib/machineIdentity.ts`; `MachineIdentityMark` renders it.

use roost_protocol::wire::{HostIdentity, Worker, WorkerOs};

/// A distribution with a local mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxDistributionBrand {
    /// Alpine Linux.
    Alpine,
    /// Arch Linux.
    Arch,
    /// Debian.
    Debian,
    /// Fedora.
    Fedora,
    /// Ubuntu.
    Ubuntu,
}

impl LinuxDistributionBrand {
    /// The `data-linux-brand` spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alpine => "alpine",
            Self::Arch => "arch",
            Self::Debian => "debian",
            Self::Fedora => "fedora",
            Self::Ubuntu => "ubuntu",
        }
    }
}

/// What a machine mark shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineIdentityPresentation {
    /// The Material Symbols ligature for a generic mark.
    pub icon: &'static str,
    /// The machine kind, e.g. `MacBook` or `Ubuntu Linux`.
    pub label: String,
    /// The accessible name and hover text.
    pub title: String,
    /// `M3 Pro` and the like, for an Apple silicon Mac.
    pub apple_chip_badge: Option<String>,
    /// The distribution mark, for a recognized Linux.
    pub linux_brand: Option<LinuxDistributionBrand>,
}

const MACBOOK_HARDWARE_MODEL_IDS: [&str; 16] = [
    "Mac14,2", "Mac14,5", "Mac14,6", "Mac14,7", "Mac14,9", "Mac14,10", "Mac14,15", "Mac15,3",
    "Mac15,6", "Mac15,7", "Mac15,8", "Mac15,9", "Mac15,10", "Mac15,11", "Mac15,12", "Mac15,13",
];

/// `(prefix, brand, label)`: the lower-cased distribution name starts with
/// `prefix`, then whitespace or the end. v2's optional ` GNU/Linux` and
/// ` Linux` suffixes begin with whitespace, so that alternative already
/// accepts every name they would.
const LINUX_DISTRIBUTIONS: [(&str, LinuxDistributionBrand, &str); 5] = [
    (
        "alpine linux",
        LinuxDistributionBrand::Alpine,
        "Alpine Linux",
    ),
    ("arch linux", LinuxDistributionBrand::Arch, "Arch Linux"),
    ("debian", LinuxDistributionBrand::Debian, "Debian Linux"),
    ("fedora", LinuxDistributionBrand::Fedora, "Fedora Linux"),
    ("ubuntu", LinuxDistributionBrand::Ubuntu, "Ubuntu Linux"),
];

/// The presentation for a worker record; an unknown worker is a generic computer.
pub fn machine_identity_presentation(worker: Option<&Worker>) -> MachineIdentityPresentation {
    let Some(worker) = worker else {
        return generic("computer", "Computer");
    };
    let identity = worker.host_identity.as_ref();
    let field =
        |read: fn(&HostIdentity) -> Option<&String>| identity.and_then(read).map(String::as_str);
    match worker.os {
        WorkerOs::Darwin => {
            let is_macbook =
                field(|id| id.hardware_model.as_ref()).is_some_and(is_macbook_hardware);
            let apple_chip_badge = field(|id| id.chip.as_ref()).and_then(apple_chip_badge_for);
            let label = if is_macbook { "MacBook" } else { "Mac" };
            MachineIdentityPresentation {
                icon: if is_macbook {
                    "laptop_mac"
                } else {
                    "desktop_mac"
                },
                label: label.to_owned(),
                title: match &apple_chip_badge {
                    Some(badge) => format!("{label} · Apple {badge}"),
                    None => label.to_owned(),
                },
                apple_chip_badge,
                linux_brand: None,
            }
        }
        WorkerOs::Linux => {
            let distribution =
                field(|id| id.linux_distribution.as_ref()).and_then(linux_distribution_for);
            let label = distribution.map_or("Linux", |(_, label)| label);
            MachineIdentityPresentation {
                icon: "dns",
                label: label.to_owned(),
                title: format!("{label} machine"),
                apple_chip_badge: None,
                linux_brand: distribution.map(|(brand, _)| brand),
            }
        }
        WorkerOs::Win32 => {
            if field(|id| id.hardware_model.as_ref()).is_some_and(is_windows_laptop_model) {
                generic("laptop_windows", "Windows laptop")
            } else {
                generic("desktop_windows", "Windows PC")
            }
        }
    }
}

fn generic(icon: &'static str, label: &str) -> MachineIdentityPresentation {
    MachineIdentityPresentation {
        icon,
        label: label.to_owned(),
        title: label.to_owned(),
        apple_chip_badge: None,
        linux_brand: None,
    }
}

/// `^Apple\s+(M[1-9]\d?(?:\s+(?:Pro|Max|Ultra))?)$`, case-insensitive, with the
/// badge's leading `m` upper-cased.
fn apple_chip_badge_for(chip: &str) -> Option<String> {
    let trimmed = chip.trim();
    let (vendor, rest) = trimmed.split_at_checked(5)?;
    if !vendor.eq_ignore_ascii_case("apple") || !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let badge = rest.trim_start();
    let mut words = badge.split_whitespace();
    let generation = words.next()?;
    let tier = words.next();
    if words.next().is_some() || !is_chip_generation(generation) {
        return None;
    }
    if let Some(tier) = tier
        && !["pro", "max", "ultra"].contains(&tier.to_ascii_lowercase().as_str())
    {
        return None;
    }
    let mut out = String::from("M");
    out.push_str(&badge[1..]);
    Some(out)
}

/// `M[1-9]\d?`, case-insensitive.
fn is_chip_generation(word: &str) -> bool {
    let bytes = word.as_bytes();
    matches!(bytes.first(), Some(b'M' | b'm'))
        && matches!(bytes.get(1), Some(b'1'..=b'9'))
        && (bytes.len() == 2 || (bytes.len() == 3 && bytes[2].is_ascii_digit()))
}

fn linux_distribution_for(distribution: &str) -> Option<(LinuxDistributionBrand, &'static str)> {
    let name = distribution.trim().to_lowercase();
    LINUX_DISTRIBUTIONS
        .iter()
        .find(|(prefix, _, _)| {
            name.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        })
        .map(|(_, brand, label)| (*brand, *label))
}

fn is_macbook_hardware(model: &str) -> bool {
    model.starts_with("MacBook") || MACBOOK_HARDWARE_MODEL_IDS.contains(&model)
}

/// `\b(?:laptop|notebook|book)\b`, case-insensitive.
fn is_windows_laptop_model(model: &str) -> bool {
    model
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| ["laptop", "notebook", "book"].contains(&word.to_ascii_lowercase().as_str()))
}
