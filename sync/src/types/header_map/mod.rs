use ckb_async_runtime::Handle;
use ckb_stop_handler::{SignalSender, StopHandler};
use ckb_types::{
    core::{BlockNumber, EpochNumberWithFraction},
    packed::{Byte32, Byte32Reader},
    prelude::{Entity, FromSliceShouldBeOk, Reader},
    U256,
};
use std::path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::MissedTickBehavior;

mod backend;
mod backend_heed;
mod kernel_lru;
mod memory;

pub(crate) use self::{
    backend::KeyValueBackend, backend_heed::HeedBackend, kernel_lru::HeaderMapKernel,
    memory::MemoryMap,
};

use super::HeaderIndexView;

#[derive(Clone, Debug, PartialEq, Eq)]
struct HeaderIndexViewInner {
    number: BlockNumber,
    epoch: EpochNumberWithFraction,
    timestamp: u64,
    parent_hash: Byte32,
    total_difficulty: U256,
    skip_hash: Option<Byte32>,
}

impl HeaderIndexViewInner {
    fn to_vec(&self) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(self.number.to_le_bytes().as_slice());
        v.extend_from_slice(self.epoch.full_value().to_le_bytes().as_slice());
        v.extend_from_slice(self.timestamp.to_le_bytes().as_slice());
        v.extend_from_slice(self.parent_hash.as_slice());
        v.extend_from_slice(self.total_difficulty.to_le_bytes().as_slice());
        if let Some(ref skip_hash) = self.skip_hash {
            v.extend_from_slice(skip_hash.as_slice());
        }
        v
    }

    fn from_slice_should_be_ok(slice: &[u8]) -> Self {
        let number = BlockNumber::from_le_bytes(slice[0..8].try_into().expect("stored slice"));
        let epoch = EpochNumberWithFraction::from_full_value(u64::from_le_bytes(
            slice[8..16].try_into().expect("stored slice"),
        ));
        let timestamp = u64::from_le_bytes(slice[16..24].try_into().expect("stored slice"));
        let parent_hash = Byte32Reader::from_slice_should_be_ok(&slice[24..56]).to_entity();
        let total_difficulty = U256::from_little_endian(&slice[56..88]).expect("stored slice");
        let skip_hash = if slice.len() == 120 {
            Some(Byte32Reader::from_slice_should_be_ok(&slice[88..120]).to_entity())
        } else {
            None
        };
        Self {
            number,
            epoch,
            timestamp,
            parent_hash,
            total_difficulty,
            skip_hash,
        }
    }
}

impl From<(Byte32, HeaderIndexViewInner)> for HeaderIndexView {
    fn from((hash, inner): (Byte32, HeaderIndexViewInner)) -> Self {
        let HeaderIndexViewInner {
            number,
            epoch,
            timestamp,
            parent_hash,
            total_difficulty,
            skip_hash,
        } = inner;
        Self {
            hash,
            number,
            epoch,
            timestamp,
            parent_hash,
            total_difficulty,
            skip_hash,
        }
    }
}

impl From<HeaderIndexView> for (Byte32, HeaderIndexViewInner) {
    fn from(view: HeaderIndexView) -> Self {
        let HeaderIndexView {
            hash,
            number,
            epoch,
            timestamp,
            parent_hash,
            total_difficulty,
            skip_hash,
        } = view;
        (
            hash,
            HeaderIndexViewInner {
                number,
                epoch,
                timestamp,
                parent_hash,
                total_difficulty,
                skip_hash,
            },
        )
    }
}
pub struct HeaderMap {
    inner: Arc<HeaderMapKernel<HeedBackend>>,
    stop: StopHandler<()>,
}

impl Drop for HeaderMap {
    fn drop(&mut self) {
        self.stop.try_send(());
    }
}

const INTERVAL: Duration = Duration::from_millis(500);
// HeaderIndexView size is 152 bytes
const ITEM_BYTES_SIZE: usize = 152;
const WARN_THRESHOLD: usize = ITEM_BYTES_SIZE * 100_000;

impl HeaderMap {
    pub(crate) fn new<P>(tmpdir: Option<P>, memory_limit: usize, async_handle: &Handle) -> Self
    where
        P: AsRef<path::Path>,
    {
        if memory_limit < ITEM_BYTES_SIZE {
            panic!("The limit setting is too low");
        }
        if memory_limit < WARN_THRESHOLD {
            ckb_logger::warn!(
                "The low memory limit setting {} will result in inefficient synchronization",
                memory_limit
            );
        }
        let size_limit = memory_limit / ITEM_BYTES_SIZE;
        let inner = Arc::new(HeaderMapKernel::new(tmpdir, size_limit));
        let map = Arc::clone(&inner);
        let (stop, mut stop_rx) = oneshot::channel::<()>();

        async_handle.spawn(async move {
            let mut interval = tokio::time::interval(INTERVAL);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        map.limit_memory();
                    }
                    _ = &mut stop_rx => break,
                }
            }
        });

        Self {
            inner,
            stop: StopHandler::new(SignalSender::Tokio(stop), None, "HeaderMap".to_string()),
        }
    }

    pub(crate) fn contains_key(&self, hash: &Byte32) -> bool {
        self.inner.contains_key(hash)
    }

    pub(crate) fn get(&self, hash: &Byte32) -> Option<HeaderIndexView> {
        self.inner.get(hash)
    }

    pub(crate) fn insert(&self, view: HeaderIndexView) {
        self.inner.insert(view)
    }

    pub(crate) fn remove(&self, hash: &Byte32) {
        self.inner.remove(hash)
    }
}
