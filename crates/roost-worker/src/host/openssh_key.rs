//! The OpenSSH private key container for an ed25519 key: the format the
//! worker's key file is written in, and the only reader of it. Called by
//! `host::jwt` when a key file is loaded or written, and by nothing else.
//! Reproduced from `apps/worker/src/host/jwt.ts` (`parseOpenSshEd25519`,
//! `encodeOpenSshEd25519`) so a key written here is byte-for-byte the file a v2
//! install holds, and an existing one still parses.
//!
//! A real parse, not v2's scan for the 32-byte public key appearing twice. A
//! scan cannot tell an unencrypted key from one whose body is ciphertext under
//! a passphrase, and signing with those bytes produces a signature no
//! coordinator can verify — presented as an unknown `kid`, with nothing in the
//! worker's own logs to explain it.

use base64::prelude::{BASE64_STANDARD, Engine as _};

const OPENSSH_MAGIC: &[u8; 15] = b"openssh-key-v1\0";
const KEY_TYPE: &[u8] = b"ssh-ed25519";
const PEM_BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const PEM_END: &str = "-----END OPENSSH PRIVATE KEY-----";

/// The base64 line width OpenSSH wraps at.
const PEM_LINE: usize = 70;

/// The `none` cipher's block size, which is what a key with no cipher is padded
/// to. The padding bytes are 1, 2, 3 … and are checked, not skipped.
const PAD_BLOCK: usize = 8;

/// The integrity word pair written into an unencrypted private block. Any
/// consistent value is valid; this is v2's, so a key file this writes differs
/// from v2's only in the key material.
const CHECK_WORD: u32 = 0x1234_5678;

/// A reader over the length-prefixed fields of an OpenSSH key document.
struct Fields<'a> {
    rest: &'a [u8],
}

impl<'a> Fields<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    /// The next `count` bytes, or a refusal naming what ran out.
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        if self.rest.len() < count {
            return Err(format!(
                "the document ends after {} bytes where {count} were read",
                self.rest.len()
            ));
        }
        let (head, tail) = self.rest.split_at(count);
        self.rest = tail;
        Ok(head)
    }

    fn word(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        let mut word = [0u8; 4];
        word.copy_from_slice(bytes);
        Ok(u32::from_be_bytes(word))
    }

    /// A `uint32`-length-prefixed byte string.
    fn string(&mut self) -> Result<&'a [u8], String> {
        let length = self.word()? as usize;
        self.take(length)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.string()?
            .try_into()
            .map_err(|_| format!("expected {N} bytes where a shorter field was read"))
    }
}

/// The seed and the public key of an unencrypted OpenSSH ed25519 key.
///
/// The public key is read out of the file's PUBLIC block and the seed out of
/// its PRIVATE block, and the two are compared in both directions: a document
/// whose halves disagree is a document whose identity is not knowable, and
/// this worker would then dial as a fingerprint the coordinator has never seen.
pub(crate) fn parse_openssh_ed25519(pem: &str) -> Result<([u8; 32], [u8; 32]), String> {
    let body = decode_pem(pem)?;
    let mut document = Fields::new(&body);
    if document.take(OPENSSH_MAGIC.len())? != OPENSSH_MAGIC {
        return Err("it is not an openssh-key-v1 document".to_string());
    }
    let cipher = document.string()?;
    if cipher != b"none" {
        return Err(format!(
            "it is encrypted with {}; this worker signs with an unencrypted key only",
            String::from_utf8_lossy(cipher)
        ));
    }
    if document.string()? != b"none" {
        return Err("it names a key derivation function, so it is not a `none` \
                    cipher document"
            .to_string());
    }
    if !document.string()?.is_empty() {
        return Err("an unencrypted key has no kdf options".to_string());
    }
    let key_count = document.word()?;
    if key_count != 1 {
        return Err(format!(
            "it holds {key_count} keys; this worker signs with exactly one"
        ));
    }
    let mut public_block = Fields::new(document.string()?);
    if public_block.string()? != KEY_TYPE {
        return Err(format!(
            "its key type is not {KEY_TYPE}; this worker signs with ed25519 only"
        ));
    }
    let public: [u8; 32] = public_block.array()?;
    let mut private_block = Fields::new(document.string()?);
    let first = private_block.word()?;
    if first != private_block.word()? {
        return Err("its two integrity words disagree, so the body is damaged".to_string());
    }
    if private_block.string()? != KEY_TYPE {
        return Err("its private block names a different key type".to_string());
    }
    if private_block.array::<32>()? != public {
        return Err(
            "its private block names a different public key than the file does".to_string(),
        );
    }
    let secret = private_block.string()?;
    let seed: [u8; 32] = secret
        .get(..32)
        .and_then(|head| head.try_into().ok())
        .ok_or_else(|| format!("its private key is {} bytes, not 64", secret.len()))?;
    if secret.len() != 64 || secret[32..] != public {
        return Err("its private key does not carry its own public key".to_string());
    }
    check_padding(&private_block)?;
    Ok((seed, public))
}

/// The OpenSSH PEM for an unencrypted ed25519 key, wrapped the way OpenSSH
/// wraps one.
pub(crate) fn encode_openssh_ed25519(seed: &[u8; 32], public: &[u8; 32]) -> String {
    let mut body = Vec::with_capacity(256);
    body.extend_from_slice(OPENSSH_MAGIC);
    push_string(&mut body, b"none");
    push_string(&mut body, b"none");
    push_string(&mut body, b"");
    body.extend_from_slice(&1u32.to_be_bytes());
    push_string(&mut body, &public_block(public));
    push_string(&mut body, &private_block(seed, public));
    let encoded = BASE64_STANDARD.encode(&body);
    let wrapped = encoded
        .as_bytes()
        .chunks(PEM_LINE)
        .map(|line| std::str::from_utf8(line))
        .collect::<Result<Vec<&str>, _>>()
        .unwrap_or_default()
        .join("\n");
    format!("{PEM_BEGIN}\n{wrapped}\n{PEM_END}\n")
}

/// The padding after the private fields: 1, 2, 3 … up to the block size, and
/// every one of them checked, because a key whose padding is wrong is refused
/// by every OpenSSH client that reads it.
fn check_padding(private_block: &Fields<'_>) -> Result<(), String> {
    let mut expected: u8 = 1;
    for byte in private_block.rest {
        if *byte != expected {
            return Err(format!("its padding byte {expected} is missing"));
        }
        expected = expected.wrapping_add(1);
    }
    Ok(())
}

fn public_block(public: &[u8; 32]) -> Vec<u8> {
    let mut block = Vec::with_capacity(KEY_TYPE.len() + 4 + public.len());
    push_string(&mut block, KEY_TYPE);
    push_string(&mut block, public);
    block
}

fn private_block(seed: &[u8; 32], public: &[u8; 32]) -> Vec<u8> {
    let mut block = Vec::with_capacity(160);
    block.extend_from_slice(&CHECK_WORD.to_be_bytes());
    block.extend_from_slice(&CHECK_WORD.to_be_bytes());
    push_string(&mut block, KEY_TYPE);
    push_string(&mut block, public);
    let mut secret = [0u8; 64];
    secret[..32].copy_from_slice(seed);
    secret[32..].copy_from_slice(public);
    push_string(&mut block, &secret);
    push_string(&mut block, b"");
    for index in 1..=(PAD_BLOCK - block.len() % PAD_BLOCK) % PAD_BLOCK {
        block.push(index);
    }
    block
}

fn push_string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

/// The base64 body between the PEM fences, with the line breaks removed.
fn decode_pem(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .flat_map(|line| line.chars())
        .filter(|character| !character.is_whitespace())
        .collect();
    if body.is_empty() {
        return Err(format!("it has no {PEM_BEGIN} body"));
    }
    BASE64_STANDARD
        .decode(body.as_bytes())
        .map_err(|error| format!("its base64 body does not decode: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        CHECK_WORD, OPENSSH_MAGIC, PEM_BEGIN, PEM_END, decode_pem, encode_openssh_ed25519,
        parse_openssh_ed25519,
    };
    use base64::prelude::{BASE64_STANDARD, Engine as _};

    /// A key file v2 itself wrote, byte for byte: the output of
    /// `loadWorkerKey` from `apps/worker/src/host/jwt.ts` under Bun, whose seed
    /// and public key are the two constants below. An install that already has
    /// one of these has to keep working, and a key this writes has to be
    /// readable by every `ssh-keygen` and every `authorized_keys` row built
    /// from the last one.
    const V2_KEY_FILE: &str = "\
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCrGuvYPZjMy0bZD93XV8nxNJcw65prI2uuYpv5SPkdKwAAAIgSNFZ4EjRW
eAAAAAtzc2gtZWQyNTUxOQAAACCrGuvYPZjMy0bZD93XV8nxNJcw65prI2uuYpv5SPkdKw
AAAECeMuWaUJzYUVJGUWlHAwO40t2oE+TyGDWIAQiKQK0Fdqsa69g9mMzLRtkP3ddXyfE0
lzDrmmsja65im/lI+R0rAAAAAAECAwQF
-----END OPENSSH PRIVATE KEY-----
";
    const V2_SEED_B64: &str = "njLlmlCc2FFSRlFpRwMDuNLdqBPk8hg1iAEIikCtBXY=";
    const V2_PUBLIC_B64: &str = "qxrr2D2YzMtG2Q/d11fJ8TSXMOuaayNrrmKb+Uj5HSs=";

    fn v2_parts() -> ([u8; 32], [u8; 32]) {
        let decode = |text: &str| -> [u8; 32] {
            BASE64_STANDARD
                .decode(text)
                .expect("a 32-byte key half in base64")
                .try_into()
                .expect("32 bytes")
        };
        (decode(V2_SEED_B64), decode(V2_PUBLIC_B64))
    }

    #[test]
    fn a_key_written_here_is_the_file_v2_wrote() {
        let (seed, public) = v2_parts();
        assert_eq!(
            encode_openssh_ed25519(&seed, &public),
            V2_KEY_FILE,
            "the check word, the empty comment and the padding are the only \
             places this format can drift, and every one of them makes ssh \
             refuse the file"
        );
    }

    #[test]
    fn a_key_v2_wrote_is_readable_here() {
        assert_eq!(
            parse_openssh_ed25519(V2_KEY_FILE).expect("a v2 key file parses"),
            v2_parts(),
            "an install that already has one of these files has to keep dialling \
             as the machine the coordinator enrolled"
        );
    }

    #[test]
    fn a_damaged_body_is_refused_rather_than_signed_with() {
        // v2 located the seed by scanning for the public key appearing twice,
        // which cannot tell these three from a healthy key. Each is a file a
        // worker would have signed a token from.
        let (seed, public) = v2_parts();
        let mut damaged = decode_pem(V2_KEY_FILE).expect("the fixture decodes");
        let integrity = damaged
            .windows(4)
            .position(|window| window == CHECK_WORD.to_be_bytes())
            .expect("the first integrity word");
        damaged[integrity + 1] ^= 0x01;
        assert!(
            parse_openssh_ed25519(&damaged_to_pem(&damaged)).is_err(),
            "a body whose two integrity words disagree is damaged, and a \
             damaged key is a machine nobody has enrolled"
        );

        // A passphrase-protected key, spliced into the fixture rather than
        // corrupted: the body is a length-prefixed stream, so swapping the
        // cipher name for a real one produces a document that is well formed
        // at every offset and whose private block is ciphertext.
        let body = decode_pem(V2_KEY_FILE).expect("the fixture decodes");
        let cipher = b"aes256-ctr";
        let mut encrypted = Vec::with_capacity(body.len() + cipher.len());
        encrypted.extend_from_slice(&body[..OPENSSH_MAGIC.len()]);
        encrypted.extend_from_slice(&(cipher.len() as u32).to_be_bytes());
        encrypted.extend_from_slice(cipher);
        encrypted.extend_from_slice(&body[OPENSSH_MAGIC.len() + 4 + 4..]);
        let refused = parse_openssh_ed25519(&damaged_to_pem(&encrypted))
            .expect_err("a passphrase-protected key is not a signing key");
        assert!(
            refused.contains("aes256-ctr"),
            "the refusal names the cipher it found, because an operator holding \
             an encrypted key needs to be told to remove the passphrase: {refused}"
        );
        assert!(parse_openssh_ed25519(&encode_openssh_ed25519(&seed, &public)).is_ok());
    }

    fn damaged_to_pem(body: &[u8]) -> String {
        format!("{PEM_BEGIN}\n{}\n{PEM_END}\n", BASE64_STANDARD.encode(body))
    }
}
