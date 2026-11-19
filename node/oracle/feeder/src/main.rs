//! `perps-oracle`: the oracle feeder CLI (UNAUDITED PROTOTYPE).
//!
//! ```text
//! perps-oracle address  --key-file F
//! perps-oracle dev-key  --label L --out F                      (THROWAWAY key from a public label)
//! perps-oracle observe  --fixtures DIR --signer I --key-file F --vault 0x..  (sign one round from recorded responses)
//! perps-oracle replay   --rounds DIR                            (unsigned observations of recorded rounds, in order)
//! perps-oracle assemble --obs F1 F2 ...                         (OracleUpdate from feeder submissions)
//! perps-oracle apply    --committee F --updates JSONL           (the engine rule from genesis over updates)
//! perps-oracle record   --out FILE.jsonl [--rounds N]           (net feature: record live venue responses)
//! perps-oracle run      (--signer I --key-file F | --key-dir DIR) --vault 0x.. --out JSONL [--rounds N]
//!                       [--record FILE.jsonl] [--node http://127.0.0.1:P [--token-file F]]   (net feature: live feeder)
//! perps-oracle exit     --committee F --updates JSONL --market M --at T  (exit price band after the updates)
//! perps-oracle committee-gen --n 7 --quorum 5 --out DIR         (run-time THROWAWAY dev committee, keys 0600)
//! perps-oracle tape     --recording DIR|FILE.jsonl [--every K]  (recorded venue rounds -> replay tape, by the pipeline)
//! perps-oracle tape-check --tape FILE                           (parse a tape, print its expanded rounds' summary)
//! perps-oracle replay-feed --tape FILE --vault 0x.. (--key-dir DIR | --node-dev-seed S [--n 7] [--quorum 5])
//!                       [--node http://127.0.0.1:P [--token-file F]] [--mode updates|observations] [--speed K]
//!                       [--loop] [--rounds N] [--out JSONL] [--strict]      (the deterministic replay source)
//!
//! `--vault` is the PerpsVault address of the deployment: the verifyingContract of the engine signing domain
//! (SPEC 2.1). An observation signed for one deployment is invalid on every other.
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
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

/// A recorded round in the directory form (see `recording`).
pub fn load_round(dir: &Path) -> Result<(u64, Vec<Response>), String> {
    perps_oracle_feeder::recording::load_round_dir(dir)
}

pub fn vault_arg(v: &str) -> Result<[u8; 20], String> {
    let a = json::hex_bytes::<20>(&v.to_lowercase(), "--vault")?;
    if a == [0u8; 20] {
        return Err("--vault: the zero address".into());
    }
    Ok(a)
}

fn sign_observation(
    signer: u8,
    key: &k256::ecdsa::SigningKey,
    vault: &[u8; 20],
    o: &perps_oracle::types::Observation,
) -> SignedObservation {
    let sigb = sig::sign(key, &eip712::digest(&eip712::domain_separator(vault), o));
    SignedObservation {
        signer,
        time_ms: o.time_ms,
        prices: o.prices.clone(),
        signature: sigb,
    }
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
            eprintln!(
                "THROWAWAY development key (derived from a public label; never for real use)"
            );
            println!("0x{}", hex::encode(sig::address_of(k.verifying_key())));
        }
        "observe" => {
            let (t0, resp) = load_round(Path::new(one(&m, "fixtures")?))?;
            let signer: u8 = one(&m, "signer")?.parse().map_err(|_| "--signer: 0..255")?;
            let key = keyfile::read_key(Path::new(one(&m, "key-file")?))?;
            let vault = vault_arg(one(&m, "vault")?)?;
            let mut f = Feeder::new(default_config()).map_err(|e| format!("{e:?}"))?;
            let (obs, rep) = f.observe(round_of(t0), t0, &resp);
            let signed = obs
                .as_ref()
                .map(|o| json::signed_to_json(&sign_observation(signer, &key, &vault, o), o.round));
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"observation": signed, "report": json::round_report_to_json(&rep)})
                )
                .unwrap()
            );
        }
        "replay" => {
            let rounds =
                perps_oracle_feeder::recording::load_recording(Path::new(one(&m, "rounds")?))?;
            let mut f =
                Feeder::new(default_config().with_kraken_batch()).map_err(|e| format!("{e:?}"))?;
            for (label, t0, resp) in rounds {
                let (obs, rep) = f.observe(round_of(t0), t0, &resp);
                let line = json!({
                    "dir": label,
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
            let u = OracleUpdate {
                round: round.ok_or("no observations")?,
                observations: obs,
            };
            println!(
                "{}",
                serde_json::to_string(&json::update_to_json(&u)).unwrap()
            );
        }
        "apply" | "exit" => {
            let exit_at = if cmd == "exit" {
                let mk: u32 = one(&m, "market")?
                    .parse()
                    .map_err(|_| "--market: a market id")?;
                let at: u64 = one(&m, "at")?
                    .parse()
                    .map_err(|_| "--at: block time in ms")?;
                Some((mk, at))
            } else {
                None
            };
            let prm = json::committee_from_json(&read_json(Path::new(one(&m, "committee")?))?)?;
            let mut st = OracleState::genesis(&prm).map_err(|e| format!("{e:?}"))?;
            let text = fs::read_to_string(one(&m, "updates")?).map_err(|e| e.to_string())?;
            for (i, line) in text
                .lines()
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
            {
                let v: Value =
                    serde_json::from_str(line).map_err(|e| format!("line {}: {e}", i + 1))?;
                let t = v
                    .get("blockTime")
                    .and_then(Value::as_u64)
                    .ok_or(format!("line {}: blockTime", i + 1))?;
                let stale = begin_block(&prm, &mut st, t);
                let u = json::update_from_json(
                    v.get("update").ok_or(format!("line {}: update", i + 1))?,
                )?;
                let res = match apply_update(&prm, &mut st, &u, t) {
                    Ok(r) => json::report_to_json(&r),
                    Err(e) => json::error_to_json(&e),
                };
                println!(
                    "{}",
                    serde_json::to_string(
                        &json!({"blockTime": t, "haltedStale": stale, "result": res})
                    )
                    .unwrap()
                );
            }
            println!(
                "{}",
                serde_json::to_string(
                    &json!({"stateBytes": format!("0x{}", hex::encode(st.encode()))})
                )
                .unwrap()
            );
            if let Some((mk, at)) = exit_at {
                let x = perps_oracle::committee::exit_price(&prm, &st, mk, at);
                println!("{}", serde_json::to_string(&json!({"exit": x.map(|x| json!({
                    "source": format!("{:?}", x.source), "low": x.low.to_string(), "high": x.high.to_string(),
                    "reference": x.reference.to_string(), "settle": x.settle.to_string(), "ageMs": x.age_ms, "settleDue": x.settle_due}))})).unwrap());
            }
        }
        "committee-gen" => {
            let n: usize = one(&m, "n")?.parse().map_err(|_| "--n: 1..255")?;
            let q: u8 = one(&m, "quorum")?.parse().map_err(|_| "--quorum: 1..255")?;
            let c = keyfile::DevCommittee::generate(n, q)?;
            c.write_dir(Path::new(one(&m, "out")?))?;
            eprintln!(
                "THROWAWAY dev oracle committee (random keys generated now; never for real use)"
            );
            println!("{}", c.public_json());
        }
        "tape" => {
            let every: usize = match m.get("every") {
                Some(_) => one(&m, "every")?
                    .parse()
                    .ok()
                    .filter(|k| *k >= 1)
                    .ok_or("--every: at least 1")?,
                None => 1,
            };
            let rounds =
                perps_oracle_feeder::recording::load_recording(Path::new(one(&m, "recording")?))?;
            let mut f =
                Feeder::new(default_config().with_kraken_batch()).map_err(|e| format!("{e:?}"))?;
            for (i, (label, t0, resp)) in rounds.iter().enumerate() {
                // Every round goes through the pipeline (the frozen trackers need the full history); `--every`
                // only thins what is written.
                let (obs, rep) = f.observe(round_of(*t0), *t0, resp);
                if i % every != 0 {
                    continue;
                }
                let prices: BTreeMap<u32, (u64, u8)> = obs
                    .map(|o| {
                        o.prices
                            .iter()
                            .map(|p| (p.market_id, (p.price, p.sources)))
                            .collect()
                    })
                    .unwrap_or_default();
                let absent: Vec<u32> = rep
                    .markets
                    .iter()
                    .filter(|x| x.result.is_err())
                    .map(|x| x.market_id)
                    .collect();
                println!(
                    "{}",
                    perps_oracle_feeder::tape::row(&prices, &absent, label)
                );
            }
        }
        "tape-check" => {
            let text = fs::read_to_string(one(&m, "tape")?).map_err(|e| e.to_string())?;
            let steps = perps_oracle_feeder::tape::parse(&text)?;
            let silent = steps.iter().filter(|s| s.silent).count();
            let mut range: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
            for s in &steps {
                for (mk, (p, _)) in &s.prices {
                    let e = range.entry(*mk).or_insert((*p, *p));
                    *e = (e.0.min(*p), e.1.max(*p));
                }
            }
            println!(
                "{}",
                json!({"rounds": steps.len(), "silent": silent, "seconds": steps.len() as u64 * 3,
                "markets": range.iter().map(|(mk, (lo, hi))| json!({"marketId": mk,
                    "low": perps_oracle_feeder::tape::format_price(*lo), "high": perps_oracle_feeder::tape::format_price(*hi)})).collect::<Vec<_>>()})
            );
        }
        "replay-feed" => {
            use perps_oracle_feeder::replay;
            let text = fs::read_to_string(one(&m, "tape")?).map_err(|e| e.to_string())?;
            let steps = perps_oracle_feeder::tape::parse(&text)?;
            let vault = vault_arg(one(&m, "vault")?)?;
            let num = |k: &str, default: u64| -> Result<u64, String> {
                match m.get(k) {
                    Some(_) => one(&m, k)?.parse().map_err(|_| format!("--{k}: a number")),
                    None => Ok(default),
                }
            };
            let committee = if m.contains_key("key-dir") {
                keyfile::DevCommittee::read_dir(Path::new(one(&m, "key-dir")?))?
            } else {
                let c = keyfile::DevCommittee::node_dev(
                    one(&m, "node-dev-seed")?,
                    num("n", 7)? as usize,
                    u8::try_from(num("quorum", 5)?).map_err(|_| "--quorum")?,
                )?;
                eprintln!("signing with the node's THROWAWAY dev oracle keys (public derivation; dev genesis only)");
                c
            };
            let node = match m.get("node") {
                None => None,
                Some(_) => {
                    let token = match m.get("token-file") {
                        Some(_) => Some(perps_oracle_feeder::submit::read_token(Path::new(one(
                            &m,
                            "token-file",
                        )?))?),
                        None => None,
                    };
                    Some(perps_oracle_feeder::submit::NodeClient::new(
                        one(&m, "node")?,
                        token,
                    )?)
                }
            };
            let opt = replay::Options {
                speed: num("speed", 1)?.max(1) as usize,
                looped: m.contains_key("loop"),
                mode: match m.get("mode").map(|_| one(&m, "mode")).transpose()? {
                    None | Some("updates") => replay::Mode::Updates,
                    Some("observations") => replay::Mode::Observations,
                    Some(x) => return Err(format!("--mode {x}: updates or observations")),
                },
                offset_ms: num("offset-ms", 150)?.min(2000),
                max_rounds: m.get("rounds").map(|_| num("rounds", 0)).transpose()?,
            };
            let mut log: Box<dyn std::io::Write> = match m.get("out") {
                Some(_) => Box::new(
                    fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(one(&m, "out")?)
                        .map_err(|e| e.to_string())?,
                ),
                None => Box::new(std::io::stdout()),
            };
            eprintln!(
                "replay-feed: {} tape rounds, committee {} (quorum {}), DEV ONLY",
                steps.len(),
                committee.public_json()["committeeArg"]
                    .as_str()
                    .unwrap_or_default(),
                committee.quorum
            );
            let sum = replay::run(
                &steps,
                &committee.keys,
                &vault,
                node.as_ref(),
                &opt,
                &mut replay::SystemClock,
                &mut *log,
            )?;
            eprintln!("replay-feed: end of tape {}", replay::summary_json(&sum));
            if m.contains_key("strict") && node.is_some() && sum.accepted + sum.silent != sum.rounds
            {
                return Err("--strict: not every round was accepted by the node".into());
            }
        }
        #[cfg(feature = "net")]
        "record" => perps_oracle_feeder::net::record(&m)?,
        #[cfg(feature = "net")]
        "run" => perps_oracle_feeder::net::run_live(&m)?,
        _ => {
            return Err(format!(
                "unknown command {cmd:?}; see the header of feeder/src/main.rs"
            ))
        }
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
