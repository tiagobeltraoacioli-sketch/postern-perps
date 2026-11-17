//! The oracle signer's key file (development form). UNAUDITED PROTOTYPE.
//!
//! The key file holds 32 bytes as 64 hex characters (optional `0x`, optional trailing newline). It is refused
//! when group or world can read or write it (like OpenSSH), when it is a symbolic link, or when its content is not
//! exactly that. Production keys are generated and held by each validator operator (owner input: hardware or a
//! dedicated host); agents and tests only use throwaway keys derived from a public label (`dev-key`).

use std::fs;
use std::io::Write;
use std::path::Path;

use k256::ecdsa::SigningKey;
use perps_oracle::keccak256;

pub fn read_key(path: &Path) -> Result<SigningKey, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("key file {}: {e}", path.display()))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("key file: must be a regular file, not a link".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!("key file: mode {mode:o} lets others read or write it; use chmod 600"));
        }
    }
    let text = fs::read_to_string(path).map_err(|e| format!("key file: {e}"))?;
    let t = text.strip_suffix('\n').unwrap_or(&text);
    let t = t.strip_prefix("0x").unwrap_or(t);
    if t.len() != 64 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("key file: expected 64 hex characters".into());
    }
    let b: [u8; 32] = hex::decode(t).unwrap().try_into().unwrap();
    SigningKey::from_bytes(&b.into()).map_err(|_| "key file: not a valid secp256k1 scalar".into())
}

/// A THROWAWAY key derived from a public label: anyone can recompute it. Development and tests only.
pub fn dev_key(label: &str) -> SigningKey {
    let mut seed = keccak256(format!("postern-perps-oracle-THROWAWAY-dev-key:{label}").as_bytes());
    loop {
        if let Ok(k) = SigningKey::from_bytes(&seed.into()) {
            return k;
        }
        seed = keccak256(&seed);
    }
}

/// Writes a throwaway dev key with mode 0600; refuses to overwrite.
pub fn write_dev_key(label: &str, path: &Path) -> Result<SigningKey, String> {
    let k = dev_key(label);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{}", hex::encode(k.to_bytes())).map_err(|e| e.to_string())?;
    Ok(k)
}
