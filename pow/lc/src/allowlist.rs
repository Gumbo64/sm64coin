//! The node RPC allowlist: the only daemon calls the light-client service (and the website / apps)
//! may forward. The node's RPC is unrestricted (stop_daemon, set_bans, prune_blockchain ...).
//!
//! This is the list from `pow/launcher/src/rpc.rs` (`RPC_ALLOWED`) plus the proof-record and
//! wallet calls the service needs. TODO(integration): the launcher should depend on this crate and
//! drop its copy; until then keep both in step.

/// JSON-RPC methods (`/json_rpc`).
pub const RPC_ALLOWED: &[&str] = &[
    "get_info", "get_block_count", "get_block", "get_block_header_by_hash", "get_block_header_by_height",
    "get_block_headers_range", "get_block_template", "submit_block",
    // proof-of-play (the miner's parent state and cycle) and the node's leaderboard
    "get_sm64_cycle", "get_sm64_state", "sm64_board", "sm64_board_join",
    // read-only calls a wallet makes (restricted-RPC safe)
    "get_version", "get_fee_estimate", "get_last_block_header", "get_output_distribution", "hard_fork_info",
    // v0.4 proof records
    "submit_sm64_proof",
];

/// Non-JSON paths (`.bin` epee and the plain-JSON wallet calls).
pub const PATH_ALLOWED: &[&str] = &[
    "/getblocks.bin", "/get_outs.bin", "/gethashes.bin", "/get_transaction_pool_hashes.bin",
    "/send_raw_transaction", "/get_public_nodes",
];

pub fn rpc_allowed(method: &str) -> bool {
    RPC_ALLOWED.contains(&method)
}

pub fn path_allowed(path: &str) -> bool {
    PATH_ALLOWED.contains(&path)
}
