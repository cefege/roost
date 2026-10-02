//! `__smoke.uploadAttachment(sessionId, sizeBytes, filename?)`: a synthetic
//! file of distinct bytes through the product's own chunked relay upload,
//! answered as `{ abs_path }`. wasm32 only; `smoke::dispatch` routes the call
//! here and `components::terminal_chrome::upload` owns the upload. Ports
//! `apps/web/src/smoke/smokeFileTransferProbes.ts:16-23`.

use std::rc::Rc;

use serde_json::json;

use super::backdoor::{Reply, SmokeBackdoor};
use super::call::UploadAttachmentRequest;
use crate::components::terminal_chrome::upload::upload_attachment;

impl SmokeBackdoor {
    pub(super) fn upload_attachment_call(
        self: &Rc<Self>,
        request: UploadAttachmentRequest,
    ) -> Reply {
        let this = Rc::clone(self);
        Reply::Later(Box::pin(async move {
            let size = usize::try_from(request.size_bytes)
                .map_err(|_| format!("{} bytes does not fit this page", request.size_bytes))?;
            // Distinct bytes, so a reordered or duplicated chunk changes the
            // stored digest instead of reassembling into the same file.
            let bytes = (0..size).map(|index| (index & 0xff) as u8).collect();
            let abs_path =
                upload_attachment(&this.pump, &request.session_id, request.filename, bytes).await?;
            Ok(Some(json!({ "abs_path": abs_path })))
        }))
    }
}
