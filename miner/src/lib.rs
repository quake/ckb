extern crate bigint;
extern crate crossbeam_channel;
extern crate ethash;
#[macro_use]
extern crate log;
extern crate ckb_chain as chain;
extern crate ckb_core as core;
extern crate ckb_network as network;
extern crate ckb_notify;
extern crate ckb_pool as pool;
extern crate ckb_protocol;
extern crate ckb_sync as sync;
extern crate ckb_time as time;
extern crate ckb_util as util;
extern crate rand;
#[macro_use]
extern crate serde_derive;

use bigint::H256;

mod block_template;
pub mod miner;
mod sealer;

pub use block_template::{build_block_template, BlockTemplate};

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Config {
    // Max number of transactions this miner will assemble in a block
    pub max_tx: usize,
    pub new_transactions_threshold: u16,
    pub ethash_path: Option<String>,
    pub redeem_script_hash: H256,
}
