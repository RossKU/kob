//! The operator's hot key: the only key a matcher or keeper holds (P2PK Schnorr).
//!
//! Key handling: never on the command line. The secret is read from
//!
//! 1. `$KOB_OPERATOR_KEY_FILE` or `--key-file PATH` (a path, not the secret), or
//! 2. `$CREDENTIALS_DIRECTORY/kob-operator-key` (systemd `LoadCredential=`, tmpfs), or
//! 3. `$KOB_OPERATOR_KEY` (hex; removed from the process environment once read).
//!
//! A key file holds 64 hex characters. On Unix it must not be readable or writable by group or
//! others (mode `0600` / `0400`), or loading fails. On Windows the file's ACL is read (`icacls /save`, SDDL) and a warning names any
//! account group (Everyone, Users, Authenticated Users, ...) that may read it; `KOB_KEY_ACL_STRICT=1` turns the warning into an error.
//! The secret is wiped from memory on drop. Keep
//! only a few days of operating funds on the key (a balance alert is part of monitoring).

use std::path::{Path, PathBuf};

use kaspa_addresses::{Address, Prefix, Version};
use kob_protocol::script::p2pk_spk;
use kob_protocol::tx::{pubkey_of, sign_digest, sign_locally, BuiltTx, InputSignature};

/// Environment variable naming the key file.
pub const KEY_FILE_ENV: &str = "KOB_OPERATOR_KEY_FILE";
/// Environment variable holding the hex secret (discouraged; removed after reading).
pub const KEY_ENV: &str = "KOB_OPERATOR_KEY";
/// Credential name under `$CREDENTIALS_DIRECTORY` (systemd `LoadCredential=kob-operator-key:...`).
pub const CREDENTIAL_NAME: &str = "kob-operator-key";

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("no operator key: set {KEY_FILE_ENV}, --key-file, a systemd credential `{CREDENTIAL_NAME}`, or {KEY_ENV}")]
    Missing,
    #[error("key file {0}: {1}")]
    File(PathBuf, String),
    #[error("key file {0} is accessible by group or others (mode {1:o}); chmod 600 it")]
    Permissions(PathBuf, u32),
    #[error("key file {0} is readable by other accounts ({1}); restrict its ACL (icacls <file> /inheritance:r /grant:r <you>:F)")]
    Acl(PathBuf, String),
    #[error("operator key: {0}")]
    Invalid(String),
}

/// Signs builder requests with local keys.
pub trait Signer: Send + Sync {
    /// The operator's x-only public key (change, keeper tips, token inventory).
    fn pubkey(&self) -> [u8; 32];
    /// Signs every request of `built` this signer holds a key for.
    fn sign(&self, built: &BuiltTx) -> Result<Vec<InputSignature>, String>;
}

/// The hot key.
pub struct HotKey {
    secret: [u8; 32],
    pubkey: [u8; 32],
}

/// Overwrites `buf` with zeros in a way the optimiser may not remove.
pub fn wipe(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: a valid, aligned, exclusively borrowed byte.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

impl Drop for HotKey {
    fn drop(&mut self) {
        wipe(&mut self.secret);
    }
}

impl std::fmt::Debug for HotKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HotKey({})", kob_protocol::json::to_hex(&self.pubkey))
    }
}

impl HotKey {
    /// From a 32-byte secret.
    pub fn from_secret(secret: [u8; 32]) -> Result<HotKey, KeyError> {
        let pubkey = pubkey_of(&secret).map_err(|e| KeyError::Invalid(e.to_string()))?;
        Ok(HotKey { secret, pubkey })
    }

    /// From 64 hex characters (surrounding whitespace ignored).
    pub fn from_hex(s: &str) -> Result<HotKey, KeyError> {
        let b = kob_protocol::json::hex32(s.trim()).map_err(KeyError::Invalid)?;
        HotKey::from_secret(b)
    }

    /// Reads a key file, refusing group- or world-accessible files on Unix.
    pub fn from_file(path: &Path) -> Result<HotKey, KeyError> {
        check_permissions(path)?;
        // the hex text is wiped after parsing: it is the secret in another spelling
        let mut buf = std::fs::read(path).map_err(|e| KeyError::File(path.to_path_buf(), e.to_string()))?;
        let r = match std::str::from_utf8(&buf) {
            Ok(s) => HotKey::from_hex(s),
            Err(_) => Err(KeyError::Invalid("the key file is not text".into())),
        };
        wipe(&mut buf);
        r
    }

    /// Loads the key from the sources listed in the module documentation, in that order.
    pub fn load(key_file: Option<&Path>) -> Result<HotKey, KeyError> {
        if let Some(p) = key_file {
            return HotKey::from_file(p);
        }
        if let Some(p) = std::env::var_os(KEY_FILE_ENV) {
            return HotKey::from_file(Path::new(&p));
        }
        if let Some(dir) = std::env::var_os("CREDENTIALS_DIRECTORY") {
            let p = Path::new(&dir).join(CREDENTIAL_NAME);
            if p.exists() {
                return HotKey::from_file(&p);
            }
        }
        if let Ok(s) = std::env::var(KEY_ENV) {
            std::env::remove_var(KEY_ENV);
            let r = HotKey::from_hex(&s);
            // `remove_var` only unlinks the variable; the copy this process made is wiped here. Prefer a key file or a
            // systemd credential (the operator guide says so): the environment block of the process is not scrubbable.
            let mut b = s.into_bytes();
            wipe(&mut b);
            return r;
        }
        Err(KeyError::Missing)
    }

    /// Kaspa address of the key on a network (`mainnet`, `testnet-10`, `devnet`, `simnet`).
    pub fn address(&self, network: &str) -> Address {
        address_of(&self.pubkey, network)
    }
}

/// P2PK address of an x-only key.
pub fn address_of(pubkey: &[u8; 32], network: &str) -> Address {
    let prefix = if network.starts_with("mainnet") {
        Prefix::Mainnet
    } else if network.starts_with("devnet") {
        Prefix::Devnet
    } else if network.starts_with("simnet") {
        Prefix::Simnet
    } else {
        Prefix::Testnet
    };
    Address::new(prefix, Version::PubKey, pubkey)
}

/// Script public key string (`version ‖ script` hex) of the operator's P2PK outputs.
pub fn p2pk_spk_string(pubkey: &[u8; 32]) -> String {
    kob_protocol::tx::spk_to_string(&p2pk_spk(pubkey))
}

#[cfg(unix)]
fn check_permissions(path: &Path) -> Result<(), KeyError> {
    use std::os::unix::fs::PermissionsExt;
    let m = std::fs::metadata(path).map_err(|e| KeyError::File(path.to_path_buf(), e.to_string()))?;
    let mode = m.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(KeyError::Permissions(path.to_path_buf(), mode));
    }
    Ok(())
}

/// Set to any value other than `0` to make a key file readable by other accounts a hard error on Windows (default: a warning).
pub const STRICT_ACL_ENV: &str = "KOB_KEY_ACL_STRICT";

/// Trustees of an SDDL ACE that stand for "other accounts": Everyone, Users, Authenticated Users, Interactive, Network, Anonymous, Guests
/// (aliases and the SIDs they abbreviate). The owner, SYSTEM and Administrators are not "others".
const OTHER_TRUSTEES: &[&str] = &[
    "WD",
    "BU",
    "AU",
    "IU",
    "NU",
    "AN",
    "BG",
    "LG",
    "S-1-1-0",
    "S-1-5-32-545",
    "S-1-5-11",
    "S-1-5-4",
    "S-1-5-2",
    "S-1-5-7",
    "S-1-5-32-546",
];

/// Whether an SDDL access-rights field grants reading the file's data: an alias (`FA` file all, `FR` file read, `GA` generic all, `GR` generic
/// read) or a hexadecimal mask with FILE_READ_DATA (0x1), GENERIC_READ (0x80000000) or GENERIC_ALL (0x10000000). An unparsable mask counts
/// as read (fail towards the warning).
fn grants_read(rights: &str) -> bool {
    if let Some(hex) = rights.strip_prefix("0x").or_else(|| rights.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).map(|m| m & (0x1 | 0x8000_0000 | 0x1000_0000) != 0).unwrap_or(true);
    }
    // two-letter aliases, concatenated
    rights.as_bytes().chunks(2).any(|c| matches!(c, b"FA" | b"FR" | b"GA" | b"GR"))
}

/// The other accounts an SDDL DACL lets read the file: the trustees of every access-ALLOWED ACE (`A`) that grants read
/// access and names Everyone / Users / Authenticated Users and the like. Deny ACEs and the owner / SYSTEM / Administrators are ignored.
/// Pure text: the platform-specific part only fetches the SDDL.
pub fn sddl_readable_by_others(sddl: &str) -> Vec<String> {
    let Some(dacl) = sddl.find("D:").map(|i| &sddl[i + 2..]) else { return vec![] };
    let dacl = dacl.split("S:").next().unwrap_or(dacl); // drop a SACL
    let mut out = vec![];
    for ace in dacl.split('(').skip(1) {
        let ace = ace.split(')').next().unwrap_or("");
        let f: Vec<&str> = ace.split(';').collect();
        // type;flags;rights;object;inherit-object;trustee
        if f.len() < 6 || f[0] != "A" {
            continue;
        }
        let trustee = f[5].trim();
        if OTHER_TRUSTEES.contains(&trustee) && grants_read(f[2]) {
            out.push(trustee.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(not(unix))]
fn check_permissions(path: &Path) -> Result<(), KeyError> {
    std::fs::metadata(path).map_err(|e| KeyError::File(path.to_path_buf(), e.to_string()))?;
    match windows_acl_sddl(path) {
        Some(sddl) => {
            let others = sddl_readable_by_others(&sddl);
            if others.is_empty() {
                return Ok(());
            }
            let strict = std::env::var(STRICT_ACL_ENV).map(|v| v != "0" && !v.is_empty()).unwrap_or(false);
            if strict {
                return Err(KeyError::Acl(path.to_path_buf(), others.join(", ")));
            }
            tracing::warn!(
                path = %path.display(),
                readable_by = %others.join(", "),
                "operator key file is readable by other accounts; restrict it (icacls <file> /inheritance:r /grant:r <you>:F) or set KOB_KEY_ACL_STRICT=1 to refuse"
            );
            Ok(())
        }
        None => {
            tracing::warn!(path = %path.display(), "could not read the ACL of the operator key file (icacls unavailable); check it by hand");
            Ok(())
        }
    }
}

/// The SDDL of a file, from `icacls <file> /save <tmp>` (SDDL is locale-independent, unlike icacls' account names).
#[cfg(not(unix))]
fn windows_acl_sddl(path: &Path) -> Option<String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("kob-acl-{}-{nanos}.txt", std::process::id()));
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name()?;
    // `icacls` saves paths relative to the working directory of the call, so run it in the file's directory
    let out = std::process::Command::new("icacls").current_dir(dir).arg(name).arg("/save").arg(&tmp).output().ok()?;
    let saved = if out.status.success() { std::fs::read(&tmp).ok() } else { None };
    let _ = std::fs::remove_file(&tmp);
    let bytes = saved?;
    // saved as UTF-16LE by icacls (with or without a BOM, depending on the Windows version); plain text is accepted too
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || (bytes.len() >= 2 && bytes[1] == 0);
    let text = if utf16 {
        let body = if bytes.starts_with(&[0xFF, 0xFE]) { &bytes[2..] } else { &bytes[..] };
        let u: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    text.lines().map(str::trim).find(|l| l.starts_with("D:")).map(str::to_string)
}

impl Signer for HotKey {
    fn pubkey(&self) -> [u8; 32] {
        self.pubkey
    }
    fn sign(&self, built: &BuiltTx) -> Result<Vec<InputSignature>, String> {
        // Signs digest by digest (no copy of the secret in a temporary key map); a request for any
        // other key is an error: the operator only ever signs its own funding.
        built
            .sign
            .iter()
            .map(|r| {
                if r.pubkey != self.pubkey {
                    return Err(format!("input {} needs a signature by {}", r.input_index, kob_protocol::json::to_hex(&r.pubkey)));
                }
                let signature = sign_digest(&self.secret, &r.sighash).map_err(|e| e.to_string())?;
                Ok(InputSignature { input_index: r.input_index, signature })
            })
            .collect()
    }
}

/// Several local keys (tests: takers and makers signing next to the operator).
pub struct LocalKeys {
    pub operator: [u8; 32],
    pub keys: std::collections::BTreeMap<[u8; 32], [u8; 32]>,
}

impl Signer for LocalKeys {
    fn pubkey(&self) -> [u8; 32] {
        self.operator
    }
    fn sign(&self, built: &BuiltTx) -> Result<Vec<InputSignature>, String> {
        sign_locally(built, &self.keys).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_sources() {
        let k = HotKey::from_hex(&"11".repeat(32)).unwrap();
        assert_eq!(k.pubkey(), pubkey_of(&[0x11; 32]).unwrap());
        assert!(format!("{k:?}").starts_with("HotKey("));
        assert!(!format!("{k:?}").contains(&"11".repeat(32)));
        assert!(HotKey::from_hex("zz").is_err());
        let a = k.address("testnet-10").to_string();
        assert!(a.starts_with("kaspatest:"), "{a}");
        assert!(k.address("mainnet").to_string().starts_with("kaspa:"));

        let dir = std::env::temp_dir().join(format!("kob-key-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("key");
        std::fs::write(&p, format!("{}\n", "22".repeat(32))).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(matches!(HotKey::from_file(&p), Err(KeyError::Permissions(..))));
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(HotKey::load(Some(&p)).unwrap().pubkey(), pubkey_of(&[0x22; 32]).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sddl_names_the_other_accounts_that_can_read() {
        // owner + SYSTEM + Administrators only: fine
        assert!(sddl_readable_by_others("D:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;S-1-5-21-1-2-3-1001)").is_empty());
        // a default user-profile ACL: Users have read/execute inherited from the folder
        let d = "O:S-1-5-21-1-2-3-1001G:S-1-5-21-1-2-3-513D:AI(A;ID;FA;;;BA)(A;ID;FA;;;SY)(A;ID;0x1200a9;;;BU)(A;ID;FA;;;S-1-5-21-1-2-3-1001)";
        assert_eq!(sddl_readable_by_others(d), vec!["BU".to_string()]);
        assert_eq!(sddl_readable_by_others("D:(A;;FR;;;WD)(A;;FA;;;AU)(A;;GR;;;WD)"), vec!["AU".to_string(), "WD".to_string()]);
        assert_eq!(sddl_readable_by_others("D:(A;;GA;;;S-1-1-0)"), vec!["S-1-1-0".to_string()]);
        // no read bit (write attributes / synchronise only), a deny, a non-DACL section, a SACL
        assert!(sddl_readable_by_others("D:(A;;0x100100;;;WD)").is_empty());
        assert!(sddl_readable_by_others("D:(D;;FA;;;WD)").is_empty());
        assert!(sddl_readable_by_others("O:WDG:WD").is_empty());
        assert!(sddl_readable_by_others("D:(A;;FA;;;SY)S:(A;;FR;;;WD)").is_empty());
        assert!(sddl_readable_by_others("").is_empty());
        // an unparsable mask counts as read (fail towards the warning)
        assert_eq!(sddl_readable_by_others("D:(A;;0xZZ;;;WD)"), vec!["WD".to_string()]);
    }

    /// The Windows check against the real ACL of a real file: after `icacls /inheritance:r /grant:r <me>:F` nobody else may read it;
    /// granting Everyone read is reported (a warning by default, an error with `KOB_KEY_ACL_STRICT`).
    #[cfg(windows)]
    #[test]
    fn windows_acl_of_a_real_file() {
        let dir = std::env::temp_dir().join(format!("kob-acl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("key");
        std::fs::write(&p, format!("{}\n", "33".repeat(32))).unwrap();
        let user = std::env::var("USERNAME").unwrap_or_default();
        let ok = std::process::Command::new("icacls")
            .arg(&p)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{user}:F"))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            let sddl = windows_acl_sddl(&p).expect("the ACL is readable");
            assert!(sddl_readable_by_others(&sddl).is_empty(), "{sddl}");
            assert_eq!(HotKey::from_file(&p).unwrap().pubkey(), pubkey_of(&[0x33; 32]).unwrap());
            // Everyone may read: reported, the key still loads (warning)
            assert!(std::process::Command::new("icacls").arg(&p).args(["/grant", "*S-1-1-0:R"]).output().unwrap().status.success());
            let sddl = windows_acl_sddl(&p).expect("the ACL is readable");
            assert_eq!(sddl_readable_by_others(&sddl), vec!["WD".to_string()], "{sddl}");
            assert!(HotKey::from_file(&p).is_ok());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wipe_zeroes_the_buffer() {
        let mut b = b"00112233".to_vec();
        wipe(&mut b);
        assert!(b.iter().all(|x| *x == 0));
    }
}
