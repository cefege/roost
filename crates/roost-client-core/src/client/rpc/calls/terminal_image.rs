//! The coordinator method a terminal pane uses to fetch one retained image.
//!
//! Called through roost-web's `CoordRpc::call`; the image bytes stay outside
//! cell lanes and are fetched only when a rendered placement needs them.

use roost_proto::{SessionsGetTerminalImageRequest, SessionsGetTerminalImageResponse};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `SessionsGetTerminalImage`: fetch the PNG for one retained image key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalImage {
    /// The session whose terminal owns the image.
    pub session_id: String,
    /// The retained image's key.
    pub image_key: u64,
}

impl UnaryMethod for TerminalImage {
    const METHOD: &'static str = "SessionsGetTerminalImage";
    type Response = Vec<u8>;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsGetTerminalImageRequest {
                session_id: self.session_id.clone(),
                image_key: self.image_key,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Vec<u8>, RpcCodecError> {
        let response: SessionsGetTerminalImageResponse = decode_message(Self::METHOD, body)?;
        Ok(response.png)
    }
}
