//! The operator's hot key: the only key a matcher or keeper holds (P2PK Schnorr).
//!
//! # Key handling
//!
//! The key is never on the command line. The secret is read from the first of these that is set:
//!
//! 1. `$KOB_OPERATOR_KEY_FILE` or `--key-file PATH` (a path, not the secret);
//! 2. `$CREDENTIALS_DIRECTORY/kob-operator-key` (systemd `LoadCredential=`, tmpfs);
//! 3. `$KOB_OPERATOR_KEY` (hex; removed from the process environment once read).
//!
//! A key file holds 64 hex characters. Loading fails when another account can access the file.
//!
//! - On Unix the file must not be readable or writable by group or others (mode `0600` / `0400`).
//! - On Windows the file's ACL is read (`icacls /save`, SDDL). Loading fails when any other account
//!   may read it: a group such as Everyone or Users, or a named account. Only the account running
//!   kobd and the privileged principals (SYSTEM, Administrators, ...) are exempt. Loading also
//!   fails when the ACL cannot be read. `KOB_KEY_ACL_STRICT=0` turns the refusal into a warning.
//!
//! The secret is wiped from memory on drop. Keep only a few days of operating funds on the key (a
//! balance alert is part of monitoring).

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

/// Why the operator key could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// No key source is configured.
    #[error("no operator key: give --key-file PATH (or {KEY_FILE_ENV}), a systemd credential `{CREDENTIAL_NAME}`, or run `kobd setup`, which makes one")]
    Missing,
    /// The key file could not be read or written: the path and the reason.
    #[error("key file {0}: {1}")]
    File(PathBuf, String),
    /// The key file is accessible by group or others (Unix): the path and its mode.
    #[error("key file {0} is accessible by group or others (mode {1:o}); chmod 600 it")]
    Permissions(PathBuf, u32),
    /// The file, who else may read it (by name), and the command that restricts it.
    #[error(
        "key file {0} is readable by other accounts ({1}); restrict its ACL with `{2}`, or set KOB_KEY_ACL_STRICT=0 to start anyway (not recommended)"
    )]
    Acl(PathBuf, String, String),
    /// The key text is not a valid key.
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

    /// Reads a key file, refusing a file that other accounts may access (Unix: group or others;
    /// Windows: see [`STRICT_ACL_ENV`]).
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
            // `remove_var` only unlinks the variable. The copy this process made is wiped here.
            // Prefer a key file or a systemd credential (the operator guide says so), since the
            // environment block of the process is not scrubbable.
            let mut b = s.into_bytes();
            wipe(&mut b);
            return r;
        }
        Err(KeyError::Missing)
    }

    /// A fresh key from the operating system's CSPRNG, written to a NEW key file at `path` that
    /// only the current account may read (`kobd setup`).
    ///
    /// The file is created empty and restricted before the secret is written, so the secret is
    /// never in a file another account could read, not even briefly.
    ///
    /// - Unix: mode `0600` at creation.
    /// - Windows: the inherited ACL is replaced by one entry for the current account (`icacls
    ///   /inheritance:r /grant:r`). The result is checked with the same SDDL test
    ///   [`HotKey::from_file`] applies.
    ///
    /// An existing file is never replaced, since it may hold a key with funds. The file is read
    /// back through [`HotKey::from_file`], so a key the executor would refuse is never handed out.
    /// On any failure the new file is removed. No address was shown yet, so it holds no funds.
    pub fn create_file(path: &Path) -> Result<HotKey, KeyError> {
        let err = |e: std::io::Error| KeyError::File(path.to_path_buf(), e.to_string());
        let mut sk = secp256k1::SecretKey::new(&mut secp256k1::rand::rngs::OsRng);
        let mut secret = sk.secret_bytes();
        sk.non_secure_erase();
        let made = HotKey::from_secret(secret);
        wipe(&mut secret);
        let key = made?;
        create_owner_only(path).map_err(err)?;
        let written = (|| {
            restrict_to_owner(path)?;
            use std::io::Write;
            let mut text = format!("{}\n", kob_protocol::json::to_hex(&key.secret)).into_bytes();
            let mut f = std::fs::OpenOptions::new().write(true).truncate(true).open(path).map_err(err)?;
            let w = f.write_all(&text).and_then(|_| f.sync_all());
            wipe(&mut text);
            w.map_err(err)?;
            let back = HotKey::from_file(path)?;
            if back.pubkey != key.pubkey {
                return Err(KeyError::Invalid("the key file does not read back as the key written".into()));
            }
            Ok(())
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(path);
            return Err(e);
        }
        Ok(key)
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

/// Creates the empty key file, failing when anything exists at `path`. Only its owner can read or
/// write it from the start.
#[cfg(unix)]
fn create_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map(|_| ())
}

/// Mode `0600` (the umask can only have removed bits), checked as loading checks it.
#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), KeyError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| KeyError::File(path.to_path_buf(), e.to_string()))?;
    check_permissions(path)
}

/// Creates the empty key file, failing when anything exists at `path`.
///
/// Its ACL is the inherited one until [`restrict_to_owner`], and nothing is written before that.
#[cfg(not(unix))]
fn create_owner_only(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(path).map(|_| ())
}

/// Replaces the file's ACL by one full-control entry for the current account (no inherited
/// entries), then requires that no other account or group may read it.
///
/// This is the strict form of the check [`HotKey::from_file`] applies, whatever
/// `KOB_KEY_ACL_STRICT` says.
#[cfg(not(unix))]
fn restrict_to_owner(path: &Path) -> Result<(), KeyError> {
    let file_err = |m: String| KeyError::File(path.to_path_buf(), m);
    let me = current_account().ok_or_else(|| file_err("cannot name the current account (whoami, USERNAME)".into()))?;
    let out = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{me}:F"))
        .output()
        .map_err(|e| file_err(format!("icacls: {e}")))?;
    if !out.status.success() {
        return Err(file_err(format!("icacls could not restrict the file: {}", String::from_utf8_lossy(&out.stdout).trim())));
    }
    let sddl = windows_acl_sddl(path).ok_or_else(|| file_err("cannot read the file's ACL back (icacls /save)".into()))?;
    let others = sddl_readable_by_others(&sddl, current_sid().as_deref());
    if !others.is_empty() {
        return Err(KeyError::Acl(path.to_path_buf(), trustee_names(&others), acl_fix(path)));
    }
    Ok(())
}

/// The command that gives the key file at `path` one entry, full control for the account running
/// kobd.
///
/// The account is named `DOMAIN\user` when the environment says so, else by its SID. The refusal
/// prints the command as it is.
#[cfg(not(unix))]
fn acl_fix(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let me =
        match (std::env::var("USERDOMAIN").ok().filter(|d| !d.is_empty()), std::env::var("USERNAME").ok().filter(|u| !u.is_empty())) {
            (Some(d), Some(u)) => format!("{d}\\{u}"),
            (None, Some(u)) => u,
            _ => current_account().unwrap_or_else(|| "<your account>".into()),
        };
    format!("icacls \"{}\" /inheritance:r /grant:r \"{me}:F\"", abs.display())
}

/// The SID of the account that runs this process (`whoami /user`), once.
#[cfg(not(unix))]
fn current_sid() -> Option<String> {
    static SID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SID.get_or_init(|| {
        std::process::Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| sid_of_whoami_csv(&String::from_utf8_lossy(&o.stdout)))
    })
    .clone()
}

/// The current account for `icacls /grant`: `*<SID>` from `whoami /user` (unambiguous,
/// locale-independent), else `%USERDOMAIN%\%USERNAME%`.
#[cfg(not(unix))]
fn current_account() -> Option<String> {
    if let Some(sid) = current_sid() {
        return Some(format!("*{sid}"));
    }
    let user = std::env::var("USERNAME").ok().filter(|u| !u.is_empty())?;
    Some(match std::env::var("USERDOMAIN").ok().filter(|d| !d.is_empty()) {
        Some(d) => format!("{d}\\{user}"),
        None => user,
    })
}

/// The SID of `whoami /user /fo csv /nh` output (`"host\user","S-1-5-21-..."`).
pub fn sid_of_whoami_csv(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let sid = line.rsplit(',').next()?.trim().trim_matches('"');
    (sid.starts_with("S-1-") && sid[4..].chars().all(|c| c.is_ascii_digit() || c == '-')).then(|| sid.to_string())
}

/// Name of the variable that relaxes the Windows key-file check.
///
/// By default, and on every build, a key file that another account may read is refused.
/// `KOB_KEY_ACL_STRICT=0` turns the refusal into a warning, and so it does for a failure to read
/// the ACL. Any other value, or none, refuses.
pub const STRICT_ACL_ENV: &str = "KOB_KEY_ACL_STRICT";

/// Whether the Windows key-file check refuses (`KOB_KEY_ACL_STRICT` unset or anything but `0`).
pub fn acl_strict() -> bool {
    std::env::var(STRICT_ACL_ENV).map(|v| v.trim() != "0").unwrap_or(true)
}

/// Trustees of an SDDL ACE that are not "another account".
///
/// They are the account that runs the executor (passed in) and the privileged principals, which can
/// read any file of the machine anyway, so refusing them protects nothing. A profile folder's
/// inherited ACL names them on a fresh Windows and on CI runners. The privileged principals are:
///
/// - SYSTEM (`SY`) and the Administrators group (`BA`);
/// - the built-in Administrator account (`LA`, or its full SID with RID 500,
///   [`builtin_administrator`]);
/// - TrustedInstaller;
/// - the owner-rights and creator-owner placeholders (aliases and the SIDs they abbreviate).
///
/// Every other trustee that may read the file is another account: a well-known group (Everyone,
/// Users, Authenticated Users, Guests, ...), a named account, or a named group (a second user, a
/// sync client's service account).
const OWN_TRUSTEES: &[&str] = &[
    "SY",
    "BA",
    "LA",
    "OW",
    "CO",
    "S-1-5-18",
    "S-1-5-32-544",
    "S-1-3-4",
    "S-1-3-0",
    // NT SERVICE\TrustedInstaller
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
];

/// The built-in Administrator account by its full SID (`S-1-5-21-<machine or domain>-500`).
///
/// `LA` abbreviates it for the local machine's.
fn builtin_administrator(sid: &str) -> bool {
    sid.len() > "S-1-5-21-".len() && sid[.."S-1-5-21-".len()].eq_ignore_ascii_case("S-1-5-21-") && sid.ends_with("-500")
}

/// A trustee of an SDDL ACE as people know it.
///
/// Well-known aliases and SIDs map to their names (`BU` is `Users`, `WD` is `Everyone`, ...).
/// Anything else (a named account's SID) stays as it is.
pub fn trustee_name(trustee: &str) -> String {
    let name = match trustee.to_ascii_uppercase().as_str() {
        "WD" | "S-1-1-0" => "Everyone",
        "BU" | "S-1-5-32-545" => "Users",
        "AU" | "S-1-5-11" => "Authenticated Users",
        "BG" | "S-1-5-32-546" => "Guests",
        "IU" | "S-1-5-4" => "Interactive users",
        "NU" | "S-1-5-2" => "Network logon users",
        "AN" | "S-1-5-7" => "Anonymous",
        "LG" => "the Guest account",
        "PU" | "S-1-5-32-547" => "Power Users",
        "RD" | "S-1-5-32-555" => "Remote Desktop Users",
        "DU" => "Domain Users",
        _ => return trustee.to_string(),
    };
    name.to_string()
}

/// [`trustee_name`] of each trustee, comma-separated.
pub fn trustee_names(trustees: &[String]) -> String {
    trustees.iter().map(|t| trustee_name(t)).collect::<Vec<_>>().join(", ")
}

/// Whether an SDDL access-rights field grants reading the file's data.
///
/// The field is an alias (`FA` file all, `FR` file read, `GA` generic all, `GR` generic read) or a
/// hexadecimal mask with FILE_READ_DATA (0x1), GENERIC_READ (0x80000000) or GENERIC_ALL
/// (0x10000000). An unparsable mask counts as read, which fails towards the warning.
fn grants_read(rights: &str) -> bool {
    if let Some(hex) = rights.strip_prefix("0x").or_else(|| rights.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).map(|m| m & (0x1 | 0x8000_0000 | 0x1000_0000) != 0).unwrap_or(true);
    }
    // two-letter aliases, concatenated
    rights.as_bytes().chunks(2).any(|c| matches!(c, b"FA" | b"FR" | b"GA" | b"GR"))
}

/// The other accounts an SDDL DACL lets read the file.
///
/// These are the trustees of every access-ALLOWED ACE (`A`) that grants read access, other than
/// `me` (the SID of the account that runs the executor), SYSTEM, Administrators and the owner
/// placeholders ([`OWN_TRUSTEES`]). Everyone, Users and the like count, and so does any named
/// account or group. With `me` unknown (`None`) every full SID counts as another account. Deny ACEs
/// are ignored.
///
/// The function is pure text. The platform-specific part only fetches the SDDL.
pub fn sddl_readable_by_others(sddl: &str, me: Option<&str>) -> Vec<String> {
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
        let own = OWN_TRUSTEES.iter().any(|t| t.eq_ignore_ascii_case(trustee))
            || builtin_administrator(trustee)
            || me.is_some_and(|m| m.eq_ignore_ascii_case(trustee));
        if !own && grants_read(f[2]) {
            out.push(trustee.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Windows: the key file's ACL (`icacls /save`, SDDL) must let no other account read it
/// ([`sddl_readable_by_others`], named accounts included).
///
/// An ACL that cannot be read (no `icacls`, an error) and an account whose SID cannot be found
/// (`whoami`) are refused too. This is the default on every build. `KOB_KEY_ACL_STRICT=0` makes
/// each of these a warning.
#[cfg(not(unix))]
fn check_permissions(path: &Path) -> Result<(), KeyError> {
    std::fs::metadata(path).map_err(|e| KeyError::File(path.to_path_buf(), e.to_string()))?;
    let strict = acl_strict();
    let problem = match (windows_acl_sddl(path), current_sid()) {
        (Some(sddl), me) => {
            let others = sddl_readable_by_others(&sddl, me.as_deref());
            if others.is_empty() {
                return Ok(());
            }
            let who = if me.is_none() {
                format!(
                    "{} (the account running kobd could not be named by `whoami`, so every account counts)",
                    trustee_names(&others)
                )
            } else {
                trustee_names(&others)
            };
            if strict {
                return Err(KeyError::Acl(path.to_path_buf(), who, acl_fix(path)));
            }
            who
        }
        (None, _) => {
            if strict {
                return Err(KeyError::File(
                    path.to_path_buf(),
                    "its ACL could not be read (icacls /save failed or is missing), so nobody can tell who else may read the key; \
                     fix icacls, or set KOB_KEY_ACL_STRICT=0 to start anyway"
                        .into(),
                ));
            }
            tracing::warn!(path = %path.display(), "could not read the ACL of the operator key file (icacls); KOB_KEY_ACL_STRICT=0 lets it start: check it by hand");
            return Ok(());
        }
    };
    tracing::warn!(
        path = %path.display(),
        readable_by = %problem,
        fix = %acl_fix(path),
        "operator key file is readable by other accounts; KOB_KEY_ACL_STRICT=0 lets it start: restrict it with the command `fix`"
    );
    Ok(())
}

/// The SDDL of a file, from `icacls <file> /save <tmp>`. SDDL is locale-independent, unlike icacls'
/// account names.
#[cfg(not(unix))]
fn windows_acl_sddl(path: &Path) -> Option<String> {
    // One name per call: two threads of one process reading ACLs at the same time (the tests do)
    // must not share the file icacls writes, where one's removal or write would make the other's
    // read fail.
    static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("kob-acl-{}-{n}-{nanos}.txt", std::process::id()));
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name()?;
    // `icacls` saves paths relative to the working directory of the call, so run it in the file's
    // directory.
    let out = std::process::Command::new("icacls").current_dir(dir).arg(name).arg("/save").arg(&tmp).output().ok()?;
    let saved = if out.status.success() { std::fs::read(&tmp).ok() } else { None };
    let _ = std::fs::remove_file(&tmp);
    let bytes = saved?;
    // Saved as UTF-16LE by icacls (with or without a BOM, depending on the Windows version). Plain
    // text is accepted too.
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
        // Signs digest by digest, so no copy of the secret sits in a temporary key map. A request
        // for any other key is an error: the operator only ever signs its own funding.
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
    /// The operator's x-only public key.
    pub operator: [u8; 32],
    /// Secret keys by x-only public key.
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
        let me = Some("S-1-5-21-1-2-3-1001");
        let others = |sddl: &str| sddl_readable_by_others(sddl, me);
        // the account itself + SYSTEM + Administrators only: fine
        assert!(others("D:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;S-1-5-21-1-2-3-1001)").is_empty());
        // a default user-profile ACL: Users have read/execute inherited from the folder
        let d = "O:S-1-5-21-1-2-3-1001G:S-1-5-21-1-2-3-513D:AI(A;ID;FA;;;BA)(A;ID;FA;;;SY)(A;ID;0x1200a9;;;BU)(A;ID;FA;;;S-1-5-21-1-2-3-1001)";
        assert_eq!(others(d), vec!["BU".to_string()]);
        // named for people in the refusal
        assert_eq!(trustee_names(&others(d)), "Users");
        assert_eq!(
            trustee_names(&["WD".into(), "S-1-5-11".into(), "S-1-5-21-1-2-3-1002".into()]),
            "Everyone, Authenticated Users, S-1-5-21-1-2-3-1002"
        );
        assert_eq!(others("D:(A;;FR;;;WD)(A;;FA;;;AU)(A;;GR;;;WD)"), vec!["AU".to_string(), "WD".to_string()]);
        assert_eq!(others("D:(A;;GA;;;S-1-1-0)"), vec!["S-1-1-0".to_string()]);
        // A named second account (or a group by its SID) may read: that is another account too.
        assert_eq!(others("D:P(A;;FA;;;S-1-5-21-1-2-3-1001)(A;;FR;;;S-1-5-21-1-2-3-1002)"), vec!["S-1-5-21-1-2-3-1002".to_string()]);
        assert_eq!(others("D:P(A;;FA;;;S-1-5-21-1-2-3-1001)(A;;0x1200a9;;;S-1-5-80-99)"), vec!["S-1-5-80-99".to_string()]);
        // The account running kobd is unknown, so its own entry counts as another account (refuse
        // rather than guess).
        assert_eq!(
            sddl_readable_by_others("D:P(A;;FA;;;SY)(A;;FA;;;S-1-5-21-1-2-3-1001)", None),
            vec!["S-1-5-21-1-2-3-1001".to_string()]
        );
        // a service running as SYSTEM, the owner-rights placeholder
        assert!(sddl_readable_by_others("D:P(A;;FA;;;SY)(A;;FA;;;OW)", Some("S-1-5-18")).is_empty());
        // The privileged principals can read any file anyway. They are the built-in Administrator
        // (as the alias the profile folder of a fresh Windows or a CI runner gives it, or by its
        // RID 500 SID) and TrustedInstaller. A temp folder's inherited ACL passes.
        let runner = "D:AI(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;LA)";
        assert!(others(runner).is_empty(), "{:?}", others(runner));
        assert!(sddl_readable_by_others(runner, Some("S-1-5-21-9-8-7-500")).is_empty(), "the account running kobd is LA itself");
        assert!(others("D:(A;;FA;;;S-1-5-21-9-8-7-500)(A;;FR;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)")
            .is_empty());
        // But a group of ordinary accounts beside them, the guest account, or an account whose RID
        // merely contains 500 does not pass.
        assert_eq!(others(&format!("{runner}(A;OICIID;0x1200a9;;;BU)")), vec!["BU".to_string()]);
        assert_eq!(others("D:(A;;FR;;;LG)(A;;FR;;;S-1-5-21-1-2-3-5001)"), vec!["LG".to_string(), "S-1-5-21-1-2-3-5001".to_string()]);
        // no read bit (write attributes / synchronise only), a deny, a non-DACL section, a SACL
        assert!(others("D:(A;;0x100100;;;WD)").is_empty());
        assert!(others("D:(D;;FA;;;WD)").is_empty());
        assert!(others("O:WDG:WD").is_empty());
        assert!(others("D:(A;;FA;;;SY)S:(A;;FR;;;WD)").is_empty());
        assert!(others("").is_empty());
        // an unparsable mask counts as read (fail towards the refusal)
        assert_eq!(others("D:(A;;0xZZ;;;WD)"), vec!["WD".to_string()]);
    }

    /// The Windows check against the real ACL of a real file.
    ///
    /// After `icacls /inheritance:r /grant:r <me>:F` nobody else may read it. Granting Everyone
    /// read is refused (the default), and loads with a warning under `KOB_KEY_ACL_STRICT=0`.
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
            assert!(sddl_readable_by_others(&sddl, current_sid().as_deref()).is_empty(), "{sddl}");
            assert_eq!(HotKey::from_file(&p).unwrap().pubkey(), pubkey_of(&[0x33; 32]).unwrap());
            // The built-in Administrator, SYSTEM and Administrators may read (a CI runner's temp
            // folder grants them), and the file still loads.
            if let Some(admin) = current_sid().and_then(|s| s.rsplit_once('-').map(|(m, _)| format!("{m}-500"))) {
                for who in [format!("*{admin}:R"), "*S-1-5-18:F".into(), "*S-1-5-32-544:F".into()] {
                    let ok = std::process::Command::new("icacls").arg(&p).arg("/grant").arg(&who).output().unwrap().status.success();
                    assert!(ok, "icacls /grant {who}");
                }
                let sddl = windows_acl_sddl(&p).expect("the ACL is readable");
                assert!(sddl_readable_by_others(&sddl, current_sid().as_deref()).is_empty(), "{sddl}");
                assert!(HotKey::from_file(&p).is_ok(), "{sddl}");
            }
            // Everyone may read: refused by default.
            assert!(std::process::Command::new("icacls").arg(&p).args(["/grant", "*S-1-1-0:R"]).output().unwrap().status.success());
            let sddl = windows_acl_sddl(&p).expect("the ACL is readable");
            assert_eq!(sddl_readable_by_others(&sddl, current_sid().as_deref()), vec!["WD".to_string()], "{sddl}");
            if std::env::var_os(STRICT_ACL_ENV).is_none() {
                let e = HotKey::from_file(&p).unwrap_err();
                assert!(matches!(e, KeyError::Acl(..)) && e.to_string().contains("KOB_KEY_ACL_STRICT=0"), "{e}");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `create_file` writes a fresh key nobody else may read, never replaces a file, and the key
    /// loads like any key file.
    #[test]
    fn a_created_key_file_is_owner_only_and_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("operator.key");
        let k = HotKey::create_file(&p).expect("created");
        assert_eq!(HotKey::from_file(&p).unwrap().pubkey(), k.pubkey());
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.trim().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        #[cfg(windows)]
        {
            let sddl = windows_acl_sddl(&p).expect("the ACL is readable");
            assert!(sddl_readable_by_others(&sddl, current_sid().as_deref()).is_empty(), "{sddl}");
        }
        // an existing file (a key that may hold funds) is left exactly as it was
        assert!(matches!(HotKey::create_file(&p), Err(KeyError::File(..))));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        // two keys are two keys
        let q = dir.path().join("other.key");
        assert_ne!(HotKey::create_file(&q).unwrap().pubkey(), k.pubkey());
    }

    #[test]
    fn whoami_csv_names_the_sid() {
        assert_eq!(sid_of_whoami_csv("\"host\\me\",\"S-1-5-21-1-2-3-1001\"\r\n").as_deref(), Some("S-1-5-21-1-2-3-1001"));
        assert_eq!(sid_of_whoami_csv("\n\"a,b\\c\",\"S-1-5-18\"\n").as_deref(), Some("S-1-5-18"));
        assert_eq!(sid_of_whoami_csv("ERROR: nothing"), None);
        assert_eq!(sid_of_whoami_csv(""), None);
        assert_eq!(sid_of_whoami_csv("\"x\",\"S-1-5-21-1 & del\""), None);
    }

    #[test]
    fn wipe_zeroes_the_buffer() {
        let mut b = b"00112233".to_vec();
        wipe(&mut b);
        assert!(b.iter().all(|x| *x == 0));
    }
}
