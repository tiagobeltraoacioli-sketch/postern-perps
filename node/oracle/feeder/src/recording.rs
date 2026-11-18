//! Recorded venue rounds: the two on-disk forms and their loader. UNAUDITED PROTOTYPE.
//!
//! - Directory form (round 1 fixtures, `fixtures/rounds/round-<r>/<source>.json`): one file per source,
//!   `{"venue","url","sentMs","receivedMs","curlExit"|"error","body"}`.
//! - Line form (`record --out FILE.jsonl`, `run --record FILE.jsonl`): one JSON object per round,
//!   `{"round","sentMs","fetches":[{"url","sources":[..],"status","receivedMs","httpDateMs","error","body"}]}`;
//!   one entry per REQUEST, so a batched request (Kraken) appears once and names every source that reads its body;
//!   a fetch unit skipped by the drop rule appears as `{"url","sources","dropped":true}`.
//!
//! Both replay through the same pipeline: the recorded bytes are what the adapters parse.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::pipeline::Response;

fn read_json(p: &Path) -> Result<Value, String> {
    let t = fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))
}

/// A round in the directory form. The sampling time is the earliest `sentMs`.
pub fn load_round_dir(dir: &Path) -> Result<(u64, Vec<Response>), String> {
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
        let failed = v
            .get("curlExit")
            .and_then(Value::as_i64)
            .is_some_and(|c| c != 0)
            || v.get("error").is_some_and(|e| !e.is_null());
        out.push(Response {
            source: g("venue")?.as_str().ok_or("venue")?.to_string(),
            body: if failed {
                None
            } else {
                Some(g("body")?.as_str().ok_or("body")?.to_string())
            },
            received_ms: g("receivedMs")?.as_u64().ok_or("receivedMs")?,
            error: if failed {
                Some(
                    v.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("fetch failed")
                        .to_string(),
                )
            } else {
                None
            },
        });
    }
    if out.is_empty() {
        return Err(format!("{}: no recorded responses", dir.display()));
    }
    Ok((t0, out))
}

/// One request of a round, as fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetch {
    pub url: String,
    pub sources: Vec<String>,
    /// HTTP status; 0 = no HTTP response (timeout, TLS, connection).
    pub status: u16,
    pub received_ms: u64,
    pub http_date_ms: Option<u64>,
    pub error: Option<String>,
    pub body: Option<String>,
    /// Not requested this round (drop rule D2).
    pub dropped: bool,
}

impl Fetch {
    pub fn to_json(&self) -> Value {
        if self.dropped {
            return json!({"url": self.url, "sources": self.sources, "dropped": true});
        }
        json!({"url": self.url, "sources": self.sources, "status": self.status, "receivedMs": self.received_ms,
               "httpDateMs": self.http_date_ms, "error": self.error, "body": self.body})
    }

    pub fn from_json(v: &Value) -> Result<Fetch, String> {
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or(format!("fetch: {k}"))
        };
        let sources = v
            .get("sources")
            .and_then(Value::as_array)
            .ok_or("fetch: sources")?
            .iter()
            .map(|x| x.as_str().map(str::to_string).ok_or("fetch: source name"))
            .collect::<Result<Vec<_>, _>>()?;
        if v.get("dropped").and_then(Value::as_bool) == Some(true) {
            return Ok(Fetch {
                url: s("url")?,
                sources,
                status: 0,
                received_ms: 0,
                http_date_ms: None,
                error: None,
                body: None,
                dropped: true,
            });
        }
        Ok(Fetch {
            url: s("url")?,
            sources,
            status: v
                .get("status")
                .and_then(Value::as_u64)
                .and_then(|x| u16::try_from(x).ok())
                .ok_or("fetch: status")?,
            received_ms: v
                .get("receivedMs")
                .and_then(Value::as_u64)
                .ok_or("fetch: receivedMs")?,
            http_date_ms: v.get("httpDateMs").and_then(Value::as_u64),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
            body: v.get("body").and_then(Value::as_str).map(str::to_string),
            dropped: false,
        })
    }

    /// The pipeline's view: one response per source that reads this request's body.
    pub fn responses(&self, sampled_ms: u64) -> Vec<Response> {
        self.sources
            .iter()
            .map(|name| Response {
                source: name.clone(),
                body: if self.error.is_none() && !self.dropped {
                    self.body.clone()
                } else {
                    None
                },
                received_ms: if self.dropped {
                    sampled_ms
                } else {
                    self.received_ms
                },
                error: if self.dropped {
                    Some("dropped".into())
                } else {
                    self.error.clone()
                },
            })
            .collect()
    }
}

pub fn round_line(round: u64, sent_ms: u64, fetches: &[Fetch]) -> Value {
    json!({"round": round, "sentMs": sent_ms, "fetches": fetches.iter().map(Fetch::to_json).collect::<Vec<_>>()})
}

/// A round in the line form: `(sentMs, fetches)`.
pub fn parse_round_line(line: &str) -> Result<(u64, Vec<Fetch>), String> {
    let v: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let sent = v.get("sentMs").and_then(Value::as_u64).ok_or("sentMs")?;
    let fetches = v
        .get("fetches")
        .and_then(Value::as_array)
        .ok_or("fetches")?
        .iter()
        .map(Fetch::from_json)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((sent, fetches))
}

/// Every round of a recording, in order: `path` is a `.jsonl` file (line form) or a directory of round
/// directories (directory form). Each item is `(label, sentMs, responses)`.
pub fn load_recording(path: &Path) -> Result<Vec<(String, u64, Vec<Response>)>, String> {
    let mut out = Vec::new();
    if path.is_dir() {
        let mut dirs: Vec<PathBuf> = fs::read_dir(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for d in dirs {
            let (t0, r) = load_round_dir(&d)?;
            out.push((d.file_name().unwrap().to_string_lossy().into_owned(), t0, r));
        }
    } else {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        for (i, line) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let (t0, f) = parse_round_line(line)
                .map_err(|e| format!("{} line {}: {e}", path.display(), i + 1))?;
            let r = f.iter().flat_map(|x| x.responses(t0)).collect();
            out.push((format!("round-{}", t0 / perps_oracle::ROUND_MS), t0, r));
        }
    }
    if out.is_empty() {
        return Err(format!("{}: no recorded rounds", path.display()));
    }
    Ok(out)
}
