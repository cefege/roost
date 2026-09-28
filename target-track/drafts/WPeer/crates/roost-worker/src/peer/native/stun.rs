//! The one STUN exchange the native peer makes itself: a Binding request to an
//! operator STUN server, and the mapped address its success response carries,
//! which becomes the server-reflexive candidate (RFC 5389 §6, §15.1, §15.2).
//! Called by `peer::native::gather`. Stands in for the reflexive gathering
//! libjuice did inside node-datachannel for v2
//! `apps/worker/src/terminal/peer/terminal-peer-native.ts`.

use std::fs::File;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const MAGIC_COOKIE: u32 = 0x2112_A442;
const MAPPED_ADDRESS: u16 = 0x0001;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
const HEADER_BYTES: usize = 20;
const FAMILY_IPV4: u8 = 0x01;
const FAMILY_IPV6: u8 = 0x02;
/// The kernel generator, as `host::jwt` reads it: the product is POSIX-only.
const ENTROPY_SOURCE: &str = "/dev/urandom";

pub(super) type TransactionId = [u8; 12];

pub(super) fn new_transaction_id() -> io::Result<TransactionId> {
    let mut id = [0u8; 12];
    File::open(ENTROPY_SOURCE)?.read_exact(&mut id)?;
    Ok(id)
}

/// An attribute-free Binding request: STUN servers answer it without
/// credentials, and nothing in it names this worker.
pub(super) fn binding_request(transaction: &TransactionId) -> [u8; HEADER_BYTES] {
    let mut request = [0u8; HEADER_BYTES];
    request[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    request[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    request[8..20].copy_from_slice(transaction);
    request
}

/// The reflexive address a Binding success for `transaction` reports, or
/// `None` for any other datagram. XOR-MAPPED-ADDRESS wins over the legacy
/// MAPPED-ADDRESS when a server sends both.
pub(super) fn mapped_address(datagram: &[u8], transaction: &TransactionId) -> Option<SocketAddr> {
    if read_u16(datagram, 0)? != BINDING_SUCCESS
        || read_u32(datagram, 4)? != MAGIC_COOKIE
        || datagram.get(8..HEADER_BYTES)? != transaction.as_slice()
    {
        return None;
    }
    let length = usize::from(read_u16(datagram, 2)?);
    let body = datagram.get(HEADER_BYTES..HEADER_BYTES + length)?;
    let mut legacy = None;
    let mut offset = 0;
    while offset + 4 <= body.len() {
        let kind = read_u16(body, offset)?;
        let value_bytes = usize::from(read_u16(body, offset + 2)?);
        let value = body.get(offset + 4..offset + 4 + value_bytes)?;
        match kind {
            XOR_MAPPED_ADDRESS => return decode_address(value, Some(transaction)),
            MAPPED_ADDRESS => legacy = decode_address(value, None),
            _ => {}
        }
        offset += 4 + value_bytes.next_multiple_of(4);
    }
    legacy
}

fn decode_address(value: &[u8], xor: Option<&TransactionId>) -> Option<SocketAddr> {
    let family = *value.get(1)?;
    let cookie = MAGIC_COOKIE.to_be_bytes();
    let mut port = read_u16(value, 2)?;
    if xor.is_some() {
        port ^= u16::from_be_bytes([cookie[0], cookie[1]]);
    }
    let ip = match family {
        FAMILY_IPV4 => {
            let mut octets: [u8; 4] = value.get(4..8)?.try_into().ok()?;
            if xor.is_some() {
                octets.iter_mut().zip(cookie).for_each(|(octet, mask)| *octet ^= mask);
            }
            IpAddr::V4(Ipv4Addr::from(octets))
        }
        FAMILY_IPV6 => {
            let mut octets: [u8; 16] = value.get(4..20)?.try_into().ok()?;
            if let Some(transaction) = xor {
                let mask = cookie.iter().chain(transaction.iter());
                octets.iter_mut().zip(mask).for_each(|(octet, mask)| *octet ^= mask);
            }
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let pair = bytes.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([pair[0], pair[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let quad = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([quad[0], quad[1], quad[2], quad[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 5769 §2.2: the IPv4 Binding success test vector's transaction and
    /// XOR-MAPPED-ADDRESS (192.0.2.1:32853), trimmed to that one attribute.
    #[test]
    fn a_binding_success_yields_its_xor_mapped_address() {
        let transaction: TransactionId = [
            0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae,
        ];
        let mut response = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
        response.extend_from_slice(&transaction);
        response.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xa1, 0x47]);
        response.extend_from_slice(&[0xe1, 0x12, 0xa6, 0x43]);
        let mapped = mapped_address(&response, &transaction);
        assert_eq!(mapped, Some("192.0.2.1:32853".parse().unwrap()));
        let mut other = transaction;
        other[0] ^= 1;
        assert_eq!(mapped_address(&response, &other), None);
    }

    #[test]
    fn a_binding_request_is_a_bare_header() {
        let request = binding_request(&[7; 12]);
        assert_eq!(&request[0..8], &[0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42]);
        assert_eq!(&request[8..], &[7; 12]);
    }
}
