//! The pairing link as an SVG QR code a phone camera opens with no typing.
//!
//! Called by `pairing::phone_pairing`. Depends on `qrcode`'s SVG renderer. Pure
//! and native, so the decode round trip is proved without a browser.
//!
//! THE MARKUP CARRIES NO COLOUR VALUE. The modules are `currentColor` and the
//! field is unpainted, so the element that hosts the markup decides both
//! through the `--qr-module`/`--qr-field` tokens and no hex reaches the DOM.

use qrcode::QrCode;
use qrcode::render::svg;
use qrcode::types::QrError;

/// The smallest side the code is drawn at, in CSS pixels. Each module is a
/// whole number of pixels at or above it, so `crispEdges` never blurs one.
const MIN_SIDE_PX: u32 = 232;

/// The `<svg>` element for `link`, quiet zone included, with no XML prolog: it
/// is inserted into an HTML document, where a prolog is not markup at all.
pub fn pairing_qr_svg(link: &str) -> Result<String, QrError> {
    let code = QrCode::new(link.as_bytes())?;
    let document = code
        .render::<svg::Color<'_>>()
        .min_dimensions(MIN_SIDE_PX, MIN_SIDE_PX)
        .dark_color(svg::Color("currentColor"))
        .light_color(svg::Color("none"))
        .build();
    Ok(match document.find("<svg") {
        Some(start) => document[start..].to_owned(),
        None => document,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::pairing_qr_svg;

    /// The value of `name="…"` on the first element that carries it.
    fn attribute<'a>(markup: &'a str, name: &str) -> &'a str {
        let marker = format!(" {name}=\"");
        let start = markup.find(&marker).expect("attribute present") + marker.len();
        let length = markup[start..].find('"').expect("attribute closed");
        &markup[start..start + length]
    }

    /// Paint the dark path back onto a bitmap, exactly as a browser would fill
    /// it: every `M{left} {top}h{w}v{h}H{left}V{top}` is one dark rectangle.
    fn rasterize(markup: &str) -> (usize, Vec<bool>) {
        let side: usize = attribute(markup, "width").parse().unwrap();
        assert_eq!(attribute(markup, "height"), side.to_string());
        let path_start = markup.find("fill=\"currentColor\" d=\"").unwrap();
        let path = attribute(&markup[path_start - 1..], "d");
        let mut pixels = vec![false; side * side];
        for rectangle in path.split('M').filter(|segment| !segment.is_empty()) {
            let numbers: Vec<usize> = rectangle
                .split(|byte: char| !byte.is_ascii_digit())
                .filter(|digits| !digits.is_empty())
                .map(|digits| digits.parse().unwrap())
                .collect();
            let [left, top, width, height, ..] = numbers[..] else {
                panic!("not a rectangle: {rectangle}");
            };
            for row in top..top + height {
                for column in left..left + width {
                    pixels[row * side + column] = true;
                }
            }
        }
        (side, pixels)
    }

    fn decode(markup: &str) -> String {
        let (side, pixels) = rasterize(markup);
        let mut image =
            rqrr::PreparedImage::prepare_from_bitmap(side, side, |x, y| pixels[y * side + x]);
        let grids = image.detect_grids();
        assert_eq!(grids.len(), 1, "exactly one code in the drawing");
        grids[0].decode().unwrap().1
    }

    #[test]
    fn the_svg_decodes_to_the_exact_pairing_link() {
        let link = "https://roost.example.com/#pair=roost_bt_0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(decode(&pairing_qr_svg(link).unwrap()), link);
    }

    #[test]
    fn the_markup_is_one_svg_element_with_no_colour_value_of_its_own() {
        let markup = pairing_qr_svg("https://roost.example.com/#pair=roost_bt_ab").unwrap();
        assert!(markup.starts_with("<svg"), "{markup}");
        assert!(markup.ends_with("</svg>"), "{markup}");
        assert!(
            !markup.contains('#'),
            "no hex colour and no fragment text: {markup}"
        );
        assert!(markup.contains("fill=\"currentColor\""));
        assert!(markup.contains("fill=\"none\""));
        assert!(attribute(&markup, "width").parse::<u32>().unwrap() >= 232);
    }
}
