extern crate rustc_hash;
extern crate slab;
use ckb_network::PeerIndex;
use ckb_types::{
    core::{Cycle, TransactionView},
    packed::ProposalShortId,
};
use ckb_util::shrink_to_fit;
use multi_index_map::MultiIndexMap;

const DEFAULT_MAX_VERIFY_TRANSACTIONS: usize = 100;
const SHRINK_THRESHOLD: usize = 120;

#[derive(Debug, Clone, Eq)]
pub(crate) struct Entry {
    pub(crate) tx: TransactionView,
    pub(crate) remote: Option<(Cycle, PeerIndex)>,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Entry) -> bool {
        self.tx == other.tx
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VerifyStatus {
    Fresh,
    Verifying,
    Completed,
}

#[derive(MultiIndexMap, Clone)]
pub struct VerifyEntry {
    #[multi_index(hashed_unique)]
    pub id: ProposalShortId,
    #[multi_index(hashed_non_unique)]
    pub status: VerifyStatus,
    // other sort key
    pub inner: Entry,
}

#[derive(Default)]
pub(crate) struct VerifyQueue {
    inner: MultiIndexVerifyEntryMap,
}

impl VerifyQueue {
    pub(crate) fn new() -> Self {
        VerifyQueue {
            inner: MultiIndexVerifyEntryMap::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.len() > DEFAULT_MAX_VERIFY_TRANSACTIONS
    }

    pub fn contains_key(&self, id: &ProposalShortId) -> bool {
        self.inner.get_by_id(id).is_some()
    }

    pub fn shrink_to_fit(&mut self) {
        shrink_to_fit!(self.inner, SHRINK_THRESHOLD);
    }

    pub fn remove_tx(&mut self, id: &ProposalShortId) -> Option<Entry> {
        let ret = self.inner.remove_by_id(id);
        self.shrink_to_fit();
        Some(ret.unwrap().inner)
    }

    pub fn remove_txs(&mut self, ids: impl Iterator<Item = ProposalShortId>) {
        for id in ids {
            self.inner.remove_by_id(&id);
        }
        self.shrink_to_fit();
    }

    /// If the queue did not have this tx present, true is returned.
    /// If the queue did have this tx present, false is returned.
    pub fn add_tx(&mut self, tx: TransactionView, remote: Option<(Cycle, PeerIndex)>) -> bool {
        if self.contains_key(&tx.proposal_short_id()) {
            return false;
        }
        let entry = Entry { tx, remote };
        self.inner.insert(VerifyEntry {
            id: tx.proposal_short_id(),
            status: VerifyStatus::Fresh,
            inner: entry.clone(),
        });
        true
    }

    /// Clears the map, removing all elements.
    pub fn clear(&mut self) {
        self.inner.clear();
        self.shrink_to_fit()
    }
}
