//! Script helpers: push-only parsing of signature scripts, P2SH derivation and addresses.

use kaspa_addresses::Prefix;
use kaspa_consensus_core::tx::ScriptPublicKey;

/// Parse a push-only script into its data items. Small-integer opcodes yield their value as one
/// byte (`OP_0` is the empty item). Returns `None` on any non-push opcode or truncated push.
pub fn parse_pushes(script: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < script.len() {
        let op = script[i];
        i += 1;
        let len = match op {
            0x00 => {
                out.push(Vec::new());
                continue;
            }
            0x01..=0x4b => op as usize,
            0x4c => {
                let l = *script.get(i)? as usize;
                i += 1;
                l
            }
            0x4d => {
                let b = script.get(i..i + 2)?;
                i += 2;
                u16::from_le_bytes([b[0], b[1]]) as usize
            }
            0x4e => {
                let b = script.get(i..i + 4)?;
                i += 4;
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
            }
            0x4f => {
                out.push(vec![0x81]);
                continue;
            }
            0x51..=0x60 => {
                out.push(vec![op - 0x50]);
                continue;
            }
            _ => return None,
        };
        let end = i.checked_add(len)?;
        out.push(script.get(i..end)?.to_vec());
        i = end;
    }
    Some(out)
}

/// The P2SH script public key of `redeem` as the node prints it: version (2 bytes, big endian)
/// followed by the script.
pub fn p2sh_spk(redeem: &[u8]) -> Vec<u8> {
    let spk = kaspa_txscript::pay_to_script_hash_script(redeem);
    spk_bytes(&spk)
}

pub fn spk_bytes(spk: &ScriptPublicKey) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + spk.script().len());
    v.extend_from_slice(&spk.version().to_be_bytes());
    v.extend_from_slice(spk.script());
    v
}

pub fn parse_spk(bytes: &[u8]) -> Option<ScriptPublicKey> {
    if bytes.len() < 2 {
        return None;
    }
    let version = u16::from_be_bytes([bytes[0], bytes[1]]);
    Some(ScriptPublicKey::from_vec(version, bytes[2..].to_vec()))
}

/// Address prefix for a node network id (`mainnet`, `testnet-10`, `devnet`, `simnet`).
pub fn prefix_for_network(network: &str) -> Prefix {
    if network.starts_with("mainnet") {
        Prefix::Mainnet
    } else if network.starts_with("testnet") {
        Prefix::Testnet
    } else if network.starts_with("simnet") {
        Prefix::Simnet
    } else {
        Prefix::Devnet
    }
}

/// Address of a script public key (version + script bytes), if it has one.
pub fn spk_address(spk: &[u8], network: &str) -> Option<String> {
    let spk = parse_spk(spk)?;
    kaspa_txscript::extract_script_pub_key_address(&spk, prefix_for_network(network)).ok().map(|a| a.to_string())
}

/// Decode a script-integer of up to 8 bytes (little endian, sign-magnitude).
pub fn script_int(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return Some(0);
    }
    if bytes.len() > 8 {
        return None;
    }
    let mut v: i128 = 0;
    for (i, b) in bytes.iter().enumerate() {
        let b = if i == bytes.len() - 1 { b & 0x7f } else { *b };
        v |= (b as i128) << (8 * i);
    }
    if bytes[bytes.len() - 1] & 0x80 != 0 {
        v = -v;
    }
    i64::try_from(v).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes() {
        let mut s = vec![0x08];
        s.extend_from_slice(&5i64.to_le_bytes());
        s.extend_from_slice(&[0x00, 0x51, 0x4f]);
        s.extend_from_slice(&[0x4c, 0x02, 0xaa, 0xbb]);
        s.extend_from_slice(&[0x4d, 0x01, 0x00, 0xcc]);
        let p = parse_pushes(&s).unwrap();
        assert_eq!(p.len(), 6);
        assert_eq!(script_int(&p[0]), Some(5));
        assert!(p[1].is_empty());
        assert_eq!(p[2], vec![1]);
        assert_eq!(p[3], vec![0x81]);
        assert_eq!(p[4], vec![0xaa, 0xbb]);
        assert_eq!(p[5], vec![0xcc]);
        assert!(parse_pushes(&[0x05, 1, 2]).is_none());
        assert!(parse_pushes(&[0xac]).is_none());
    }

    #[test]
    fn p2sh_shape_and_address() {
        let spk = p2sh_spk(&[1, 2, 3]);
        assert_eq!(&spk[..2], &[0, 0]);
        assert_eq!(spk[2], 0xaa);
        assert_eq!(spk[3], 0x20);
        assert_eq!(spk.len(), 2 + 1 + 1 + 32 + 1);
        assert_eq!(*spk.last().unwrap(), 0x87);
        let a = spk_address(&spk, "testnet-10").unwrap();
        assert!(a.starts_with("kaspatest:"), "{a}");
        assert!(spk_address(&spk, "mainnet").unwrap().starts_with("kaspa:"));
    }

    #[test]
    fn negative_ints() {
        assert_eq!(script_int(&[0x81]), Some(-1));
        assert_eq!(script_int(&[0xe8, 0x03]), Some(1000));
        assert_eq!(script_int(&[]), Some(0));
    }
}
