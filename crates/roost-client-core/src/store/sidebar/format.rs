//! Sidebar text and colour derivations: the short machine label a row shows
//! and the deterministic hue a fingerprint or folder key paints its avatar in.
//! Ports `apps/web/src/lib/sidebarFormat.ts` (`shortServerLabel`; `shortCwd`
//! is the host path codec's `short_worker_path`) and `apps/web/src/lib/fpColor.ts`.
//! Folder groups, session rows and the viewer chips call these.

/// Drop a `.local` suffix and, when at least two segments remain, a leading
/// all-letters owner segment: `mike-m1-air-old` reads `m1-air-old`.
pub fn short_server_label(label: &str) -> String {
    let clean = label.strip_suffix(".local").unwrap_or(label);
    let parts: Vec<&str> = clean.split('-').collect();
    let owner_is_letters =
        !parts[0].is_empty() && parts[0].chars().all(|character| character.is_ascii_alphabetic());
    if parts.len() > 2 && owner_is_letters {
        parts[1..].join("-")
    } else {
        clean.to_owned()
    }
}

/// A fingerprint's hue and the tonal pair a viewer avatar paints with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpColor {
    /// `0..360`.
    pub hue: u32,
    /// The avatar background.
    pub bg: String,
    /// The avatar monogram.
    pub fg: String,
}

/// FNV-1a over the key's UTF-16 code units, reduced to a hue. A `fp:suffix`
/// viewer key hashes only the fingerprint, so one browser's tabs share a tone.
///
/// The multiply is done in `f64` and then wrapped to 32 bits, because that is
/// what v2's `(h * 0x01000193) >>> 0` computes: the product exceeds 2^53 and
/// JavaScript rounds it before truncating. An exact 32-bit multiply would give
/// every folder a different colour than v2 gave it.
pub fn color_for_fp(key: &str) -> FpColor {
    let fp = key.split_once(':').map_or(key, |(head, _)| head);
    let mut hash: u32 = 0x811c_9dc5;
    for unit in fp.encode_utf16() {
        hash ^= u32::from(unit);
        let product = f64::from(hash) * f64::from(0x0100_0193_u32);
        hash = wrap_to_u32(product);
    }
    let hue = hash % 360;
    FpColor {
        hue,
        bg: format!("hsl({hue} 60% 28% / 0.85)"),
        fg: format!("hsl({hue} 70% 78%)"),
    }
}

/// The avatar background a folder or session row sets as `--avatar-bg`.
pub fn avatar_background(key: &str) -> String {
    format!("hsl({} 48% 42%)", color_for_fp(key).hue)
}

/// ECMAScript `ToUint32` of a non-negative integral double.
fn wrap_to_u32(value: f64) -> u32 {
    let wrapped = value % 4_294_967_296.0;
    // `wrapped` is an integer in `0..2^32` by construction, so the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let exact = wrapped as u32;
    exact
}
