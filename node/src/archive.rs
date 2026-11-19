//! Rebuilding the engine state at a past height from stored blocks (R436-11): the input log holds every block
//! since genesis, so any node that keeps the log can recompute the state at any height and prove an account
//! against it, long after the in-memory frame of that height is gone. Used by the sequencer's API and by a
//! validator's own read service (`crate::vserve`).
//!
//! The rebuild is deterministic replay: each block's inputs are decoded and applied by the engine, and the state
//! root after each block must equal the block's logged root, or the rebuild fails (never a wrong state served).
//!
//! UNAUDITED PROTOTYPE.

use crate::engine::Engine;
use crate::genesis::Genesis;
use crate::types::{decode_inputs, Block};

/// Replays blocks `base_height + 1 ..= target` onto `engine` (the state at `base_height`). `take(from, limit)`
/// returns stored blocks from height `from`, oldest first (it may lock for the copy; the replay holds no lock).
pub fn rebuild(g: &Genesis, base_height: u64, mut engine: Box<dyn Engine>, target: u64, take: &dyn Fn(u64, u64) -> Vec<Block>) -> Result<Box<dyn Engine>, String> {
    if engine.height() != base_height {
        return Err(format!("rebuild base is at height {}, not {base_height}", engine.height()));
    }
    let mut h = base_height;
    while h < target {
        let chunk = take(h + 1, (target - h).min(200));
        if chunk.is_empty() {
            return Err(format!("the stored log ends at height {h}, before {target}"));
        }
        for b in chunk {
            if b.header.height != h + 1 {
                return Err(format!("stored block {} where {} was expected", b.header.height, h + 1));
            }
            let inputs = decode_inputs(&g.types, &b.inputs_encoded)?;
            engine.apply_block(b.header.height, b.header.time, &inputs).map_err(|e| format!("rebuild: block {} refused: {e}", b.header.height))?;
            if engine.state_root() != b.header.state_root {
                return Err(format!("rebuild: state root after block {} differs from the logged one", b.header.height));
            }
            h += 1;
            if h == target {
                break;
            }
        }
    }
    Ok(engine)
}
