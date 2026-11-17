//! `engine` CLI (SPEC section 13). UNAUDITED PROTOTYPE.
//!
//!   engine replay <genesis.json> <log.jsonl> [--expect-root <0xhex> --at <height>]
//!   engine demo <out-dir> [blocks]      writes a throwaway-key genesis and log (fixtures for node/review)
//!   engine bench [orders]               matching throughput on one core
//!   engine vectors                      EIP-712 test vectors (JSON on stdout) for independent checkers
//!
//! Log format: one JSON object per line `{"height":"1","time":"1794880001000","inputs":"0x<inputsEncoded>"}`
//! where `inputsEncoded` is SPEC 3.2's canonical encoding (the bytes that logHash commits to).
use postern_perps_engine::codec::{decode_inputs, encode_inputs};
use postern_perps_engine::genesis::Genesis;
use postern_perps_engine::hash::hex0x;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;
use std::io::{BufRead, Write};
use std::time::Instant;

fn die(msg: &str) -> ! {
    eprintln!("engine: {msg}");
    std::process::exit(2)
}

fn replay(args: &[String]) {
    if args.len() < 2 {
        die("usage: engine replay <genesis.json> <log.jsonl> [--expect-root <hex> --at <height>]");
    }
    let g = Genesis::from_json(&std::fs::read_to_string(&args[0]).unwrap_or_else(|e| die(&e.to_string()))).unwrap_or_else(|e| die(&e));
    let mut expect: Option<(String, u64)> = None;
    if let Some(i) = args.iter().position(|a| a == "--expect-root") {
        let root = args.get(i + 1).cloned().unwrap_or_else(|| die("--expect-root needs a value"));
        let at = args.iter().position(|a| a == "--at").and_then(|j| args.get(j + 1)).and_then(|v| v.parse().ok()).unwrap_or_else(|| die("--at <height> required with --expect-root"));
        expect = Some((root.to_lowercase(), at));
    }
    let mut e = Engine::new(g);
    let f = std::fs::File::open(&args[1]).unwrap_or_else(|x| die(&x.to_string()));
    let mut matched = false;
    for (ln, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.unwrap_or_else(|x| die(&x.to_string()));
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_else(|x| die(&format!("line {}: {x}", ln + 1)));
        let h: u64 = v["height"].as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| die("height"));
        let t: u64 = v["time"].as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| die("time"));
        let hexs = v["inputs"].as_str().and_then(|s| s.strip_prefix("0x")).unwrap_or_else(|| die("inputs"));
        let bytes = hex::decode(hexs).unwrap_or_else(|x| die(&x.to_string()));
        let inputs = decode_inputs(&bytes).unwrap_or_else(|x| die(&format!("height {h}: {}", x.0)));
        let out = e.apply_block(&Block { height: h, time: t, inputs }).unwrap_or_else(|x| die(&format!("height {h}: block refused {x:?}")));
        let ok = out.outcomes.iter().filter(|o| o.is_ok()).count();
        println!("{h} {} {} ok={ok}/{}", hex0x(&out.state_root), hex0x(&out.log_hash), out.outcomes.len());
        if let Some((r, at)) = &expect {
            if *at == h {
                if &hex0x(&out.state_root) != r {
                    die(&format!("state root mismatch at {h}: got {} expected {r}", hex0x(&out.state_root)));
                }
                matched = true;
            }
        }
        if let Err(m) = invariants::check(&e) {
            die(&format!("invariant broken at {h}: {m}"));
        }
    }
    if expect.is_some() && !matched {
        die("expected height not reached");
    }
}

fn demo(args: &[String]) {
    let dir = args.first().unwrap_or_else(|| die("usage: engine demo <out-dir> [blocks]"));
    let blocks: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(50);
    std::fs::create_dir_all(dir).unwrap_or_else(|x| die(&x.to_string()));
    let checked = genesis(5, vec![market(1, "BTC-USD")]);
    std::fs::write(format!("{dir}/genesis.json"), checked.g.encoding()).unwrap();
    let mut e = Engine::new(checked);
    let ok = oracle_keys(5);
    let keys: Vec<_> = (0..4).map(|i| key(100 + i)).collect();
    let mut f = std::fs::File::create(format!("{dir}/log.jsonl")).unwrap();
    let mut t = 1_794_880_000_000u64;
    let mut px = 6_725_000_000_000u64;
    for h in 1..=blocks {
        t += 1000;
        let mut inputs = vec![oracle_round(&e, &ok, t, &[(1, px)])];
        if h == 1 {
            for (i, k) in keys.iter().enumerate() {
                inputs.push(Input::Deposit { index: i as u64, account: addr(k), amount: 100_000_000_000 });
            }
        } else {
            let k = &keys[(h % 4) as usize];
            let side = h % 2 == 0;
            let price = if side { px - 10_000_000 * (h % 5) } else { px + 10_000_000 * (h % 5) };
            inputs.push(sign_action(&e, k, place(addr(k), t, 1, side, ORDER_LIMIT, TIF_GTC, price, 1_000_000 * (1 + h % 7), false)));
            let k2 = &keys[((h + 1) % 4) as usize];
            inputs.push(sign_action(&e, k2, place(addr(k2), t + 1, 1, !side, ORDER_MARKET, TIF_IOC, if side { 1_000_000_000 } else { 9_000_000_000_000 }, 500_000, false)));
        }
        px = if h % 3 == 0 { px + 50_000_000 } else { px - 30_000_000 };
        let enc = encode_inputs(&inputs);
        let out = e.apply_block(&Block { height: h, time: t, inputs }).unwrap();
        writeln!(f, "{{\"height\":\"{h}\",\"time\":\"{t}\",\"inputs\":\"{}\"}}", hex0x(&enc)).unwrap();
        if h == blocks {
            std::fs::write(format!("{dir}/expected-root.txt"), format!("{} {}\n", h, hex0x(&out.state_root))).unwrap();
        }
    }
    println!("wrote {dir}/genesis.json, {dir}/log.jsonl, {dir}/expected-root.txt");
}

fn bench(args: &[String]) {
    let n: u64 = args.first().and_then(|v| v.parse().ok()).unwrap_or(20_000);
    let mut e = Engine::new(genesis(5, vec![market(1, "BTC-USD")]));
    let ok = oracle_keys(5);
    let keys: Vec<_> = (0..16).map(|i| key(500 + i)).collect();
    let mut t = 1_794_880_000_000u64;
    let px = 6_725_000_000_000u64;
    let mut inputs = vec![oracle_round(&e, &ok, t + 1000, &[(1, px)])];
    for (i, k) in keys.iter().enumerate() {
        inputs.push(Input::Deposit { index: i as u64, account: addr(k), amount: 1_000_000_000_000 });
    }
    t += 1000;
    e.apply_block(&Block { height: 1, time: t, inputs }).unwrap();
    // pre-sign (signing is the client's cost, not the engine's)
    let per_block = 1000u64;
    let mut blocks = vec![];
    let mut h = 1;
    let mut i = 0u64;
    while i < n {
        h += 1;
        t += 200;
        let mut inputs = vec![];
        for _ in 0..per_block.min(n - i) {
            let k = &keys[(i % 16) as usize];
            let buy = i % 2 == 0;
            let off = 10_000_000 * (i % 20);
            let price = if buy { px - 100_000_000 + off } else { px + 100_000_000 - off };
            inputs.push(sign_action(&e, k, place(addr(k), t + (i / 16) % 25_000, 1, buy, ORDER_LIMIT, TIF_GTC, price, 100_000 * (1 + i % 5), false)));
            i += 1;
        }
        blocks.push(Block { height: h, time: t, inputs });
    }
    let start = Instant::now();
    let mut fills = 0usize;
    for b in &blocks {
        let out = e.apply_block(b).unwrap();
        fills += out.events.iter().filter(|x| matches!(x, Event::Fill { .. })).count();
    }
    let dt = start.elapsed();
    println!("bench: {n} signed limit orders in {} blocks, {fills} fills, {:.3} s, {:.0} orders/s (includes signature recovery and a full state root per block)", blocks.len(), dt.as_secs_f64(), n as f64 / dt.as_secs_f64());
    std::io::stdout().flush().ok();
}

fn vectors() {
    use postern_perps_engine::eip712::{digest, oracle_struct_hash, sign_digest, struct_hash};
    use serde_json::json;
    let e = Engine::new(genesis(5, vec![market(1, "BTC-USD")]));
    let k = key(1);
    let a = addr(&k);
    let ag = addr(&key(2));
    let t = 1_794_880_000_000u64;
    let h = |x: &[u8]| hex0x(x);
    let mut acts: Vec<(Action, serde_json::Value)> = vec![];
    let po = PlaceOrder { account: a, nonce: t, market_id: 1, is_buy: true, order_type: ORDER_LIMIT, time_in_force: TIF_GTC, price: 6_725_000_000_000, size: 50_000_000, reduce_only: false, trigger_price: 0, client_order_id: [7u8; 32] };
    acts.push((Action::PlaceOrder(po), json!({"account": h(&a), "nonce": t.to_string(), "marketId": "1", "isBuy": true, "orderType": "0", "timeInForce": "0", "price": "6725000000000", "size": "50000000", "reduceOnly": false, "triggerPrice": "0", "clientOrderId": h(&[7u8; 32])})));
    acts.push((Action::CancelOrder { account: a, nonce: t + 1, market_id: 1, order_id: 42 }, json!({"account": h(&a), "nonce": (t + 1).to_string(), "marketId": "1", "orderId": "42"})));
    acts.push((Action::CancelAll { account: a, nonce: t + 2, market_id: 0 }, json!({"account": h(&a), "nonce": (t + 2).to_string(), "marketId": "0"})));
    acts.push((Action::UpdateLeverage { account: a, nonce: t + 3, market_id: 1, leverage: 20, isolated: false }, json!({"account": h(&a), "nonce": (t + 3).to_string(), "marketId": "1", "leverage": "20", "isolated": false})));
    acts.push((Action::UpdateMargin { account: a, nonce: t + 4, market_id: 1, amount: 5_000_000, add: true }, json!({"account": h(&a), "nonce": (t + 4).to_string(), "marketId": "1", "amount": "5000000", "add": true})));
    acts.push((Action::Withdraw { account: a, nonce: t + 5, amount: 100_000_000, recipient: ag }, json!({"account": h(&a), "nonce": (t + 5).to_string(), "amount": "100000000", "recipient": h(&ag)})));
    acts.push((Action::ApproveAgent { account: a, nonce: t + 6, agent: ag, name: "desk bot".into(), expires_at: t + 86_400_000 }, json!({"account": h(&a), "nonce": (t + 6).to_string(), "agent": h(&ag), "name": "desk bot", "expiresAt": (t + 86_400_000).to_string()})));
    acts.push((Action::RevokeAgent { account: a, nonce: t + 7, agent: ag }, json!({"account": h(&a), "nonce": (t + 7).to_string(), "agent": h(&ag)})));
    let names = ["PlaceOrder", "CancelOrder", "CancelAll", "UpdateLeverage", "UpdateMargin", "Withdraw", "ApproveAgent", "RevokeAgent"];
    let mut out = vec![];
    for (i, (act, msg)) in acts.into_iter().enumerate() {
        let sh = struct_hash(&act);
        let d = digest(&e.domain_sep, &sh);
        let sig = sign_digest(&k, &d);
        let wire = postern_perps_engine::codec::encode_input(&Input::Action(SignedAction { action: act, signature: sig }));
        out.push(json!({"primaryType": names[i], "message": msg, "structHash": h(&sh), "digest": h(&d), "signer": h(&a), "signature": h(&sig), "inputBytes": h(&wire)}));
    }
    let ps = vec![OraclePrice { market_id: 1, price: 6_725_000_000_000, sources: 5 }, OraclePrice { market_id: 2, price: 348_000_000_000, sources: 4 }];
    let round = t / 3000;
    let osh = oracle_struct_hash(round, round * 3000 + 17, &ps);
    let od = digest(&e.domain_sep, &osh);
    let ok = oracle_keys(5);
    let osig = sign_digest(&ok[0], &od);
    let v = json!({
        "_note": "UNAUDITED prototype test vectors generated by `engine vectors` (SPEC 0.1). Throwaway keys: key(n) = keccak256(\"postern-perps throwaway test key n\").",
        "domain": {"name": "PosternPerps", "version": "1", "chainId": "700770", "verifyingContract": DEV_VAULT},
        "domainSeparator": h(&e.domain_sep),
        "actions": out,
        "oracle": {"round": round.to_string(), "timeMs": (round * 3000 + 17).to_string(), "prices": [{"marketId": "1", "price": "6725000000000", "sources": "5"}, {"marketId": "2", "price": "348000000000", "sources": "4"}], "structHash": h(&osh), "digest": h(&od), "signer": h(&addr(&ok[0])), "signature": h(&osig)}
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("replay") => replay(&args[2..]),
        Some("demo") => demo(&args[2..]),
        Some("bench") => bench(&args[2..]),
        Some("vectors") => vectors(),
        _ => die("usage: engine replay|demo|bench …"),
    }
}
