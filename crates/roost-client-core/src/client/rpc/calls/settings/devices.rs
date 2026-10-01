//! The browser identities authorized on this coordinator: list, revoke, and
//! rotate this device's own key.
//!
//! Called by roost-web's Settings devices pane. v2 call sites:
//! `apps/web/src/components/Settings/DevicesPane.tsx:35-64` (`devicesList`,
//! `devicesRevoke`, `rotateCurrentWebKey`). A refusal is an `Err` the pane
//! renders, never an empty list: a coordinator that denied the read must not
//! look like a coordinator with no devices.

use roost_proto::{
    DeviceRow, DevicesListRequest, DevicesListResponse, DevicesRevokeRequest,
    DevicesRevokeResponse, DevicesRotateCurrentRequest, DevicesRotateCurrentResponse,
};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// One authorized browser.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthorizedDevice {
    /// The authorized key's fingerprint.
    pub fingerprint: String,
    /// The label the device paired with, empty when it paired unnamed.
    pub label: String,
    /// When the key was authorized.
    pub added_at_ms: u64,
    /// Whether this is the browser asking.
    pub is_self: bool,
    /// The address the pairing came from, when the coordinator recorded one.
    pub paired_from_ip: String,
    /// The country the pairing resolved to.
    pub paired_country: String,
    /// The user agent the pairing presented.
    pub paired_user_agent: String,
    /// The signed-in identity behind a managed pairing.
    pub paired_edge_identity: String,
}

impl AuthorizedDevice {
    fn from_proto(row: &DeviceRow) -> Self {
        Self {
            fingerprint: row.fingerprint.clone(),
            label: row.label.clone(),
            added_at_ms: row.added_at_ms,
            is_self: row.is_self,
            paired_from_ip: row.paired_from_ip.clone(),
            paired_country: row.paired_country.clone(),
            paired_user_agent: row.paired_user_agent.clone(),
            paired_edge_identity: row.paired_edge_identity.clone(),
        }
    }
}

/// The provenance line a device row shows under its fingerprint.
pub fn pairing_provenance(device: &AuthorizedDevice) -> String {
    let origin = [
        device.paired_from_ip.as_str(),
        device.paired_country.as_str(),
    ]
    .into_iter()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let origin_label = if origin.is_empty() {
        String::new()
    } else {
        format!("Paired from {origin}")
    };
    let identity_label = match device.paired_edge_identity.trim() {
        "" => String::new(),
        identity => format!("Signed in as {identity}"),
    };
    [origin_label, identity_label]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `DevicesList`: every browser this coordinator authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListDevices;

impl UnaryMethod for ListDevices {
    const METHOD: &'static str = "DevicesList";
    type Response = Vec<AuthorizedDevice>;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &DevicesListRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<Vec<AuthorizedDevice>, RpcCodecError> {
        let response: DevicesListResponse = decode_message(Self::METHOD, body)?;
        Ok(response
            .devices
            .iter()
            .map(AuthorizedDevice::from_proto)
            .collect())
    }
}

/// `DevicesRevoke`: permanently drop one browser's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokeDevice {
    /// The fingerprint to revoke.
    pub fingerprint: String,
}

impl UnaryMethod for RevokeDevice {
    const METHOD: &'static str = "DevicesRevoke";
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &DevicesRevokeRequest {
                fingerprint: self.fingerprint.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<bool, RpcCodecError> {
        let response: DevicesRevokeResponse = decode_message(Self::METHOD, body)?;
        Ok(response.ok)
    }
}

/// `DevicesRotateCurrent`: replace this browser's key with the one it offers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RotateCurrentDevice {
    /// The new public key, base64.
    pub ssh_pubkey_b64: String,
    /// The label the new key is authorized under.
    pub label: String,
}

impl UnaryMethod for RotateCurrentDevice {
    const METHOD: &'static str = "DevicesRotateCurrent";
    type Response = String;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &DevicesRotateCurrentRequest {
                ssh_pubkey_b64: self.ssh_pubkey_b64.clone(),
                label: self.label.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<String, RpcCodecError> {
        let response: DevicesRotateCurrentResponse = decode_message(Self::METHOD, body)?;
        Ok(response.fingerprint)
    }
}
