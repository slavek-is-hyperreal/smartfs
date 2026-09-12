//! smartfs-ipfs — IpfsStore and CID helpers for SmartFS.
//!
//! Exclusively owns the Kubo daemon client, CID computation from content_hash (<256KB),
//! and BlobStore implementation for IPFS.

pub mod cid;
pub mod store;

pub use cid::{compute_cid, compute_cid_from_digest, MAX_INLINE_CID_SIZE};
pub use store::IpfsStore;
