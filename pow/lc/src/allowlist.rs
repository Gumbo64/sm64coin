//! The node RPC allowlist: the only daemon calls the light-client service (and the website / apps)
//! may forward. The node's RPC is unrestricted (stop_daemon, set_bans, prune_blockchain ...).
//!
//! The launcher (`pow/launcher/src/rpc.rs`) uses these lists directly. The wallet part is derived from every
//! daemon call `wallet2` makes (sm64coin/monero/src/wallet/wallet2.cpp, node_rpc_proxy.cpp, and monero-ts, which
//! compiles the same wallet2): only calls the node serves on a *restricted* RPC are listed (never
//! `/get_transaction_pool`, `/get_peer_list`, `/start_mining` ...).

/// JSON-RPC methods (`/json_rpc`).
pub const RPC_ALLOWED: &[&str] = &[
    "get_info", "get_block_count", "get_block", "get_block_header_by_hash", "get_block_header_by_height",
    "get_block_headers_range", "get_block_template", "submit_block",
    // proof-of-play (the miner's parent state and cycle) and the node's leaderboard
    "get_sm64_cycle", "get_sm64_state", "sm64_board", "sm64_board_join",
    // read-only calls a wallet makes (restricted-RPC safe): node_rpc_proxy.cpp (get_version, get_info, hard_fork_info,
    // get_fee_estimate, rpc_access_info, getblockheaderbyheight) and wallet2.cpp (getblockheadersrange,
    // get_output_histogram, get_output_distribution, get_txpool_backlog)
    "get_version", "get_fee_estimate", "get_last_block_header", "get_output_distribution", "hard_fork_info",
    "get_output_histogram", "get_txpool_backlog", "rpc_access_info",
    // the legacy spellings wallet2 still sends
    "getblockheaderbyheight", "getblockheadersrange", "getblockheaderbyhash", "getlastblockheader", "getblockcount", "getblock",
    // v0.4 proof records
    "submit_sm64_proof",
];

/// Non-JSON paths (`.bin` epee and the plain-JSON wallet calls).
pub const PATH_ALLOWED: &[&str] = &[
    // sync: blocks, hashes, output indexes, output distribution, pool
    "/getblocks.bin", "/get_blocks.bin", "/getblocks_by_height.bin", "/get_blocks_by_height.bin", "/gethashes.bin", "/get_hashes.bin",
    "/get_o_indexes.bin", "/get_output_distribution.bin", "/get_transaction_pool_hashes.bin", "/get_transaction_pool_hashes",
    "/get_transaction_pool_stats",
    // sending: decoys, tx lookup, spent check, broadcast
    "/get_outs.bin", "/get_transactions", "/gettransactions", "/is_key_image_spent", "/send_raw_transaction", "/sendrawtransaction",
    "/get_height", "/getheight", "/get_info", "/getinfo", "/get_public_nodes",
];

pub fn rpc_allowed(method: &str) -> bool {
    RPC_ALLOWED.contains(&method)
}

pub fn path_allowed(path: &str) -> bool {
    PATH_ALLOWED.contains(&path)
}
