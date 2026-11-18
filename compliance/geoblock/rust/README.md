# postern-geoblock (Rust port for the node)

UNAUDITED prototype (agent 397). Not legal advice. Port of `../src/*.mjs` (the reference) for agent 389's node
admission: strict list loader, the decision, `GET /v1/geo` body, sanctions matcher, edge assertion verification
with replay cache, attestation verification. The OFAC builder and the IP-database loaders stay in JS (operator
tools; the node loads their outputs). `tests/vectors.rs` runs every file in `../vectors/`.

Build and test (never on the Mac; on the build host through the shared limiter):
`~/l2dexbridge-20261030/build-slot.sh cargo test -j 4 --locked` with your own `CARGO_TARGET_DIR`.

Differences from the JS, all stricter or equal: `builtUtc` must be `YYYY-MM-DDTHH:MM:SS[.mmm]Z` (JS accepts what
`Date.parse` accepts); otherwise behaviour is held equal by the vectors.
