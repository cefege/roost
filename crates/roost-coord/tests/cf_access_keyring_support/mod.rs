//! The RS256 key material the Cloudflare key-ring tests sign with: four fixed
//! primes and the functions that turn them into a JWK and a signature.
//!
//! FIXED PRIMES, NOT A GENERATED KEY, so the modulus is a value this test KNOWS
//! rather than one it must read back out of a type that does not expose it, and
//! so "a signature from another key" is a statement about two distinct keys
//! rather than one key twice. All four were generated once and verified: 1024
//! bits each, prime under Miller-Rabin, each product 2048 bits, and the two
//! products different.

use rsa::pkcs1v15::{Signature, SigningKey};
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::{BigUint, RsaPrivateKey};

use super::KEY_ID;
/// Two fixed 1024-bit primes, so the key is byte-identical on every run and the
/// modulus is a value this test KNOWS rather than one it must read back out of a
/// type that does not expose it. All four were generated once and verified:
/// 1024 bits each, prime under Miller-Rabin, each product 2048 bits, and the
/// two products different -- which is what makes "a signature from another key"
/// a statement about two distinct keys rather than one key twice.
const PRIME_P: &str = "1256234485391403936069559009783797527569811472016129160064473854898\
29252760021427778249067215250508715797248512159121409128187046642946770621429909\
65039884870973109759188673567519824317682528955914313548056705220041441515336177888\
7133885858432211334786287197210268041769498174748619370241719169896298355957383";
const PRIME_Q: &str = "14571679302933428459799886630031973708132018330787835789829531970544431\
350898708879164276545211511952089513389750016974975229117996296678389787198627061\
91415074290415129207881324904008526440142560210028202258533477691058875661111422\
79509967596263315186727778295066699460738154683242024853946831219372489356637";
const PRIME_P_ALT: &str = "10335305399059514569615015046132475415387634558092062621070621479014320\
32761416378253806255443499593346409788873309168019857363152923678874615009525753066\
85830948408280625577924191838674079936039699014513877527324405528104482750782718\
602101275622147505784295939666850098470236456707503842742425560077271560049";
const PRIME_Q_ALT: &str = "16657826182786747677728115025911402530088647072166297659670736995035221\
25377449794127521143886155905471321585901512789889948405163956522145796380300430\
17584033742954692200576318385790526501520603410928354200488724169107175047630310\
778812394232891121945075144845356715714440266914865918449073954075459868160241";
const PUBLIC_EXPONENT: u32 = 65537;

/// A key built from two known primes, plus the modulus and exponent its JWK
/// publishes. `RsaPublicKey`'s `n` and `e` are PRIVATE with no accessor, so a
/// JWK cannot be read back out of a key; `RsaPrivateKey::from_p_q` computes
/// `n = p*q` itself, which is how to KNOW the modulus. `rsa-0.9.10/src/key.rs:287`.
fn test_key(p_decimal: &str, q_decimal: &str) -> (RsaPrivateKey, BigUint, BigUint) {
    let p = big_uint(p_decimal);
    let q = big_uint(q_decimal);
    let exponent = BigUint::from(PUBLIC_EXPONENT);
    let private =
        RsaPrivateKey::from_p_q(p.clone(), q.clone(), exponent.clone()).expect("a valid key pair");
    (private, p * q, exponent)
}

/// The key every RSA test in this file signs with.
fn the_test_key() -> (RsaPrivateKey, BigUint, BigUint) {
    test_key(PRIME_P, PRIME_Q)
}

/// A decimal constant as a `BigUint`. `parse_bytes` is the INHERENT constructor;
/// `from_str_radix` is a `num_traits::Num` method and this crate does not depend
/// on `num-traits`, so that name resolves to nothing here. A `None` is not a
/// runtime condition: the input is a constant in this file.
fn big_uint(decimal: &str) -> BigUint {
    BigUint::parse_bytes(decimal.as_bytes(), 10)
        .unwrap_or_else(|| panic!("PRIME_P/PRIME_Q must be decimal digits: {decimal}"))
}

/// The JWK -- the single key object, NOT the `{"keys":[...]}` document.
///
/// `RsaJwks::verify_rs256` takes the key a `kid` NAMED: the gate pulls the
/// entry out of the document with `jwk_in` and passes THAT. Handed the whole
/// document, `key.get("n")` reads `None` and every verification refuses -- so a
/// "must not verify" near-miss assertion passes for a reason that has nothing
/// to do with the near-miss it is about, and only a "must verify" row can tell.
fn jwk_for(modulus: &BigUint, exponent: &BigUint) -> String {
    let n = roost_host::b64url_encode(&modulus.to_bytes_be());
    let e = roost_host::b64url_encode(&exponent.to_bytes_be());
    format!(r#"{{"kty":"RSA","alg":"RS256","use":"sig","kid":"{KEY_ID}","n":"{n}","e":"{e}"}}"#)
}

/// A real RS256 signature over a real `header.payload`.
fn sign(signing_input: &str, private: &RsaPrivateKey) -> Vec<u8> {
    let signature: Signature =
        SigningKey::<Sha256>::new(private.clone()).sign(signing_input.as_bytes());
    signature.to_vec()
}
