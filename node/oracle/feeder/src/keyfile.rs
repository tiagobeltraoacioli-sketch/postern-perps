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
    let meta =
        fs::symlink_metadata(path).map_err(|e| format!("key file {}: {e}", path.display()))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("key file: must be a regular file, not a link".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "key file: mode {mode:o} lets others read or write it; use chmod 600"
            ));
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
    let mut f = opts
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{}", hex::encode(k.to_bytes())).map_err(|e| e.to_string())?;
    Ok(k)
}

/// A fresh THROWAWAY key from the operating system's random source (`/dev/urandom`). For run-time dev committees
/// only: it is written to a 0600 file in a directory the caller names and is never committed anywhere.
pub fn random_key() -> Result<SigningKey, String> {
    use std::io::Read;
    let mut f = fs::File::open("/dev/urandom").map_err(|e| format!("/dev/urandom: {e}"))?;
    loop {
        let mut b = [0u8; 32];
        f.read_exact(&mut b)
            .map_err(|e| format!("/dev/urandom: {e}"))?;
        if b == [0u8; 32] {
            return Err("/dev/urandom returned zeros".into());
        }
        if let Ok(k) = SigningKey::from_bytes(&b.into()) {
            return Ok(k);
        }
    }
}

/// Writes a key with mode 0600; refuses to overwrite.
pub fn write_key(k: &SigningKey, path: &Path) -> Result<(), String> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{}", hex::encode(k.to_bytes())).map_err(|e| e.to_string())
}

/// A dev oracle committee held by ONE process (development and rehearsals only; in production every validator
/// operator holds its own key on its own host). `keys[i]` signs as committee index `i`; addresses strictly
/// ascending, as the node's genesis orders its committees.
pub struct DevCommittee {
    pub quorum: u8,
    pub keys: Vec<SigningKey>,
}

impl DevCommittee {
    pub fn addresses(&self) -> Vec<[u8; 20]> {
        self.keys
            .iter()
            .map(|k| perps_oracle::sig::address_of(k.verifying_key()))
            .collect()
    }

    fn check(self) -> Result<Self, String> {
        let n = self.keys.len();
        if n == 0 || n > 255 {
            return Err("committee: 1 to 255 signers".into());
        }
        let q = usize::from(self.quorum);
        if !(n / 2 < q && q <= n) {
            return Err(format!(
                "committee: quorum {q} is not a strict majority of {n}"
            ));
        }
        let a = self.addresses();
        if a.windows(2).any(|w| w[0] >= w[1]) {
            return Err("committee: addresses must be strictly ascending".into());
        }
        Ok(self)
    }

    /// `n` fresh random throwaway keys, sorted by address.
    pub fn generate(n: usize, quorum: u8) -> Result<Self, String> {
        let mut keys = (0..n)
            .map(|_| random_key())
            .collect::<Result<Vec<_>, _>>()?;
        keys.sort_by_key(|k| perps_oracle::sig::address_of(k.verifying_key()));
        DevCommittee { quorum, keys }.check()
    }

    /// Writes `dir/oracle-<i>.key` (0600) and `dir/committee.json` (`{"quorum", "signers"}`: public). The
    /// directory must not exist yet (mode 0700 on creation).
    pub fn write_dir(&self, dir: &Path) -> Result<(), String> {
        let mut b = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(dir)
            .map_err(|e| format!("{}: {e} (the key directory must be new)", dir.display()))?;
        for (i, k) in self.keys.iter().enumerate() {
            write_key(k, &dir.join(format!("oracle-{i}.key")))?;
        }
        fs::write(
            dir.join("committee.json"),
            self.public_json().to_string() + "\n",
        )
        .map_err(|e| e.to_string())
    }

    pub fn public_json(&self) -> serde_json::Value {
        let s: Vec<String> = self
            .addresses()
            .iter()
            .map(|a| format!("0x{}", hex::encode(a)))
            .collect();
        serde_json::json!({
            "_note": "THROWAWAY dev oracle committee generated at run time; never a real committee",
            "quorum": self.quorum,
            "signers": s,
            "committeeArg": s.join(","),
        })
    }

    /// Reads a directory written by [`DevCommittee::write_dir`]; every key must be the one `committee.json` names
    /// at its index.
    pub fn read_dir(dir: &Path) -> Result<Self, String> {
        let p = dir.join("committee.json");
        let v: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?,
        )
        .map_err(|e| format!("{}: {e}", p.display()))?;
        let quorum = v
            .get("quorum")
            .and_then(serde_json::Value::as_u64)
            .and_then(|q| u8::try_from(q).ok())
            .ok_or("committee.json: quorum")?;
        let signers = v
            .get("signers")
            .and_then(serde_json::Value::as_array)
            .ok_or("committee.json: signers")?;
        let mut keys = Vec::new();
        for (i, s) in signers.iter().enumerate() {
            let k = read_key(&dir.join(format!("oracle-{i}.key")))?;
            let want = s.as_str().ok_or("committee.json: signer")?;
            let have = format!(
                "0x{}",
                hex::encode(perps_oracle::sig::address_of(k.verifying_key()))
            );
            if want != have {
                return Err(format!(
                    "oracle-{i}.key is not the key of signer {i} ({want})"
                ));
            }
            keys.push(k);
        }
        DevCommittee { quorum, keys }.check()
    }

    /// The throwaway oracle committee of the NODE's dev genesis (`node/src/dev.rs` `dev_keys`: the keys
    /// `sig::throwaway_key("{seed}/oracle", i)` for `i < n`, sorted by address), so the replay feeder can drive a dev
    /// node that was not given `--oracle-committee`. Public derivation: anyone can recompute these keys.
    pub fn node_dev(seed: &str, n: usize, quorum: u8) -> Result<Self, String> {
        let mut keys: Vec<SigningKey> = (0..n as u64)
            .map(|i| {
                let mut ctr = 0u32;
                loop {
                    let s = keccak256(
                        format!("postern-perps THROWAWAY dev key | {seed}/oracle | {i} | {ctr}")
                            .as_bytes(),
                    );
                    if let Ok(k) = SigningKey::from_bytes(&s.into()) {
                        return k;
                    }
                    ctr += 1;
                }
            })
            .collect();
        keys.sort_by_key(|k| perps_oracle::sig::address_of(k.verifying_key()));
        DevCommittee { quorum, keys }.check()
    }
}
