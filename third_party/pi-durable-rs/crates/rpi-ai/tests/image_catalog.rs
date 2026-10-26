//! Image model catalog freshness check (M4).
//!
//! The chat model catalog has a three-layer parity test suite
//! (`model_catalog.rs`); the image catalog
//! (`src/images/generated.rs`, upstream
//! `packages/ai/src/image-models.generated.ts`) had zero coverage and
//! silently fell two models behind upstream. This test parses the vendored
//! reference TS literal (`src/images/image-models.generated.ts`, rc.13:
//! checked in next to the transcription — the pinned `external/pi`
//! submodule's generated data is intentionally stale after catalog-only
//! refreshes, same rationale as `model_catalog.rs` V14-09) and compares it
//! model-by-model against `rpi_ai::images::generated::image_models()`, so
//! transcription drift fails the gate loudly instead of lagging unnoticed.

use std::path::PathBuf;

use rpi_ai::images::generated::image_models;
use rpi_ai::types::{ImagesModel, ImagesOutputModality, InputModality};

fn upstream_ts_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/images/image-models.generated.ts")
        .canonicalize()
        .expect(
            "vendored image-models.generated.ts reference not found \
             (refresh flow: regenerate upstream, vendor here + transcribe)",
        )
}

/// One model block parsed from the upstream TS literal.
#[derive(Debug, PartialEq)]
struct UpstreamImageModel {
    id: String,
    name: String,
    api: String,
    provider: String,
    base_url: String,
    input: Vec<String>,
    output: Vec<String>,
    /// (input, output, cacheRead, cacheWrite)
    cost: [f64; 4],
}

/// Extract `"value"` from a line like `name: "Qwen: Qwen Image 3",`.
fn ts_string_field(line: &str, key: &str) -> Option<String> {
    let t = line.trim();
    let rest = t.strip_prefix(key)?.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.rfind('"')?;
    Some(rest[..end].to_owned())
}

/// Extract `["a", "b"]` from a line like `input: ["text", "image"],`.
fn ts_array_field(line: &str, key: &str) -> Option<Vec<String>> {
    let t = line.trim();
    let rest = t.strip_prefix(key)?.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('[')?;
    let end = rest.rfind(']')?;
    Some(
        rest[..end]
            .split(',')
            .map(|item| item.trim().trim_matches('"').to_owned())
            .filter(|item| !item.is_empty())
            .collect(),
    )
}

/// Extract a number from a line like `cacheRead: 0,` inside a `cost` block.
/// The generator quotes the cost keys in newer formats (71dca871b+:
/// `"cacheRead": 0,`) — accept both shapes.
fn ts_number_field(line: &str, key: &str) -> Option<f64> {
    let t = line.trim();
    let unquoted = t.strip_prefix(key).and_then(|rest| rest.strip_prefix(':'));
    let quoted = t
        .strip_prefix(&format!("\"{key}\""))
        .and_then(|rest| rest.strip_prefix(':'));
    let rest = unquoted.or(quoted)?.trim_start();
    rest.trim_end_matches(',').parse().ok()
}

/// Parse every `"<id>": { ... } satisfies ImagesModel<...>,` block from the
/// upstream TS literal. The literal is generated and fully uniform (verified
/// against pi 0.82.1 @ 2efa728), so a strict line parser suffices; any
/// structural change upstream fails the count/assertions below loudly.
fn parse_upstream_models(ts: &str) -> Vec<UpstreamImageModel> {
    let mut models = Vec::new();
    let mut lines = ts.lines().peekable();

    while let Some(line) = lines.next() {
        let t = line.trim();
        // Model entry header: a quoted key followed by `: {`. The provider
        // group key is skipped whether unquoted (`openrouter: {`, generator
        // @ 9841914) or quoted (`"openrouter": {`, @ 71dca871b+).
        let is_provider_group = t == "openrouter: {" || t == "\"openrouter\": {";
        if is_provider_group || !(t.starts_with('"') && t.ends_with(": {")) {
            continue;
        }
        let key = t[1..t.len() - 4].to_owned();

        let mut model = UpstreamImageModel {
            id: key.clone(),
            name: String::new(),
            api: String::new(),
            provider: String::new(),
            base_url: String::new(),
            input: Vec::new(),
            output: Vec::new(),
            cost: [0.0; 4],
        };

        while let Some(line) = lines.next() {
            let t = line.trim();
            if t.starts_with("} satisfies") {
                break;
            }
            if t.starts_with("cost:") && t.trim_end().ends_with('{') {
                for cost_line in lines.by_ref() {
                    let c = cost_line.trim();
                    if c.starts_with('}') {
                        break;
                    }
                    if let Some(v) = ts_number_field(c, "input") {
                        model.cost[0] = v;
                    } else if let Some(v) = ts_number_field(c, "output") {
                        model.cost[1] = v;
                    } else if let Some(v) = ts_number_field(c, "cacheRead") {
                        model.cost[2] = v;
                    } else if let Some(v) = ts_number_field(c, "cacheWrite") {
                        model.cost[3] = v;
                    } else {
                        panic!("unrecognized cost line for {key}: {c}");
                    }
                }
            } else if let Some(v) = ts_string_field(t, "id") {
                assert_eq!(v, key, "id field diverges from block key for {key}");
            } else if let Some(v) = ts_string_field(t, "name") {
                model.name = v;
            } else if let Some(v) = ts_string_field(t, "api") {
                model.api = v;
            } else if let Some(v) = ts_string_field(t, "provider") {
                model.provider = v;
            } else if let Some(v) = ts_string_field(t, "baseUrl") {
                model.base_url = v;
            } else if let Some(v) = ts_array_field(t, "input") {
                model.input = v;
            } else if let Some(v) = ts_array_field(t, "output") {
                model.output = v;
            } else {
                panic!("unrecognized line in block {key}: {t}");
            }
        }
        models.push(model);
    }
    models
}

fn input_modality_str(m: &InputModality) -> &'static str {
    match m {
        InputModality::Text => "text",
        InputModality::Image => "image",
    }
}

fn output_modality_str(m: &ImagesOutputModality) -> &'static str {
    match m {
        ImagesOutputModality::Text => "text",
        ImagesOutputModality::Image => "image",
    }
}

#[test]
fn test_image_catalog_matches_upstream_model_for_model() {
    let ts_path = upstream_ts_path();
    let ts = std::fs::read_to_string(&ts_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", ts_path.display()));
    let upstream = parse_upstream_models(&ts);
    // Pin the vendored-reference total (rc.13 catalog-only refresh @
    // 71dca871b, 54 models): guards against the parser silently dropping
    // blocks after an upstream format change.
    assert_eq!(
        upstream.len(),
        54,
        "upstream parser found {} model blocks, expected 54 \
         (upstream format may have changed)",
        upstream.len()
    );

    let catalog = image_models();
    let rust_models: Vec<&ImagesModel> = catalog
        .iter()
        .flat_map(|(provider, models)| {
            models.iter().inspect(move |m| {
                assert_eq!(
                    m.provider, *provider,
                    "model {} provider diverges from its catalog group",
                    m.id
                );
            })
        })
        .collect();

    assert_eq!(
        rust_models.len(),
        upstream.len(),
        "image model count diverges: rust {} vs upstream {} (ids: rust {:?} / upstream {:?})",
        rust_models.len(),
        upstream.len(),
        rust_models.iter().map(|m| &m.id).collect::<Vec<_>>(),
        upstream.iter().map(|m| &m.id).collect::<Vec<_>>(),
    );

    for (rust, expected) in rust_models.iter().zip(&upstream) {
        let actual = UpstreamImageModel {
            id: rust.id.clone(),
            name: rust.name.clone(),
            api: rust.api.as_str().to_owned(),
            provider: rust.provider.clone(),
            base_url: rust.base_url.clone(),
            input: rust
                .input
                .iter()
                .map(input_modality_str)
                .map(str::to_owned)
                .collect(),
            output: rust
                .output
                .iter()
                .map(output_modality_str)
                .map(str::to_owned)
                .collect(),
            cost: [
                rust.cost.rates.input,
                rust.cost.rates.output,
                rust.cost.rates.cache_read,
                rust.cost.rates.cache_write,
            ],
        };
        assert_eq!(
            &actual, expected,
            "field-level divergence for image model {}",
            expected.id
        );
        assert_eq!(
            rust.headers, None,
            "unexpected headers on image model {}",
            expected.id
        );
    }
}
