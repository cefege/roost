//! Canonical terminal overlay dimensions and margins carried by TSP.
//! Overlay shape parsing owns these values; measured browser placement resolves
//! cells/rows using the surface's font metrics and percentages against bounds.

use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(in super::super) enum Dimension {
    Cells(f64),
    Percent(f64),
}

impl Dimension {
    pub fn parse(value: &Value) -> Option<Self> {
        if let Some(cells) = value.as_f64() {
            return Some(Self::Cells(cells));
        }
        let percent = value.as_str()?.strip_suffix('%')?;
        if percent.is_empty()
            || percent.starts_with('.')
            || percent.ends_with('.')
            || !percent
                .chars()
                .all(|character| character.is_ascii_digit() || character == '.')
        {
            return None;
        }
        let percent = percent.parse::<f64>().ok()?;
        percent
            .is_finite()
            .then_some(Self::Percent(percent / 100.0))
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub fn resolve(self, extent: f64, cell: f64) -> f64 {
        match self {
            Self::Cells(value) => value * cell,
            Self::Percent(fraction) => fraction * extent,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(in super::super) struct OverlayOptions {
    pub width: Option<Dimension>,
    pub min_width: Option<f64>,
    pub max_height: Option<Dimension>,
    pub row: Option<Dimension>,
    pub col: Option<Dimension>,
    pub offset_x: f64,
    pub offset_y: f64,
    pub margin: Option<[f64; 4]>,
}

impl OverlayOptions {
    pub fn parse(props: &Map<String, Value>) -> Self {
        let dimension = |name: &str| props.get(name).and_then(Dimension::parse);
        let number = |name: &str| props.get(name).and_then(Value::as_f64);
        let margin = props.get("margin").map(|margin| {
            if let Some(value) = margin.as_f64() {
                [value.max(0.0); 4]
            } else {
                ["top", "right", "bottom", "left"].map(|side| {
                    margin
                        .get(side)
                        .and_then(Value::as_f64)
                        .unwrap_or_default()
                        .max(0.0)
                })
            }
        });
        Self {
            width: dimension("width"),
            min_width: number("minWidth"),
            max_height: dimension("maxHeight"),
            row: dimension("row"),
            col: dimension("col"),
            offset_x: number("offsetX").unwrap_or_default(),
            offset_y: number("offsetY").unwrap_or_default(),
            margin,
        }
    }
}
