//! Session presentation negotiation and lease-local renewal snapshots.
//! Opens and viewport events update the existing native transport in place.
//! Unchanged renewals cannot replace another view's last negotiated geometry.

use super::*;

impl UiState {
    pub(super) fn open(
        &mut self,
        request: &DOmpUi,
        params: &Value,
        emitter: &mut Emitter,
    ) -> Result<bool, Error> {
        if !valid_view_id(&request.view_id) || !valid_view_id(&request.tab_id) {
            return Err(Error::invalid_params());
        }
        if !self.views.contains_key(&request.client_id) && self.views.len() >= MAX_VIEWS {
            return Err(Error::new(-32000, "Too many open OMP views."));
        }
        let reset = params.get("reset").and_then(Value::as_bool) == Some(true);
        if self.opened && !reset {
            let changed = self
                .views
                .get(&request.client_id)
                .and_then(|lease| lease.presentation.as_deref())
                .is_none_or(|previous| !same_presentation(previous, params));
            let transport_id = self.transport_id.clone();
            if !changed {
                self.lease(request, &transport_id);
                return Ok(false);
            }
            let forward = self
                .open_params
                .as_deref()
                .is_none_or(|previous| !same_presentation(previous, params));
            let presentation = if forward {
                snapshot(params)
            } else {
                self.open_params.clone().ok_or_else(Error::invalid_params)?
            };
            self.lease(request, &transport_id).presentation = Some(Arc::clone(&presentation));
            self.open_params = Some(presentation);
            if forward {
                tracing::debug!(conversation_id=%emitter.conversation_id, view_id=%request.view_id, "OMP semantic UI presentation changed");
            }
            return Ok(forward);
        }
        self.resources.registry.revoke_transport(
            &self.resources.owner_id,
            &emitter.conversation_id,
            &self.transport_id,
        );
        self.views.clear();
        self.acknowledgements.clear();
        self.receipts.cancel_all();
        self.host_services.cancel_all(emitter);
        self.transport_id = request.request_id.clone();
        let presentation = snapshot(params);
        self.open_params = Some(Arc::clone(&presentation));
        self.opened = true;
        let transport_id = self.transport_id.clone();
        self.lease(request, &transport_id).presentation = Some(presentation);
        emitter.emit(OmpWorkerEvent::UiReset {
            transport_id: Some(self.transport_id.clone()),
        });
        tracing::info!(
            conversation_id = %emitter.conversation_id,
            tab_id = %request.tab_id,
            "OMP semantic UI opening"
        );
        Ok(true)
    }

    pub(super) fn remember_presentation_event(&mut self, client_id: &str, params: &Value) {
        let Some(event) = params.get("event") else {
            return;
        };
        let properties: &[(&str, &str)] = match event.get("ev").and_then(Value::as_str) {
            Some("resize") => &[("cols", "cols"), ("rows", "rows")],
            Some("theme") => &[("dark", "dark")],
            Some("motion") => &[("reduce", "reduceMotion")],
            _ => return,
        };
        let Some(lease) = self.views.get_mut(client_id) else {
            return;
        };
        let shared = match (&self.open_params, &lease.presentation) {
            (Some(current), Some(previous)) => Arc::ptr_eq(current, previous),
            _ => false,
        };
        update_presentation(&mut self.open_params, event, properties);
        if shared {
            lease.presentation = self.open_params.clone();
        } else {
            update_presentation(&mut lease.presentation, event, properties);
        }
    }
}

fn same_presentation(previous: &Value, next: &Value) -> bool {
    let (Some(previous), Some(next)) = (previous.as_object(), next.as_object()) else {
        return false;
    };
    previous.len() == next.len() - usize::from(next.contains_key("reset"))
        && previous
            .iter()
            .all(|(key, value)| next.get(key) == Some(value))
}

fn snapshot(params: &Value) -> Arc<Value> {
    let mut params = params.clone();
    if let Some(fields) = params.as_object_mut() {
        fields.remove("reset");
    }
    Arc::new(params)
}

fn update_presentation(
    presentation: &mut Option<Arc<Value>>,
    event: &Value,
    properties: &[(&str, &str)],
) {
    let Some(presentation) = presentation.as_mut() else {
        return;
    };
    if !properties.iter().any(|(from, to)| {
        event
            .get(*from)
            .is_some_and(|value| presentation.get(*to) != Some(value))
    }) {
        return;
    }
    let Some(fields) = Arc::make_mut(presentation).as_object_mut() else {
        return;
    };
    for (from, to) in properties {
        if let Some(value) = event.get(*from) {
            fields.insert((*to).to_owned(), value.clone());
        }
    }
}
