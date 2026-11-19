//! The `perps-oracle` binary end to end, offline: run-time dev committee, tape check, a replay feed into a
//! loopback sink, refusals. Round 2 (agent 432). Throwaway keys only; ports 53220-53229.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_perps-oracle"))
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("a432-cli-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

#[test]
fn committee_gen_tape_check_and_replay_feed_into_a_loopback_sink() {
    let dir = tmp("keys");
    let out = bin()
        .args(["committee-gen", "--n", "7", "--quorum", "5", "--out"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let c: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(c["signers"].as_array().unwrap().len(), 7);
    // Key material never reaches stdout or stderr.
    let key0 = fs::read_to_string(dir.join("oracle-0.key")).unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains(key0.trim()));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(key0.trim()));
    // The directory is never reused.
    assert!(!bin()
        .args(["committee-gen", "--n", "7", "--quorum", "5", "--out"])
        .arg(&dir)
        .status()
        .unwrap()
        .success());

    let tape =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tapes/rehearsal-example.jsonl");
    let out = bin()
        .args(["tape-check", "--tape"])
        .arg(&tape)
        .output()
        .unwrap();
    assert!(out.status.success());
    let t: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (t["rounds"].as_u64(), t["silent"].as_u64()),
        (Some(85), Some(5))
    );
    assert_eq!(t["markets"][0]["low"], "59180");

    // A token file others can read is refused before anything is sent.
    let tok = dir.join("token");
    fs::write(&tok, "throwaway-cli-token-0123456789\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tok, fs::Permissions::from_mode(0o644)).unwrap();
        let o = bin()
            .args([
                "replay-feed",
                "--vault",
                "0x00000000000000000000000000000000000000d1",
                "--rounds",
                "1",
                "--node",
                "http://127.0.0.1:53220",
                "--tape",
            ])
            .arg(&tape)
            .arg("--key-dir")
            .arg(&dir)
            .arg("--token-file")
            .arg(&tok)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&o.stderr).contains("token file: mode 644"));
        fs::set_permissions(&tok, fs::Permissions::from_mode(0o600)).unwrap();
    }
    // A node URL that is not loopback is refused.
    let o = bin()
        .args([
            "replay-feed",
            "--vault",
            "0x00000000000000000000000000000000000000d1",
            "--rounds",
            "1",
            "--node",
            "http://203.0.113.7:53220",
            "--tape",
        ])
        .arg(&tape)
        .arg("--key-dir")
        .arg(&dir)
        .arg("--token-file")
        .arg(&tok)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("not a loopback address"));

    // Two real rounds (about 6 s of wall clock) into a sink that accepts both.
    let l = TcpListener::bind("127.0.0.1:53220").unwrap();
    let sink = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for _ in 0..2 {
            let (mut s, _) = l.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = s.read(&mut buf).unwrap();
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).into_owned();
                if let Some((h, b)) = text.split_once("\r\n\r\n") {
                    let len: usize = h
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if b.len() >= len {
                        assert!(h.contains("Authorization: Bearer throwaway-cli-token-0123456789"));
                        bodies.push(b.to_string());
                        break;
                    }
                }
            }
            s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
                .unwrap();
        }
        bodies
    });
    let log = dir.join("feed.jsonl");
    let o = bin()
        .args([
            "replay-feed",
            "--vault",
            "0x00000000000000000000000000000000000000d1",
            "--rounds",
            "2",
            "--strict",
            "--node",
            "http://127.0.0.1:53220",
            "--tape",
        ])
        .arg(&tape)
        .arg("--key-dir")
        .arg(&dir)
        .arg("--token-file")
        .arg(&tok)
        .arg("--out")
        .arg(&log)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("\"accepted\":2") && err.contains("\"unreachable\":0"),
        "{err}"
    );
    assert!(!err.contains("throwaway-cli-token"));
    let bodies = sink.join().unwrap();
    let rounds: Vec<u64> = bodies
        .iter()
        .map(|b| {
            serde_json::from_str::<Value>(b).unwrap()["round"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(rounds[1], rounds[0] + 1, "consecutive engine rounds");
    let now_round = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        / 3000;
    assert!(
        now_round - rounds[1] <= 1,
        "signed in the current round of the wall clock"
    );
    let first: Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(first["observations"].as_array().unwrap().len(), 7);
    assert_eq!(
        first["observations"][0]["prices"][0]["price"],
        "6725000000000"
    );
    let logged = fs::read_to_string(&log).unwrap();
    assert_eq!(logged.lines().count(), 2);
    assert!(!logged.contains("throwaway-cli-token"));

    // --strict with nobody listening: exit 2, and the run still went to its end.
    let o = bin()
        .args([
            "replay-feed",
            "--vault",
            "0x00000000000000000000000000000000000000d1",
            "--rounds",
            "1",
            "--strict",
            "--node",
            "http://127.0.0.1:53221",
            "--tape",
        ])
        .arg(&tape)
        .arg("--key-dir")
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("\"unreachable\":1"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tape_from_the_recorded_rounds_is_the_pipeline_output() {
    let rounds = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/rounds");
    let out = bin()
        .args(["tape", "--recording"])
        .arg(&rounds)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let steps = perps_oracle_feeder::tape::parse(&text).unwrap();
    assert_eq!(steps.len(), 6);
    // Row for row the engine-scale golden file.
    let golden = fs::read_to_string(rounds.join("expected-1e8.jsonl")).unwrap();
    for (s, g) in steps.iter().zip(golden.lines()) {
        let g: Value = serde_json::from_str(g).unwrap();
        for p in g["prices"].as_array().unwrap() {
            let (price, sources) = s.prices[&(p["marketId"].as_u64().unwrap() as u32)];
            assert_eq!(price.to_string(), p["price"].as_str().unwrap());
            assert_eq!(u64::from(sources), p["sources"].as_u64().unwrap());
        }
    }
    // --every 2 keeps rows 0, 2, 4.
    let out = bin()
        .args(["tape", "--every", "2", "--recording"])
        .arg(&rounds)
        .output()
        .unwrap();
    let thin = perps_oracle_feeder::tape::parse(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(
        thin,
        vec![steps[0].clone(), steps[2].clone(), steps[4].clone()]
    );
}
