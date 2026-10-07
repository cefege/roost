//! A pairing link drawn as a QR code in the terminal, two modules per cell in
//! Unicode half blocks, so a phone camera can open it with no typing.
//!
//! Called by `quickstart::browser_pairing` beside the printed URL. Depends on
//! `qrcode` for the module matrix only; the drawing is here because the colours
//! have to be pinned rather than left to the terminal's theme.

use qrcode::types::QrError;
use qrcode::{Color, QrCode};

/// The light border every reader needs around the code, in modules. Four is the
/// width the QR specification requires; a narrower one reads on some phones and
/// not others.
const QUIET_ZONE_MODULES: usize = 4;

/// Black foreground on a bright white background, set on every line.
///
/// Pinned because a phone reads dark modules on a light field: left to the
/// terminal's own colours, a dark theme would draw the code inverted, which
/// many camera apps refuse to decode.
const DARK_ON_LIGHT: &str = "\x1b[30;107m";

/// Back to the terminal's own colours at the end of each line, so a resize or a
/// wrapped line never paints the rest of the screen white.
const RESET_COLOURS: &str = "\x1b[0m";

/// The QR for `link`, one terminal line per two module rows, quiet zone
/// included, each line ending in a newline.
pub fn render_terminal_qr(link: &str) -> Result<String, QrError> {
    let code = QrCode::new(link.as_bytes())?;
    let width = code.width();
    let colours = code.to_colors();
    let side = width + 2 * QUIET_ZONE_MODULES;
    let is_dark = |column: usize, row: usize| -> bool {
        let (Some(column), Some(row)) = (
            column.checked_sub(QUIET_ZONE_MODULES),
            row.checked_sub(QUIET_ZONE_MODULES),
        ) else {
            return false;
        };
        column < width
            && row < width
            && colours.get(row * width + column).copied() == Some(Color::Dark)
    };
    let mut drawn = String::with_capacity(side.div_ceil(2) * (side * 3 + 16));
    for top_row in (0..side).step_by(2) {
        drawn.push_str(DARK_ON_LIGHT);
        for column in 0..side {
            drawn.push(half_block(
                is_dark(column, top_row),
                is_dark(column, top_row + 1),
            ));
        }
        drawn.push_str(RESET_COLOURS);
        drawn.push('\n');
    }
    Ok(drawn)
}

/// The cell for one upper and one lower module, drawn in the dark foreground.
fn half_block(upper_dark: bool, lower_dark: bool) -> char {
    match (upper_dark, lower_dark) {
        (true, true) => '█',
        (true, false) => '▀',
        (false, true) => '▄',
        (false, false) => ' ',
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{DARK_ON_LIGHT, RESET_COLOURS, render_terminal_qr};

    /// Pixels per module when the drawing is turned back into an image.
    const SCALE: usize = 4;

    /// Turn the drawn lines back into a module grid, exactly as a camera sees
    /// the cells: each character is an upper and a lower module.
    fn modules_of(drawn: &str) -> Vec<Vec<bool>> {
        let mut rows = Vec::new();
        for line in drawn.lines() {
            let cells = line
                .strip_prefix(DARK_ON_LIGHT)
                .and_then(|rest| rest.strip_suffix(RESET_COLOURS))
                .expect("every line carries its own colours");
            let (upper, lower): (Vec<bool>, Vec<bool>) = cells
                .chars()
                .map(|cell| match cell {
                    '█' => (true, true),
                    '▀' => (true, false),
                    '▄' => (false, true),
                    ' ' => (false, false),
                    other => panic!("unexpected cell {other:?}"),
                })
                .unzip();
            rows.push(upper);
            rows.push(lower);
        }
        rows
    }

    fn decode(drawn: &str) -> String {
        let modules = modules_of(drawn);
        let height = modules.len() * SCALE;
        let width = modules[0].len() * SCALE;
        let mut image = rqrr::PreparedImage::prepare_from_bitmap(width, height, |x, y| {
            modules[y / SCALE][x / SCALE]
        });
        let grids = image.detect_grids();
        assert_eq!(grids.len(), 1, "exactly one code in the drawing");
        grids[0].decode().unwrap().1
    }

    #[test]
    fn the_drawn_code_decodes_to_the_exact_pairing_link() {
        let link = "https://roost.example.com/#pair=roost_bt_0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(decode(&render_terminal_qr(link).unwrap()), link);
        let loopback =
            "http://127.0.0.1:4113/#pair=roost_bt_ffffffffffffffffffffffffffffffffffffffffffffffff";
        assert_eq!(decode(&render_terminal_qr(loopback).unwrap()), loopback);
    }

    #[test]
    fn the_quiet_zone_is_light_on_every_side() {
        let modules = modules_of(&render_terminal_qr("https://roost.example.com/#pair=x").unwrap());
        let side = modules[0].len();
        for border in 0..4 {
            assert!(modules[border].iter().all(|dark| !dark));
            assert!(
                modules
                    .iter()
                    .all(|row| !row[border] && !row[side - 1 - border])
            );
        }
    }
}
