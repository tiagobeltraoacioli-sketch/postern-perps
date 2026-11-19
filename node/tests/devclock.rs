//! The DEV engine clock (435's ask: a rehearsal must be able to cross a UTC day, where the engine sweeps fees).
//! Gate, three ways, as for the dev oracle (R393): the `dev-clock` cargo feature (a release build refuses to
//! compile it), dev mode only at start, and the validators' block-time rules (R395-29, R395-37/-38) unchanged:
//! they are applied to the validator's own engine clock, so a validator whose offset differs from the
//! sequencer's signs nothing. THROWAWAY keys only. UNAUDITED PROTOTYPE.
//!
//! Without the feature (the default build, and every release build) only the first test exists; the others need
//! `--features dev-clock`.

mod common;

use common::*;
use perps_node::node::{EngineClock, Mode};

/// The feature off: no offset can be made, by the library or by either command of the binary.
#[cfg(not(feature = "dev-clock"))]
#[test]
fn the_dev_clock_is_refused_by_a_binary_built_without_the_feature() {
    use std::process::Command;
    for mode in [Mode::Dev, Mode::Live] {
        let e = EngineClock::dev_offset(mode, 1).unwrap_err();
        assert!(e.contains("without the dev-clock feature"), "{e}");
        assert_eq!(EngineClock::dev_offset(mode, 0).unwrap(), EngineClock::wall(), "no offset is the wall clock");
    }
    assert_eq!(EngineClock::wall().offset_ms(), 0);
    let bin = env!("CARGO_BIN_EXE_postern-perps-node");
    let dir = tmpdir("devclock-off");
    let d = dir.to_str().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let listen = format!("127.0.0.1:{port}");
    let runs: [Vec<&str>; 2] = [
        vec!["dev", "--data", d, "--listen", &listen, "--seed", "devclock-off", "--dev-clock-offset-ms", "86400000"],
        vec!["validator", "--index", "4", "--seed", "devclock-off", "--sequencer", "http://127.0.0.1:9", "--dev-clock-offset-ms", "86400000"],
    ];
    for args in runs {
        let out = Command::new(bin).args(&args).output().expect("run the node binary");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(err.contains("--dev-clock-offset-ms refused") && err.contains("without the dev-clock feature"), "{args:?}: {err}");
    }
}

#[cfg(feature = "dev-clock")]
mod with_feature {
    use super::*;
    use perps_node::engine::stub::StubEngine;
    use perps_node::engine::Engine;
    use perps_node::geo::GeoPolicy;
    use perps_node::node::{run_producer, run_remote_validator_clocked, spawn_local_validators, App, DEV_CLOCK_MAX_OFFSET_MS};
    use perps_node::sequencer::Sequencer;
    use perps_node::sig::key_address;
    use perps_node::util::to_hex;
    use perps_node::validator::{VResult, Validator};
    use perps_node::watcher::http_call;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    const DAY: u64 = 86_400_000;

    /// Start-time gate: dev mode only (and a bounded, forward offset).
    #[test]
    fn the_dev_clock_is_refused_outside_dev_mode() {
        let e = EngineClock::dev_offset(Mode::Live, DAY).unwrap_err();
        assert!(e.contains("outside dev mode"), "{e}");
        assert_eq!(EngineClock::dev_offset(Mode::Live, 0).unwrap(), EngineClock::wall(), "live runs on the wall clock");
        assert!(EngineClock::dev_offset(Mode::Dev, DEV_CLOCK_MAX_OFFSET_MS + 1).is_err());
        let c = EngineClock::dev_offset(Mode::Dev, DAY).unwrap();
        assert_eq!(c.offset_ms(), DAY);
        let (w, s) = (now(), c.now());
        assert!(s >= w + DAY && s < w + DAY + 5_000, "wall {w} shifted {s}");
        // a refusal at the command line stops the process: a number that is not one, and a negative offset
        let bin = env!("CARGO_BIN_EXE_postern-perps-node");
        for bad in ["-1000", "1d", ""] {
            let out = std::process::Command::new(bin).args(["validator", "--index", "4", "--seed", "devclock-gate", "--sequencer", "http://127.0.0.1:9", "--dev-clock-offset-ms", bad]).output().unwrap();
            assert_ne!(out.status.code(), Some(0), "{bad:?}");
            assert!(String::from_utf8_lossy(&out.stderr).contains("--dev-clock-offset-ms"), "{bad:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }

    /// The validators' rules are the same rules on the shifted clock: with the sequencer's offset a block is
    /// signed; R395-29 (first seen too late) and the future bound still refuse relative to that clock; R395-37
    /// still holds; and a validator with ANOTHER offset (none, or a larger one) signs nothing.
    #[test]
    fn validators_apply_their_time_rules_to_the_shifted_clock_and_a_different_offset_signs_nothing() {
        let clock = EngineClock::dev_offset(Mode::Dev, DAY + 3 * 3_600_000).unwrap();
        let ua = key_address(&user(50));

        // same offset everywhere: signed by all seven, final
        let mut n = net("devclock-same", 1);
        n.deposit_everywhere(deposit(0, ua, 1_000_000));
        let b = n.seq.produce(clock.now()).unwrap().unwrap().block.as_ref().clone();
        assert!(b.header.time >= now() + DAY, "the block carries the shifted time");
        for v in n.vals.iter_mut() {
            match v.process(&b, clock.now()) {
                VResult::Applied { checkpoint: Some((body, sig)) } => {
                    n.seq.add_signature(body.height, sig).unwrap();
                }
                r => panic!("an honest block under the shared offset was not signed: {r:?}"),
            }
        }
        assert_eq!(n.seq.latest_final(), Some(1));

        // R395-29 on the shifted clock: the sequencer runs 60 s behind the shared clock -> not signed
        let mut l = net("devclock-late", 1);
        l.deposit_everywhere(deposit(0, ua, 1_000_000));
        let late = l.seq.produce(clock.now() - 60_000).unwrap().unwrap().block.as_ref().clone();
        assert!(late.header.time < clock.now() - 50_000 && late.header.time > now() + DAY / 2, "60 s behind the SHIFTED clock, still far ahead of the wall clock");
        for v in l.vals.iter_mut() {
            match v.process(&late, clock.now()) {
                VResult::AppliedNotSigned(r) => assert!(r.contains("R395-29"), "{r}"),
                r => panic!("a late block was certified under the offset: {r:?}"),
            }
        }
        assert_eq!(l.seq.latest_final(), None);
        // the future bound on the shifted clock: 60 s ahead of the shared clock -> wait, nothing applied
        let mut f = net("devclock-future", 1);
        f.deposit_everywhere(deposit(0, ua, 1));
        let ahead = f.seq.produce(clock.now() + 60_000).unwrap().unwrap().block.as_ref().clone();
        for v in f.vals.iter_mut() {
            assert!(matches!(v.process(&ahead, clock.now()), VResult::Wait(_)));
            assert_eq!(v.height, 0);
        }

        // R395-37 on the shifted clock: waiting for the own L2 view does not make the block late
        let mut w = net("devclock-37", 1);
        w.seq_view.lock().unwrap().deposits.insert(0, deposit(0, ua, 1_000_000));
        let b = w.seq.produce(clock.now()).unwrap().unwrap().block.as_ref().clone();
        let t0 = clock.now();
        for v in w.vals.iter_mut() {
            assert!(matches!(v.process(&b, t0), VResult::Wait(_)));
        }
        for v in &w.val_views {
            v.lock().unwrap().deposits.insert(0, deposit(0, ua, 1_000_000));
        }
        for v in w.vals.iter_mut() {
            assert!(matches!(v.process(&b, t0 + 3_000), VResult::Applied { checkpoint: Some(_) }), "first seen in time on the shifted clock");
        }

        // DIFFERENT offsets: the sequencer is shifted, the validators are not (or are shifted further)
        let mut m = net("devclock-mismatch", 1);
        m.deposit_everywhere(deposit(0, ua, 1_000_000));
        let b = m.seq.produce(clock.now()).unwrap().unwrap().block.as_ref().clone();
        let wall = EngineClock::wall();
        let further = EngineClock::dev_offset(Mode::Dev, 2 * DAY + 3 * 3_600_000).unwrap();
        let nearly = EngineClock::dev_offset(Mode::Dev, clock.offset_ms() + 10_000).unwrap();
        for (i, v) in m.vals.iter_mut().enumerate() {
            match i % 3 {
                // no offset: the block is a day in the future -> not applied, not signed
                0 => {
                    assert!(matches!(v.process(&b, wall.now()), VResult::Wait(_)), "validator {i} on the wall clock");
                    assert_eq!(v.height, 0);
                }
                // a larger offset: the block is a day late -> not signed (R395-29)
                1 => match v.process(&b, further.now()) {
                    VResult::AppliedNotSigned(r) => assert!(r.contains("R395-29"), "{r}"),
                    r => panic!("validator {i} with a larger offset signed: {r:?}"),
                },
                // even 10 s more than the sequencer's is beyond the lag bound
                _ => match v.process(&b, nearly.now()) {
                    VResult::AppliedNotSigned(r) => assert!(r.contains("R395-29"), "{r}"),
                    r => panic!("validator {i} with an offset 10 s larger signed: {r:?}"),
                },
            }
        }
        assert_eq!(m.seq.latest_final(), None, "no validator with another offset signed");
    }

    struct Node {
        url: String,
        app: Arc<App>,
        stop: tokio::sync::watch::Sender<bool>,
        g: Arc<perps_node::genesis::Genesis>,
        keys: perps_node::dev::DevKeys,
    }

    fn geo() -> GeoPolicy {
        GeoPolicy::from_json(
            &serde_json::json!({"version": "test", "unknown_country_effect": "deny",
                "tiers": {"allowed": {"effect": "allow"}, "blocked": {"effect": "deny"}},
                "countries": {"PA": {"tier": "allowed"}}, "denied_policy": {"policy": "exit_only"}}),
            true,
        )
        .unwrap()
    }

    async fn serve(app: Arc<App>) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let r = perps_node::api::router(app);
        tokio::spawn(async move { axum::serve(l, r.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
        format!("http://{addr}")
    }

    async fn get_json(url: &str) -> (u16, serde_json::Value) {
        let (s, b) = http_call("GET", url, None, &[]).await.unwrap();
        (s, serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null))
    }

    async fn post_json(url: &str, body: &serde_json::Value, h: &[(&str, &str)]) -> (u16, serde_json::Value) {
        let (s, b) = http_call("POST", url, Some(&body.to_string()), h).await.unwrap();
        (s, serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null))
    }

    /// A REMOTE validator compares its offset with the one the sequencer announces before it follows: with
    /// another offset it ends with an error and signs nothing (4 local signatures stay below the threshold of
    /// 5); with the same offset its signature makes the checkpoint final.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_remote_validator_with_another_offset_refuses_to_follow_and_signs_nothing() {
        let offset = 5 * DAY + 7_000;
        let clock = EngineClock::dev_offset(Mode::Dev, offset).unwrap();
        let (g, keys) = perps_node::dev::dev_genesis("devclock-remote", 7, 5, 1, 0);
        let sv = view();
        let seq = Sequencer::open(g.clone(), keys.sequencer.clone(), Box::new(StubEngine::new(g.clone())), tmpdir("devclock-remote"), sv.clone()).unwrap();
        let app = App::new(g.clone(), seq, geo(), Mode::Dev, sv.clone(), 50, false);
        *app.clock.lock().unwrap() = clock;
        let (tx, rx) = tokio::sync::watch::channel(false);
        // four of the seven validators in process (they share the node's clock): one short of the threshold
        let engines: Vec<Box<dyn Engine>> = (0..4).map(|_| Box::new(StubEngine::new(g.clone())) as Box<dyn Engine>).collect();
        spawn_local_validators(&app, keys.validators[..4].to_vec(), engines, (0..4).map(|_| sv.clone()).collect(), &rx).unwrap();
        tokio::spawn(run_producer(app.clone(), rx.clone()));
        let url = serve(app.clone()).await;
        // the node announces its offset and serves engine time
        let (_, ni) = get_json(&format!("{url}/v1/node")).await;
        assert_eq!(ni["engineClock"]["devOffsetMs"].as_u64(), Some(offset), "{ni}");
        let (_, st) = get_json(&format!("{url}/v1/status")).await;
        let t = st["time"].as_u64().unwrap();
        assert!(t >= now() + offset - 5_000 && t <= now() + offset + 5_000, "status.time is engine time: {t}");
        sv.lock().unwrap().deposits.insert(0, deposit(0, key_address(&user(1)), 1_000_000));
        for _ in 0..100 {
            if app.seq.lock().unwrap().log.last_height >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(app.seq.lock().unwrap().log.last_height >= 1);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(app.seq.lock().unwrap().latest_final(), None, "four signatures are not five");

        let mk = |i: usize| Validator::new(g.clone(), keys.validators[i].clone(), Box::new(StubEngine::new(g.clone())), sv.clone(), None).unwrap();
        // no offset, a smaller one, a larger one: each ends at once with the mismatch and signs nothing
        for (i, other) in [(4usize, 0u64), (5, offset - 1), (6, offset + DAY)] {
            let c = EngineClock::dev_offset(Mode::Dev, other).unwrap();
            let r = tokio::time::timeout(Duration::from_secs(10), run_remote_validator_clocked(url.clone(), mk(i), rx.clone(), None, c)).await.expect("a mismatch ends the validator");
            let e = r.unwrap_err();
            assert!(e.contains("ENGINE CLOCK MISMATCH") && e.contains(&offset.to_string()) && e.contains(&other.to_string()), "{e}");
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(app.seq.lock().unwrap().latest_final(), None, "no validator with another offset signed");
        // the same offset: it follows, signs, and the checkpoint is final
        let same = tokio::spawn(run_remote_validator_clocked(url.clone(), mk(4), rx.clone(), None, clock));
        let mut fin = None;
        for _ in 0..100 {
            fin = app.seq.lock().unwrap().latest_final();
            if fin.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(fin.is_some(), "the fifth signature, by the validator with the sequencer's offset");
        assert!(!same.is_finished());
        // the sequencer is then given another offset (as after a restart): the validator that keeps the old
        // one notices within seconds and stops
        *app.clock.lock().unwrap() = EngineClock::dev_offset(Mode::Dev, offset + DAY).unwrap();
        let e = tokio::time::timeout(Duration::from_secs(15), same).await.expect("the validator notices the new offset").unwrap().unwrap_err();
        assert!(e.contains("ENGINE CLOCK MISMATCH"), "{e}");
        tx.send(true).unwrap();
    }

    const ORACLE_TOKEN: &str = "test-oracle-ingest-token-0123456789";
    const P: u64 = 6_725_000_000_000;

    fn observation_json(g: &perps_node::genesis::Genesis, key: &k256::ecdsa::SigningKey, signer: u8, t: u64, prices: &[(u32, u64)]) -> serde_json::Value {
        let mut o = perps_node::types::SignedObservation {
            signer,
            time_ms: t,
            prices: prices.iter().map(|(m, p)| perps_node::types::OraclePrice { market_id: *m, price: *p, sources: 5 }).collect(),
            signature: [0u8; 65],
        };
        o.signature = perps_node::sig::sign(key, &perps_node::oracle_pool::observation_digest(g, t / 3000, &o));
        serde_json::json!({"signer": signer, "timeMs": t, "prices": prices.iter().map(|(m, p)| serde_json::json!({"marketId": m, "price": p.to_string(), "sources": 5})).collect::<Vec<_>>(), "signature": to_hex(&o.signature)})
    }

    fn order_req(g: &perps_node::genesis::Genesis, key: &k256::ecdsa::SigningKey, nonce: u64, is_buy: bool, tif: u8, price: u64, size: u64) -> serde_json::Value {
        let a = signed(
            g,
            key,
            "PlaceOrder",
            serde_json::json!({"account": to_hex(&key_address(key)), "nonce": nonce.to_string(), "marketId": 1, "isBuy": is_buy, "orderType": 0,
                "timeInForce": tif, "price": price.to_string(), "size": size.to_string(), "reduceOnly": false, "triggerPrice": "0",
                "clientOrderId": to_hex(&[0u8; 32])}),
        );
        request(g, &a)
    }

    async fn real_node(name: &str, clock: EngineClock) -> Node {
        let (g, keys, checked) = perps_node::dev::dev_genesis_real(&format!("devclock-{name}"), 7, 5, 5, 0);
        let sv = view();
        let mk = || -> Box<dyn Engine> { Box::new(perps_node::engine::real::RealEngine::new(g.clone(), checked.clone()).unwrap()) };
        let seq = Sequencer::open(g.clone(), keys.sequencer.clone(), mk(), tmpdir(&format!("devclock-{name}")), sv.clone()).unwrap();
        let app = App::with(g.clone(), seq, geo(), Mode::Dev, sv.clone(), 50, false, [0x11; 20], Some(ORACLE_TOKEN.as_bytes().to_vec()));
        *app.clock.lock().unwrap() = clock;
        let (tx, rx) = tokio::sync::watch::channel(false);
        let engines: Vec<Box<dyn Engine>> = (0..7).map(|_| mk()).collect();
        spawn_local_validators(&app, keys.validators.clone(), engines, (0..7).map(|_| sv.clone()).collect(), &rx).unwrap();
        tokio::spawn(run_producer(app.clone(), rx.clone()));
        let url = serve(app.clone()).await;
        Node { url, app, stop: tx, g, keys }
    }

    /// 435's rehearsal need: under the offset a block sequence crosses 00:00 UTC within seconds and the engine's
    /// daily fee sweep fires: the protocol fee balance becomes a withdrawal to the fee recipient in the first
    /// block of the new day, and the seven validators (same clock) keep signing across the boundary.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn blocks_crossing_midnight_utc_under_the_offset_trigger_the_daily_fee_sweep() {
        // engine time starts 30 s before the next UTC midnight after tomorrow's (any day boundary will do)
        let wall = now();
        let midnight = (wall / DAY + 2) * DAY;
        let clock = EngineClock::dev_offset(Mode::Dev, midnight - 30_000 - wall).unwrap();
        let n = real_node("sweep", clock).await;
        let (g, base, app) = (n.g.clone(), n.url.clone(), n.app.clone());
        let day0 = app.engine_now() / DAY;
        assert_eq!(day0 + 1, midnight / DAY);
        let (a, b) = (user(61), user(62));
        {
            let mut v = app.view.lock().unwrap();
            v.deposits.insert(0, deposit(0, key_address(&a), 1_000_000_000_000));
            v.deposits.insert(1, deposit(1, key_address(&b), 1_000_000_000_000));
        }
        // the feeder stand-in signs the round of ENGINE time (a feeder on the wall clock would be a day stale)
        let (fstop_tx, mut fstop) = tokio::sync::watch::channel(false);
        let feeder = {
            let (base, g, app, keys) = (base.clone(), g.clone(), app.clone(), n.keys.oracle.clone());
            tokio::spawn(async move {
                let auth = format!("Bearer {ORACLE_TOKEN}");
                let mut last_round = 0;
                loop {
                    let t = app.engine_now();
                    if t / 3000 > last_round {
                        last_round = t / 3000;
                        let obs: Vec<serde_json::Value> = keys.iter().enumerate().map(|(i, k)| observation_json(&g, k, i as u8, t, &[(1, P)])).collect();
                        let (s, j) = post_json(&format!("{base}/v1/oracle/updates"), &serde_json::json!({"round": last_round, "observations": obs}), &[("Authorization", &auth)]).await;
                        assert_eq!(s, 200, "{j}");
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                        _ = fstop.changed() => return,
                    }
                }
            })
        };
        let mut active = false;
        for _ in 0..140 {
            let (_, m) = get_json(&format!("{base}/v1/markets/BTC-USD")).await;
            if m["status"] == "active" {
                active = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(active, "BTC-USD did not become active from rounds signed on the engine clock");
        assert_eq!(app.engine_now() / DAY, day0, "still the day before: the market became active in under 30 s");

        // nonces are judged on the engine clock: a wall-clock nonce is a day old
        let (s, e) = post_json(&format!("{base}/v1/actions"), &order_req(&g, &a, now(), false, 0, P, 400_000_000), &[("X-Mock-Country", "PA")]).await;
        assert_eq!(s, 400, "a wall-clock nonce under the offset: {e}");
        // the trade (4 BTC at 67,250): maker and taker fees go to the protocol fee account
        let t = app.engine_now();
        let (s, ack) = post_json(&format!("{base}/v1/actions?wait=included"), &order_req(&g, &a, t, false, 0, P, 400_000_000), &[("X-Mock-Country", "PA")]).await;
        assert_eq!((s, ack["result"]["ok"].as_bool()), (200, Some(true)), "{ack}");
        let (s, ack) = post_json(&format!("{base}/v1/actions?wait=included"), &order_req(&g, &b, t + 1, true, 1, P, 400_000_000), &[("X-Mock-Country", "PA")]).await;
        assert_eq!((s, ack["result"]["ok"].as_bool(), ack["result"]["orderStatus"].as_str()), (200, Some(true), Some("filled")), "{ack}");

        use postern_perps_engine::engine::ACCT_PROTOCOL_FEE;
        let read = |app: &Arc<App>| {
            let s = app.seq.lock().unwrap();
            let e = s.engine.real().unwrap();
            (e.time, e.height, e.accounts[ACCT_PROTOCOL_FEE as usize].balance, e.withdrawals.clone(), e.g.fee_recipient, e.g.g.min_fee_sweep, e.accounts[ACCT_PROTOCOL_FEE as usize].address)
        };
        let (t_before, _, fees, w_before, fee_recipient, min_sweep, fee_account) = read(&app);
        assert_eq!(t_before / DAY, day0, "the trade happened before midnight");
        assert!(fees >= min_sweep as i128 && fees > 0, "fees {fees} reach the sweep minimum {min_sweep}");
        assert!(w_before.is_empty(), "nothing was withdrawn or swept yet: {w_before:?}");

        // engine time crosses 00:00 UTC (the wall clock is nowhere near midnight + offset by accident: the
        // boundary is reached only through the offset)
        let mut after = read(&app);
        for _ in 0..400 {
            if after.0 / DAY > day0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            after = read(&app);
        }
        let (t_after, _, fees_after, w_after, ..) = after;
        assert_eq!(t_after / DAY, day0 + 1, "a block of the new UTC day was applied");
        assert_eq!(fees_after, 0, "the protocol fee balance was swept");
        assert_eq!(w_after.len(), 1, "{w_after:?}");
        let sweep = &w_after[0];
        assert_eq!((sweep.account, sweep.recipient, sweep.amount as i128), (fee_account, fee_recipient, fees), "the whole balance, to the fee recipient");
        // it happened in the FIRST block of the new day, and the block before it is of the old day
        let (first_new, last_old) = {
            let s = app.seq.lock().unwrap();
            (s.block(sweep.height).unwrap().unwrap().header.time, s.block(sweep.height - 1).unwrap().unwrap().header.time)
        };
        assert_eq!((first_new / DAY, last_old / DAY), (day0 + 1, day0), "sweep at height {}", sweep.height);
        assert!(first_new >= midnight && first_new < midnight + 10_000);
        // the validators, on the same clock, sign across the boundary: a checkpoint at or after the sweep is final
        let mut fin = None;
        for _ in 0..150 {
            fin = app.seq.lock().unwrap().latest_final();
            if fin.is_some_and(|h| h >= sweep.height) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(fin.is_some_and(|h| h >= sweep.height), "final {fin:?}, sweep at {}", sweep.height);
        let _ = fstop_tx.send(true);
        let _ = feeder.await;
        let _ = n.stop.send(true);
    }
}
