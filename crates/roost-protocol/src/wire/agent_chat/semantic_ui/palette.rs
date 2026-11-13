//! Complete active OMP theme palettes in the canonical semantic document.
//! Palette messages replace rather than merge, so previews and reconnects
//! cannot retain tokens from a different Theme instance.
use super::*;

pub fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 64
        && token.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && token.as_bytes()[0].is_ascii_alphabetic()
}

pub fn valid_color(color: &str) -> bool {
    matches!(color.len(), 4 | 5 | 7 | 9)
        && color.starts_with('#')
        && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

pub(super) fn bytes(colors: &BTreeMap<String, String>) -> usize {
    colors
        .iter()
        .map(|(token, color)| token.len() + color.len() + 8)
        .sum()
}

pub(super) fn validate(colors: &BTreeMap<String, String>) -> UiResult<()> {
    if colors.len() > 256 {
        return Err(OmpUiDocumentError::Limit);
    }
    if colors
        .iter()
        .any(|(token, color)| !valid_token(token) || !valid_color(color))
    {
        return Err(OmpUiDocumentError::Invalid("palette color"));
    }
    Ok(())
}

impl OmpUiDocument {
    pub(super) fn replace_palette(&mut self, body: &Value) -> UiResult<()> {
        let colors = body
            .get("colors")
            .and_then(Value::as_object)
            .ok_or(OmpUiDocumentError::Invalid("palette colors"))?;
        if colors.len() > 256 {
            return Err(OmpUiDocumentError::Limit);
        }
        let mut replacement = BTreeMap::new();
        for (token, color) in colors {
            let color = color
                .as_str()
                .filter(|color| valid_color(color))
                .ok_or(OmpUiDocumentError::Invalid("palette color"))?;
            if !valid_token(token) {
                return Err(OmpUiDocumentError::Invalid("palette token"));
            }
            replacement.insert(token.clone(), color.to_owned());
        }
        let total = self
            .usage_bytes
            .unwrap_or(0)
            .saturating_sub(bytes(&self.palette))
            .saturating_add(bytes(&replacement));
        if total > MAX_TSP_DOCUMENT_BYTES {
            return Err(OmpUiDocumentError::Limit);
        }
        self.palette = replacement;
        self.usage_bytes = Some(total);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn palette_replacement_snapshot_and_reconnect_never_merge_stale_tokens() -> UiResult<()> {
        let mut document = OmpUiDocument::default();
        document.apply_message(
            &json!({"verb":"t","body":{"colors":{"userMessageBg":"#221d1a","accent":"#abcdef"}}}),
        )?;
        document.apply_message(&json!({"verb":"o","body":{"id":"main"}}))?;
        let snapshot = serde_json::to_value(&document).unwrap();
        let mut restored: OmpUiDocument = serde_json::from_value(snapshot).unwrap();
        assert_eq!(document, restored);
        restored
            .apply_message(&json!({"verb":"t","body":{"colors":{"userMessageBg":"#ffffff"}}}))?;
        assert_eq!(
            restored.palette.get("userMessageBg").map(String::as_str),
            Some("#ffffff")
        );
        assert!(!restored.palette.contains_key("accent"));
        restored.apply_message(&json!({"verb":"o","body":{"id":"main","adopt":true}}))?;
        assert_eq!(restored.palette.len(), 1);
        Ok(())
    }

    #[test]
    fn invalid_palette_is_atomic_and_css_cannot_escape_the_contract() -> UiResult<()> {
        let mut document = OmpUiDocument::default();
        document.apply_message(&json!({"verb":"t","body":{"colors":{"accent":"#abcdef"}}}))?;
        let before = document.clone();
        for colors in [
            json!({"accent":"red;display:none"}),
            json!({"bad-token":"#ffffff"}),
            json!({"accent":12}),
        ] {
            assert!(
                document
                    .apply_message(&json!({"verb":"t","body":{"colors":colors}}))
                    .is_err()
            );
            assert_eq!(document, before);
        }
        document.apply_message(&json!({"verb":"t","body":{"colors":{}}}))?;
        assert!(document.palette.is_empty());
        Ok(())
    }
}
