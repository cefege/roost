//! Human-readable sizes, transfer speeds and times remaining, plus the one
//! allowlist of controls that may take keyboard focus from a terminal. Ports
//! `apps/web/src/lib/format.ts` and `apps/web/src/lib/focusOwners.ts`; read by
//! the transfer cards, the attachments pane, the machine cards (format) and the
//! terminal pane's document focus guards (`FOCUS_OWNERS`).

/// Selectors of the controls allowed to keep focus away from a terminal: native
/// and ARIA text, dialog and popup owners. Generic buttons are excluded so a
/// terminal's focus stays stable.
pub const FOCUS_OWNERS: &str = "input, textarea, select, [contenteditable=\"\"], [contenteditable=\"true\"], [role=\"textbox\"], [role=\"searchbox\"], [role=\"dialog\"], [role=\"menu\"], [role=\"listbox\"], [role=\"combobox\"], dialog, .wterm";

const KIB: f64 = 1024.0;
const MIB: f64 = 1_048_576.0;
const GIB: f64 = 1_073_741_824.0;

/// `512 B`, `4.0 KB`, `1.5 MB`, `2.0 GB`.
pub fn format_bytes(bytes: f64) -> String {
    if bytes >= GIB {
        format!("{:.1} GB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KB", bytes / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// `4.0 MB/s`, or `—` when unknown (zero, negative or not finite).
pub fn format_speed(bytes_per_sec: f64) -> String {
    if !bytes_per_sec.is_finite() || bytes_per_sec <= 0.0 {
        return "—".to_owned();
    }
    format!("{}/s", format_bytes(bytes_per_sec))
}

/// `45s`, `2m 10s`, `1h 5m`; `<1s` under a second; empty when unknown so a
/// caller can drop its "left" suffix.
pub fn format_eta(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return String::new();
    }
    if seconds < 1.0 {
        return "<1s".to_owned();
    }
    let total = seconds.round() as u64;
    let minutes = total / 60;
    let hours = minutes / 60;
    if total < 60 {
        format!("{total}s")
    } else if minutes < 60 {
        match total % 60 {
            0 => format!("{minutes}m"),
            rest => format!("{minutes}m {rest}s"),
        }
    } else {
        match minutes % 60 {
            0 => format!("{hours}h"),
            rest => format!("{hours}h {rest}m"),
        }
    }
}
