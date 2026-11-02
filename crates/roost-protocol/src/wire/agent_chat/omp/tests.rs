//! Scope precedence and model identifier boundaries for OMP configuration.
//! These tests exercise the shared resolver used by coordinator and browser.
//! Empty values inherit independently for each field.

use super::*;

fn values(value: &str) -> OmpSettingValues {
    OmpSettingValues {
        model: Some(value.into()),
        thinking_level: Some(value.into()),
    }
}

#[test]
fn omp_settings_resolve_every_source_per_field() {
    let mut scopes = vec![
        OmpScopedSettings {
            scope: OmpSettingsScope::Defaults,
            values: values("defaults"),
        },
        OmpScopedSettings {
            scope: OmpSettingsScope::Machine {
                worker_fp: "fp".into(),
            },
            values: values("machine"),
        },
        OmpScopedSettings {
            scope: OmpSettingsScope::Folder {
                worker_fp: "fp".into(),
                cwd: "/work".into(),
            },
            values: values("folder"),
        },
    ];
    let effective = resolve_omp_settings(&scopes, "fp", "/work", &values("chat"));
    assert_eq!(
        effective.model,
        OmpEffectiveValue {
            value: Some("chat".into()),
            source: OmpSettingSource::Conversation
        }
    );
    assert_eq!(effective.thinking_level, effective.model);
    for (expected, source) in [
        ("folder", OmpSettingSource::Folder),
        ("machine", OmpSettingSource::Machine),
        ("defaults", OmpSettingSource::Defaults),
    ] {
        let effective = resolve_omp_settings(&scopes, "fp", "/work", &values(""));
        assert_eq!(
            effective.model,
            OmpEffectiveValue {
                value: Some(expected.into()),
                source
            }
        );
        assert_eq!(effective.thinking_level, effective.model);
        scopes.pop();
    }
    let effective = resolve_omp_settings(&scopes, "fp", "/work", &OmpSettingValues::default());
    assert_eq!(
        effective.model,
        OmpEffectiveValue {
            value: None,
            source: OmpSettingSource::Omp
        }
    );
    assert_eq!(effective.thinking_level, effective.model);
}

#[test]
fn omp_empty_fields_inherit_independently_and_folder_match_is_exact() {
    let scopes = vec![
        OmpScopedSettings {
            scope: OmpSettingsScope::Defaults,
            values: values("default"),
        },
        OmpScopedSettings {
            scope: OmpSettingsScope::Machine {
                worker_fp: "fp".into(),
            },
            values: OmpSettingValues {
                model: Some("machine".into()),
                thinking_level: Some("".into()),
            },
        },
        OmpScopedSettings {
            scope: OmpSettingsScope::Folder {
                worker_fp: "fp".into(),
                cwd: "/work".into(),
            },
            values: values("folder"),
        },
    ];
    for (worker, folder, expected) in [
        ("other", "/work", "default"),
        ("fp", "/work/", "machine"),
        ("fp", "/work", "folder"),
    ] {
        let effective = resolve_omp_settings(
            &scopes,
            worker,
            folder,
            &OmpSettingValues {
                model: None,
                thinking_level: Some("".into()),
            },
        );
        assert_eq!(effective.model.value.as_deref(), Some(expected));
    }
    let effective = resolve_omp_settings(
        &scopes,
        "fp",
        "/else",
        &OmpSettingValues {
            model: Some("".into()),
            thinking_level: Some("high".into()),
        },
    );
    assert_eq!(effective.model.source, OmpSettingSource::Machine);
    assert_eq!(
        effective.thinking_level.source,
        OmpSettingSource::Conversation
    );
    assert_eq!(
        resolve_omp_settings(&scopes, "fp", "/else", &OmpSettingValues::default())
            .thinking_level
            .value
            .as_deref(),
        Some("default")
    );
}

#[test]
fn omp_model_splits_only_the_first_slash() {
    let model = split_omp_model("openrouter/anthropic/claude").expect("qualified model");
    assert_eq!(model.provider, "openrouter");
    assert_eq!(model.model_id, "anthropic/claude");
    assert_eq!(join_omp_model(&model), "openrouter/anthropic/claude");
    for invalid in ["nomodel", "", "/model", "provider/"] {
        assert_eq!(split_omp_model(invalid), None);
    }
}
