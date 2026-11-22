//! On-chain leaderboard opt-in.
//!
//! A miner opts in by writing a small record into the extra-nonce field of its own
//! blocks' coinbase (via `get_block_template`'s `extra_nonce`; nodes treat it as opaque
//! bytes, so no consensus change). The record carries a display name, the address's
//! public spend key and the *private view key*. Anyone can then check, with the view key
//! alone, that the block's coinbase really pays that address: tags that do not verify
//! are ignored, so a tag cannot claim someone else's blocks.
pub mod crypto;
pub mod tag;
