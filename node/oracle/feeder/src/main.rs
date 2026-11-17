//! `perps-oracle`: the oracle feeder CLI (UNAUDITED PROTOTYPE).
//!
//! ```text
//! perps-oracle address  --key-file F
//! perps-oracle dev-key  --label L --out F                      (THROWAWAY key from a public label)
//! perps-oracle observe  --fixtures DIR --signer I --key-file F  (sign one round from recorded responses)
//! perps-oracle replay   --rounds DIR                            (unsigned observations of recorded rounds, in order)
//! perps-oracle assemble --obs F1 F2 ...                         (OracleUpdate from feeder submissions)
//! perps-oracle apply    --committee F --updates JSONL           (the engine rule from genesis over updates)
//! perps-oracle record   --out DIR [--rounds N]                  (net feature: record live venue responses)
//! perps-oracle run      --signer I --key-file F --out JSONL [--rounds N]   (net feature: live feeder)
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use perps_oracle::committee::{apply_update, begin_block, OracleState};
use perps_oracle::types::{round_of, OracleUpdate, SignedObservation};
use perps_oracle::{eip712, sig};
use perps_oracle_feeder::json;
use perps_oracle_feeder::keyfile;
use perps_oracle_feeder::pipeline::{default_config, Feeder, Response};
use serde_json::{json, Value};

fn args_map(args: &[String]) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut key: Option<String> = None;
    for a in args {
        if let Some(k) = a.strip_prefix("--") {
            m.entry(k.to_string()).or_default();
            key = Some(k.to_string());
        } else if let Some(k) = &key {
            m.get_mut(k).unwrap().push(a.clone());
        } else {
            return Err(format!("unexpected argument {a}"));
        }
    }
    Ok(m)
}

fn one<'a>(m: &'a BTreeMap<String, Vec<String>>, k: &str) -> Result<&'a str, String> {
    match m.get(k).map(|v| v.as_slice()) {
        Some([v]) => Ok(v),
        _ => Err(format!("--{k} takes exactly one value")),
    }
}

fn read_json(p: &Path) -> Result<Value, String> {
    let t = fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))
}

/// A recorded round: every `*.json` file of `dir` in the recorder's format
/// `{"venue","url","sentMs","receivedMs","curlExit"|"error","body"}`. The sampling time is the earliest `sentMs`.
pub fn load_round(dir: &Path) -> Result<(u64, Vec<Response>), String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    let mut t0 = u64::MAX;
    for f in files {
        let v = read_json(&f)?;
        let g = |k: &str| v.get(k).ok_or(format!("{}: missing {k}", f.display()));
        let sent = g("sentMs")?.as_u64().ok_or("sentMs")?;
        t0 = t0.min(sent);
        let failed = v.get("curlExit").and_then(Value::as_i64).is_some_and(|c| c != 0) || v.get("error").is_some_and(|e| !e.is_null());
        out.push(Response {
            source: g("venue")?.as_str().ok_or("venue")?.to_string(),
            body: if failed { None } else { Some(g("body")?.as_str().ok_or("body")?.to_string()) },
            received_ms: g("receivedMs")?.as_u64().ok_or("receivedMs")?,
            error: if failed { Some(v.get("error").and_then(Value::as_str).unwrap_or("fetch failed").to_string()) } else { None },
        });
    }
    if out.is_empty() {
        return Err(format!("{}: no recorded responses", dir.display()));
    }
    Ok((t0, out))
}

fn sign_observation(signer: u8, key: &k256::ecdsa::SigningKey, o: &perps_oracle::types::Observation) -> SignedObservation {
    let sigb = sig::sign(key, &eip712::digest(o));
    SignedObservation { signer, time_ms: o.time_ms, prices: o.prices.clone(), signature: sigb }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let cmd = args.first().cloned().unwrap_or_default();
    let m = args_map(&args[1.min(args.len())..])?;
    match cmd.as_str() {
        "address" => {
            let k = keyfile::read_key(Path::new(one(&m, "key-file")?))?;
            println!("0x{}", hex::encode(sig::address_of(k.verifying_key())));
        }
        "dev-key" => {
            let k = keyfile::write_dev_key(one(&m, "label")?, Path::new(one(&m, "out")?))?;
            eprintln!("THROWAWAY development key (derived from a public label; never for real use)");
            println!("0x{}", hex::encode(sig::address_of(k.verifying_key())));
        }
        "observe" => {
            let (t0, resp) = load_round(Path::new(one(&m, "fixtures")?))?;
            let signer: u8 = one(&m, "signer")?.parse().map_err(|_| "--signer: 0..255")?;
            let key = keyfile::read_key(Path::new(one(&m, "key-file")?))?;
            let mut f = Feeder::new(default_config()).map_err(|e| format!("{e:?}"))?;
            let (obs, rep) = f.observe(round_of(t0), t0, &resp);
            let signed = obs.as_ref().map(|o| json::signed_to_json(&sign_observation(signer, &key, o), o.round));
            println!("{}", serde_json::to_string_pretty(&json!({"observation": signed, "report": json::round_report_to_json(&rep)})).unwrap());
        }
        "replay" => {
            let root = PathBuf::from(one(&m, "rounds")?);
            let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
                .map_err(|e| format!("{}: {e}", root.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            let mut f = Feeder::new(default_config()).map_err(|e| format!("{e:?}"))?;
            for d in dirs {
                let (t0, resp) = load_round(&d)?;
                let (obs, rep) = f.observe(round_of(t0), t0, &resp);
                let line = json!({
                    "dir": d.file_name().unwrap().to_string_lossy(),
                    "round": rep.round,
                    "prices": obs.map(|o| o.prices.iter().map(json::price_to_json).collect::<Vec<_>>()),
                    "report": json::round_report_to_json(&rep),
                });
                println!("{}", serde_json::to_string(&line).unwrap());
            }
        }
        "assemble" => {
            let files = m.get("obs").ok_or("--obs F1 F2 ...")?;
            let mut round = None;
            let mut obs = Vec::new();
            for f in files {
                let (r, o) = json::signed_from_json(&read_json(Path::new(f))?)?;
                if *round.get_or_insert(r) != r {
                    return Err(format!("{f}: round {r} differs"));
                }
                obs.push(o);
            }
            obs.sort_by_key(|o| o.signer);
            let u = OracleUpdate { round: round.ok_or("no observations")?, observations: obs };
            println!("{}", serde_json::to_string(&json::update_to_json(&u)).unwrap());
        }
        "apply" => {
            let prm = json::committee_from_json(&read_json(Path::new(one(&m, "committee")?))?)?;
            let mut st = OracleState::genesis(&prm).map_err(|e| format!("{e:?}"))?;
            let text = fs::read_to_string(one(&m, "updates")?).map_err(|e| e.to_string())?;
            for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
                let v: Value = serde_json::from_str(line).map_err(|e| format!("line {}: {e}", i + 1))?;
                let t = v.get("blockTime").and_then(Value::as_u64).ok_or(format!("line {}: blockTime", i + 1))?;
                let stale = begin_block(&prm, &mut st, t);
                let u = json::update_from_json(v.get("update").ok_or(format!("line {}: update", i + 1))?)?;
                let res = match apply_update(&prm, &mut st, &u, t) {
                    Ok(r) => json::report_to_json(&r),
                    Err(e) => json::error_to_json(&e),
                };
                println!("{}", serde_json::to_string(&json!({"blockTime": t, "haltedStale": stale, "result": res})).unwrap());
            }
            println!("{}", serde_json::to_string(&json!({"stateBytes": format!("0x{}", hex::encode(st.encode()))})).unwrap());
        }
        #[cfg(feature = "net")]
        "record" => perps_oracle_feeder::net::record(&m)?,
        #[cfg(feature = "net")]
        "run" => perps_oracle_feeder::net::run_live(&m)?,
        _ => return Err(format!("unknown command {cmd:?}; see the header of feeder/src/main.rs")),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("perps-oracle: {e}");
            ExitCode::from(2)
        }
    }
}
