//! TSP surface and blob lifecycle with aggregate resource accounting.
//! Frame transactions receive the remaining document budget before mutation.
//! Blob replacement validates first, then moves only the incoming payload.

use super::*;

impl OmpUiDocument {
    pub fn apply_message(&mut self, message: &Value) -> UiResult<()> {
        index::json_bytes(message, MAX_TSP_MESSAGE_BYTES)?;
        self.ensure_usage()?;
        let verb = message
            .get("verb")
            .and_then(Value::as_str)
            .ok_or(OmpUiDocumentError::Invalid("verb"))?;
        let body = message
            .get("body")
            .ok_or(OmpUiDocumentError::Invalid("body"))?;
        match verb {
            "q" => validate_query(body),
            "o" => self.open(body),
            "f" => self.frame(body),
            "x" => self.close(body),
            "b" => self.blob(message),
            "t" => self.replace_palette(body),
            _ => Err(OmpUiDocumentError::Invalid("unsupported message verb")),
        }
    }

    /// Drain one surface's pending reveal/scroll requests for the paint that
    /// is about to apply them. A surface the document no longer holds has none.
    pub fn take_intents(&mut self, surface_id: &str) -> Vec<OmpUiIntent> {
        self.surfaces
            .get_mut(surface_id)
            .map(OmpUiSurface::take_intents)
            .unwrap_or_default()
    }

    fn ensure_usage(&mut self) -> UiResult<()> {
        if self.usage_bytes.is_some() {
            return Ok(());
        }
        if self.surfaces.len() > MAX_TSP_SURFACES || self.blobs.len() > 256 {
            return Err(OmpUiDocumentError::Limit);
        }
        palette::validate(&self.palette)?;
        let mut bytes = palette::bytes(&self.palette);
        for surface in self.surfaces.values_mut() {
            index::ensure(surface)?;
            bytes = bytes.saturating_add(index::bytes(surface)?);
        }
        for (id, blob) in &self.blobs {
            bytes = bytes.saturating_add(blob_bytes(id, &blob.mime, &blob.data_base64));
        }
        if bytes > MAX_TSP_DOCUMENT_BYTES {
            return Err(OmpUiDocumentError::Limit);
        }
        self.usage_bytes = Some(bytes);
        Ok(())
    }

    fn open(&mut self, body: &Value) -> UiResult<()> {
        let id = string(body, "id")?;
        if body.get("adopt").and_then(Value::as_bool) == Some(true) {
            let existing = self.surfaces.get_mut(&id).ok_or(OmpUiDocumentError::Gone)?;
            existing.suspended = false;
            existing.intents.clear();
            return Ok(());
        }
        if self.surfaces.len() >= MAX_TSP_SURFACES && !self.surfaces.contains_key(&id) {
            return Err(OmpUiDocumentError::Limit);
        }
        let mode = body.get("mode").and_then(Value::as_str).unwrap_or("inline");
        if !matches!(mode, "inline" | "screen") {
            return Err(OmpUiDocumentError::Invalid("surface mode"));
        }
        let mut surface = OmpUiSurface {
            id: id.clone(),
            mode: mode.to_owned(),
            title: optional_text(body, "title", 32 * 1024)?,
            role: optional_text(body, "role", 256)?,
            sequence: 0,
            root: OmpUiNode {
                id: id.clone(),
                k: "col".into(),
                p: Map::new(),
                c: Vec::new(),
            },
            focus: None,
            suspended: false,
            runtime: None,
            intents: Vec::new(),
        };
        index::ensure(&mut surface)?;
        let before = self
            .surfaces
            .get(&id)
            .map(index::bytes)
            .transpose()?
            .unwrap_or(0);
        let after = index::bytes(&surface)?;
        let total = self
            .usage_bytes
            .unwrap_or(0)
            .saturating_sub(before)
            .saturating_add(after);
        if total > MAX_TSP_DOCUMENT_BYTES {
            return Err(OmpUiDocumentError::Limit);
        }
        self.surfaces.insert(id, surface);
        self.usage_bytes = Some(total);
        Ok(())
    }

    fn frame(&mut self, body: &Value) -> UiResult<()> {
        let id = string(body, "sf")?;
        let sequence = body
            .get("s")
            .and_then(Value::as_u64)
            .ok_or(OmpUiDocumentError::Invalid("sequence"))?;
        let ops = body
            .get("ops")
            .and_then(Value::as_array)
            .ok_or(OmpUiDocumentError::Invalid("ops"))?;
        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or(OmpUiDocumentError::Operation)?;
        let before = index::bytes(surface)?;
        let other_bytes = self.usage_bytes.unwrap_or(0).saturating_sub(before);
        frame::apply(
            surface,
            sequence,
            ops,
            MAX_TSP_DOCUMENT_BYTES.saturating_sub(other_bytes),
        )?;
        self.usage_bytes = Some(other_bytes.saturating_add(index::bytes(surface)?));
        Ok(())
    }

    fn close(&mut self, body: &Value) -> UiResult<()> {
        let id = string(body, "id")?;
        let keep = body
            .get("keep")
            .and_then(Value::as_bool)
            .ok_or(OmpUiDocumentError::Invalid("keep"))?;
        let Some(surface) = self.surfaces.get_mut(&id) else {
            return Ok(());
        };
        let before = index::bytes(surface)?;
        if keep {
            surface.root.c.retain(|node| node.id == "main");
            surface.focus = None;
            surface.suspended = true;
            surface.runtime = None;
            surface.intents.clear();
            index::ensure(surface)?;
            self.usage_bytes = Some(
                self.usage_bytes
                    .unwrap_or(0)
                    .saturating_sub(before)
                    .saturating_add(index::bytes(surface)?),
            );
        } else {
            self.surfaces.remove(&id);
            self.usage_bytes = Some(self.usage_bytes.unwrap_or(0).saturating_sub(before));
        }
        Ok(())
    }

    fn blob(&mut self, message: &Value) -> UiResult<()> {
        let headers = message
            .get("headers")
            .ok_or(OmpUiDocumentError::Invalid("blob headers"))?;
        let id = string(headers, "id")?;
        let mime = headers
            .get("mime")
            .and_then(Value::as_str)
            .filter(|mime| {
                mime.len() <= 128
                    && mime.contains('/')
                    && mime.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'-' | b'.')
                    })
            })
            .ok_or(OmpUiDocumentError::Invalid("blob mime"))?;
        let data = message
            .get("body")
            .and_then(Value::as_str)
            .ok_or(OmpUiDocumentError::Invalid("blob data"))?;
        if data.len() % 4 != 0
            || !data
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        {
            return Err(OmpUiDocumentError::Invalid("blob base64"));
        }
        if self.blobs.len() >= 256 && !self.blobs.contains_key(&id) {
            return Err(OmpUiDocumentError::Limit);
        }
        let before = self
            .blobs
            .get(&id)
            .map_or(0, |blob| blob_bytes(&id, &blob.mime, &blob.data_base64));
        let total = self
            .usage_bytes
            .unwrap_or(0)
            .saturating_sub(before)
            .saturating_add(blob_bytes(&id, mime, data));
        if total > MAX_TSP_DOCUMENT_BYTES {
            return Err(OmpUiDocumentError::Limit);
        }
        self.blobs.insert(
            id,
            OmpUiBlob {
                mime: mime.to_owned(),
                data_base64: data.to_owned(),
            },
        );
        self.usage_bytes = Some(total);
        Ok(())
    }
}

fn blob_bytes(id: &str, mime: &str, data: &str) -> usize {
    id.len() + mime.len() + data.len() + 64
}
fn optional_text(value: &Value, key: &'static str, limit: usize) -> UiResult<Option<String>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.len() <= limit => Ok(Some(text.clone())),
        _ => Err(OmpUiDocumentError::Invalid(key)),
    }
}
fn validate_query(body: &Value) -> UiResult<()> {
    match body.get("q").and_then(Value::as_str) {
        Some("hello")
            if body.get("v").is_some_and(Value::is_array)
                && body.get("app").is_some_and(Value::is_string) =>
        {
            Ok(())
        }
        Some("blobs") if body.get("ids").is_some_and(Value::is_array) => Ok(()),
        _ => Err(OmpUiDocumentError::Invalid("query")),
    }
}
