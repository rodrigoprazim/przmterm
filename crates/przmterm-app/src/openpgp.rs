//! Just enough OpenPGP to check a release: the Ed25519 public key in
//! `docs/release-key.asc` and a detached signature made with it (`gpg
//! --armor --detach-sign`, as the release workflow does). Version 4 packets,
//! the EdDSA algorithm (22) with SHA-256 or SHA-512, binary signatures;
//! anything else is refused rather than guessed.

use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256, Sha512};

/// The key releases are signed with.
pub const RELEASE_KEY: &str = include_str!("../../../docs/release-key.asc");

/// The OID of Ed25519 in OpenPGP's legacy EdDSA keys.
const ED25519_OID: [u8; 9] = [0x2B, 0x06, 0x01, 0x04, 0x01, 0xDA, 0x47, 0x0F, 0x01];
const EDDSA: u8 = 22;

/// Checks that `signature` (ASCII-armoured) is a valid signature of `data`
/// by the primary key in `key` (ASCII-armoured).
pub fn verify(key: &str, data: &[u8], signature: &str) -> Result<(), String> {
    let key = public_key(&dearmor(key)?)?;
    let packets = dearmor(signature)?;
    let (tag, body) = packets_of(&packets)?
        .into_iter()
        .next()
        .ok_or("the signature is empty")?;
    if tag != 2 {
        return Err("not a signature".into());
    }
    check_signature(&key, data, body)
}

/// The bytes inside `-----BEGIN PGP …-----` / `-----END PGP …-----`.
fn dearmor(text: &str) -> Result<Vec<u8>, String> {
    let mut lines = text.lines().map(str::trim);
    lines
        .by_ref()
        .find(|l| l.starts_with("-----BEGIN PGP "))
        .ok_or("no OpenPGP armour")?;
    // Header lines ("Comment: …") end at the first empty line.
    let mut base64 = String::new();
    let mut in_headers = true;
    for line in lines {
        if line.starts_with("-----END PGP ") {
            return base64::engine::general_purpose::STANDARD
                .decode(base64.as_bytes())
                .map_err(|e| format!("bad armour: {e}"));
        }
        if in_headers {
            if line.is_empty() {
                in_headers = false;
            } else if !line.contains(": ") {
                // No headers and no empty line: this is already data.
                in_headers = false;
                base64.push_str(line);
            }
            continue;
        }
        // The checksum line.
        if line.starts_with('=') {
            continue;
        }
        base64.push_str(line);
    }
    Err("the armour does not end".into())
}

/// Splits `bytes` into (tag, body) packets.
fn packets_of(mut bytes: &[u8]) -> Result<Vec<(u8, &[u8])>, String> {
    let mut packets = Vec::new();
    while let Some((&ctb, rest)) = bytes.split_first() {
        if ctb & 0x80 == 0 {
            return Err("not an OpenPGP packet".into());
        }
        let (tag, len, rest) = if ctb & 0x40 == 0 {
            // Old format: the tag in bits 2-5, the length's size in 0-1.
            let size = match ctb & 3 {
                0 => 1,
                1 => 2,
                2 => 4,
                _ => return Err("packet of unknown length".into()),
            };
            let (len, rest) = rest.split_at_checked(size).ok_or("truncated packet")?;
            let len = len.iter().fold(0usize, |n, b| (n << 8) | *b as usize);
            ((ctb >> 2) & 0x0F, len, rest)
        } else {
            let (&first, rest) = rest.split_first().ok_or("truncated packet")?;
            let (len, rest) = match first {
                0..=191 => (first as usize, rest),
                192..=223 => {
                    let (&second, rest) = rest.split_first().ok_or("truncated packet")?;
                    (
                        (((first as usize) - 192) << 8) + second as usize + 192,
                        rest,
                    )
                }
                255 => {
                    let (len, rest) = rest.split_at_checked(4).ok_or("truncated packet")?;
                    (len.iter().fold(0usize, |n, b| (n << 8) | *b as usize), rest)
                }
                _ => return Err("packets in parts are not supported".into()),
            };
            (ctb & 0x3F, len, rest)
        };
        let (body, rest) = rest.split_at_checked(len).ok_or("truncated packet")?;
        packets.push((tag, body));
        bytes = rest;
    }
    Ok(packets)
}

/// Reads a multiprecision integer: its length in bits, then the bytes.
fn mpi(bytes: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let (bits, rest) = bytes.split_at_checked(2).ok_or("truncated number")?;
    let len = (u16::from_be_bytes([bits[0], bits[1]]) as usize).div_ceil(8);
    rest.split_at_checked(len)
        .ok_or_else(|| "truncated number".into())
}

/// The primary key of an armoured public key block.
fn public_key(bytes: &[u8]) -> Result<VerifyingKey, String> {
    let (tag, body) = packets_of(bytes)?
        .into_iter()
        .next()
        .ok_or("the key is empty")?;
    if tag != 6 {
        return Err("not a public key".into());
    }
    // Version 4, creation time, algorithm, curve, point.
    if body.first() != Some(&4) || body.get(5) != Some(&EDDSA) {
        return Err("the key is not a version 4 EdDSA key".into());
    }
    let oid_len = *body.get(6).ok_or("truncated key")? as usize;
    let oid = body.get(7..7 + oid_len).ok_or("truncated key")?;
    if oid != ED25519_OID {
        return Err("the key is not on Ed25519".into());
    }
    let (point, _) = mpi(&body[7 + oid_len..])?;
    // 0x40, then the 32 bytes of the point.
    let point: [u8; 32] = match point {
        [0x40, rest @ ..] => rest.try_into().map_err(|_| "bad Ed25519 point")?,
        _ => return Err("bad Ed25519 point".into()),
    };
    VerifyingKey::from_bytes(&point).map_err(|e| e.to_string())
}

fn check_signature(key: &VerifyingKey, data: &[u8], body: &[u8]) -> Result<(), String> {
    // Version, type, key algorithm, hash algorithm, hashed subpackets.
    let [4, kind, algorithm, hash, ..] = *body else {
        return Err("not a version 4 signature".into());
    };
    if kind != 0x00 {
        return Err("not a signature of a binary file".into());
    }
    if algorithm != EDDSA {
        return Err("not an EdDSA signature".into());
    }
    let hashed_len = u16::from_be_bytes([
        *body.get(4).ok_or("truncated signature")?,
        *body.get(5).ok_or("truncated signature")?,
    ]) as usize;
    let hashed_end = 6 + hashed_len;
    let trailer_part = body.get(..hashed_end).ok_or("truncated signature")?;
    let rest = &body[hashed_end..];
    let (unhashed_len, rest) = rest.split_at_checked(2).ok_or("truncated signature")?;
    let unhashed_len = u16::from_be_bytes([unhashed_len[0], unhashed_len[1]]) as usize;
    let rest = rest.get(unhashed_len..).ok_or("truncated signature")?;
    let (left16, rest) = rest.split_at_checked(2).ok_or("truncated signature")?;
    let (r, rest) = mpi(rest)?;
    let (s, _) = mpi(rest)?;

    // The data, then the signature's own hashed part and its length.
    let mut trailer = trailer_part.to_vec();
    trailer.extend_from_slice(&[0x04, 0xFF]);
    trailer.extend_from_slice(&(hashed_end as u32).to_be_bytes());
    let digest: Vec<u8> = match hash {
        8 => Sha256::new()
            .chain_update(data)
            .chain_update(&trailer)
            .finalize()
            .to_vec(),
        10 => Sha512::new()
            .chain_update(data)
            .chain_update(&trailer)
            .finalize()
            .to_vec(),
        _ => return Err("the signature uses an unsupported hash".into()),
    };
    if digest[..2] != *left16 {
        return Err("the signature does not match the file".into());
    }
    let mut raw = [0u8; 64];
    for (part, at) in [(r, 0), (s, 32)] {
        if part.len() > 32 {
            return Err("bad signature numbers".into());
        }
        raw[at + 32 - part.len()..at + 32].copy_from_slice(part);
    }
    key.verify_strict(&digest, &Signature::from_bytes(&raw))
        .map_err(|_| "the signature does not match the file".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &[u8] = include_bytes!("../testdata/rc1-SHA256SUMS.txt");
    const SIG: &str = include_str!("../testdata/rc1-SHA256SUMS.txt.asc");

    #[test]
    fn checks_a_real_release() {
        assert_eq!(verify(RELEASE_KEY, SUMS, SIG), Ok(()));
    }

    #[test]
    fn refuses_changed_files_and_other_keys() {
        let mut changed = SUMS.to_vec();
        changed[0] ^= 1;
        assert!(verify(RELEASE_KEY, &changed, SIG).is_err());
        assert!(verify(RELEASE_KEY, b"", SIG).is_err());
        // A signature that is not the release key's: one bit of the
        // numbers flipped.
        let packets = dearmor(SIG).unwrap();
        let mut bent = packets.clone();
        let last = bent.len() - 5;
        bent[last] ^= 0x10;
        let armour = format!(
            "-----BEGIN PGP SIGNATURE-----\n\n{}\n-----END PGP SIGNATURE-----\n",
            base64::engine::general_purpose::STANDARD.encode(&bent)
        );
        assert!(verify(RELEASE_KEY, SUMS, &armour).is_err());
        assert!(verify(RELEASE_KEY, SUMS, "nonsense").is_err());
        assert!(verify(SIG, SUMS, SIG).is_err(), "a signature is not a key");
    }
}
