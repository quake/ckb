//! CKB chain service.
#![allow(missing_docs)]

use crate::consume_unverified::ConsumeUnverifiedBlocks;
use crate::utils::orphan_block_pool::OrphanBlockPool;
use crate::{
    tell_synchronizer_to_punish_the_bad_peer, LonelyBlock, LonelyBlockWithCallback,
    ProcessBlockRequest, TruncateRequest, UnverifiedBlock, VerifyCallback, VerifyResult,
};
use ckb_channel::{self as channel, select, Receiver, SendError, Sender};
use ckb_constant::sync::BLOCK_DOWNLOAD_WINDOW;
use ckb_error::{Error, InternalErrorKind};
use ckb_logger::{self, debug, error, info, warn};
use ckb_network::tokio;
use ckb_shared::shared::Shared;
use ckb_shared::types::VerifyFailedBlockInfo;
use ckb_shared::ChainServicesBuilder;
use ckb_stop_handler::{new_crossbeam_exit_rx, register_thread};
use ckb_types::{
    core::{service::Request, BlockView},
    packed::Byte32,
};
use ckb_verification::{BlockVerifier, NonContextualBlockTxsVerifier};
use ckb_verification_traits::{Switch, Verifier};
use std::sync::Arc;
use std::thread;

const ORPHAN_BLOCK_SIZE: usize = (BLOCK_DOWNLOAD_WINDOW * 2) as usize;

/// Controller to the chain service.
///
/// The controller is internally reference-counted and can be freely cloned.
///
/// A controller can invoke ChainService methods.
#[cfg_attr(feature = "mock", faux::create)]
#[derive(Clone)]
pub struct ChainController {
    process_block_sender: Sender<ProcessBlockRequest>,
    truncate_sender: Sender<TruncateRequest>,
    orphan_block_broker: Arc<OrphanBlockPool>,
}

#[cfg_attr(feature = "mock", faux::methods)]
impl ChainController {
    fn new(
        process_block_sender: Sender<ProcessBlockRequest>,
        truncate_sender: Sender<TruncateRequest>,
        orphan_block_broker: Arc<OrphanBlockPool>,
    ) -> Self {
        ChainController {
            process_block_sender,
            truncate_sender,
            orphan_block_broker,
        }
    }

    pub fn asynchronous_process_block_with_switch(&self, block: Arc<BlockView>, switch: Switch) {
        self.asynchronous_process_lonely_block(LonelyBlock {
            block,
            peer_id_with_msg_bytes: None,
            switch: Some(switch),
        })
    }

    pub fn asynchronous_process_block(&self, block: Arc<BlockView>) {
        self.asynchronous_process_lonely_block_with_callback(
            LonelyBlock {
                block,
                peer_id_with_msg_bytes: None,
                switch: None,
            }
            .without_callback(),
        )
    }

    pub fn asynchronous_process_block_with_callback(
        &self,
        block: Arc<BlockView>,
        verify_callback: VerifyCallback,
    ) {
        self.asynchronous_process_lonely_block_with_callback(
            LonelyBlock {
                block,
                peer_id_with_msg_bytes: None,
                switch: None,
            }
            .with_callback(Some(verify_callback)),
        )
    }

    pub fn asynchronous_process_lonely_block(&self, lonely_block: LonelyBlock) {
        let lonely_block_without_callback: LonelyBlockWithCallback =
            lonely_block.without_callback();

        self.asynchronous_process_lonely_block_with_callback(lonely_block_without_callback);
    }

    /// Internal method insert block for test
    ///
    /// switch bit flags for particular verify, make easier to generating test data
    pub fn asynchronous_process_lonely_block_with_callback(
        &self,
        lonely_block_with_callback: LonelyBlockWithCallback,
    ) {
        if Request::call(&self.process_block_sender, lonely_block_with_callback).is_none() {
            error!("Chain service has gone")
        }
    }

    pub fn blocking_process_block(&self, block: Arc<BlockView>) -> VerifyResult {
        self.blocking_process_lonely_block(LonelyBlock {
            block,
            peer_id_with_msg_bytes: None,
            switch: None,
        })
    }

    pub fn blocking_process_block_with_switch(
        &self,
        block: Arc<BlockView>,
        switch: Switch,
    ) -> VerifyResult {
        self.blocking_process_lonely_block(LonelyBlock {
            block,
            peer_id_with_msg_bytes: None,
            switch: Some(switch),
        })
    }

    pub fn blocking_process_lonely_block(&self, lonely_block: LonelyBlock) -> VerifyResult {
        let (verify_result_tx, verify_result_rx) = ckb_channel::oneshot::channel::<VerifyResult>();

        let verify_callback = {
            move |result: VerifyResult| {
                if let Err(err) = verify_result_tx.send(result) {
                    error!(
                        "blocking send verify_result failed: {}, this shouldn't happen",
                        err
                    )
                }
            }
        };

        let lonely_block_with_callback =
            lonely_block.with_callback(Some(Box::new(verify_callback)));
        self.asynchronous_process_lonely_block_with_callback(lonely_block_with_callback);
        verify_result_rx.recv().unwrap_or_else(|err| {
            Err(InternalErrorKind::System
                .other(format!("blocking recv verify_result failed: {}", err))
                .into())
        })
    }

    /// Truncate chain to specified target
    ///
    /// Should use for testing only
    pub fn truncate(&self, target_tip_hash: Byte32) -> Result<(), Error> {
        Request::call(&self.truncate_sender, target_tip_hash).unwrap_or_else(|| {
            Err(InternalErrorKind::System
                .other("Chain service has gone")
                .into())
        })
    }

    // Relay need this
    pub fn get_orphan_block(&self, hash: &Byte32) -> Option<Arc<BlockView>> {
        self.orphan_block_broker.get_block(hash)
    }

    pub fn orphan_blocks_len(&self) -> usize {
        self.orphan_block_broker.len()
    }
}

pub fn start_chain_services(builder: ChainServicesBuilder) -> ChainController {
    let orphan_blocks_broker = Arc::new(OrphanBlockPool::with_capacity(ORPHAN_BLOCK_SIZE));

    let (truncate_block_tx, truncate_block_rx) = channel::bounded(1);

    let (unverified_queue_stop_tx, unverified_queue_stop_rx) = ckb_channel::bounded::<()>(1);
    let (unverified_tx, unverified_rx) =
        channel::bounded::<UnverifiedBlock>(BLOCK_DOWNLOAD_WINDOW as usize * 3);

    let consumer_unverified_thread = thread::Builder::new()
        .name("consume_unverified_blocks".into())
        .spawn({
            let shared = builder.shared.clone();
            let verify_failed_blocks_tx = builder.verify_failed_blocks_tx.clone();
            move || {
                let consume_unverified = ConsumeUnverifiedBlocks::new(
                    shared,
                    unverified_rx,
                    truncate_block_rx,
                    builder.proposal_table,
                    verify_failed_blocks_tx,
                    unverified_queue_stop_rx,
                );

                consume_unverified.start();
            }
        })
        .expect("start unverified_queue consumer thread should ok");

    let (lonely_block_tx, lonely_block_rx) =
        channel::bounded::<LonelyBlockWithCallback>(BLOCK_DOWNLOAD_WINDOW as usize);

    let (search_orphan_pool_stop_tx, search_orphan_pool_stop_rx) = ckb_channel::bounded::<()>(1);

    let search_orphan_pool_thread = thread::Builder::new()
        .name("consume_orphan_blocks".into())
        .spawn({
            let orphan_blocks_broker = Arc::clone(&orphan_blocks_broker);
            let shared = builder.shared.clone();
            use crate::consume_orphan::ConsumeOrphan;
            let verify_failed_block_tx = builder.verify_failed_blocks_tx.clone();
            move || {
                let consume_orphan = ConsumeOrphan::new(
                    shared,
                    orphan_blocks_broker,
                    unverified_tx,
                    lonely_block_rx,
                    verify_failed_block_tx,
                    search_orphan_pool_stop_rx,
                );
                consume_orphan.start();
            }
        })
        .expect("start search_orphan_pool thread should ok");

    let (process_block_tx, process_block_rx) = channel::bounded(BLOCK_DOWNLOAD_WINDOW as usize);

    let chain_service: ChainService = ChainService::new(
        builder.shared,
        process_block_rx,
        lonely_block_tx,
        builder.verify_failed_blocks_tx,
    );
    let chain_service_thread = thread::Builder::new()
        .name("ChainService".into())
        .spawn({
            move || {
                chain_service.start_process_block();

                if let Err(SendError(_)) = search_orphan_pool_stop_tx.send(()) {
                    warn!("trying to notify search_orphan_pool thread to stop, but search_orphan_pool_stop_tx already closed")
                }
                let _ = search_orphan_pool_thread.join();

                if let Err(SendError(_))= unverified_queue_stop_tx.send(()){
                    warn!("trying to notify consume unverified thread to stop, but unverified_queue_stop_tx already closed");
                }
                let _ = consumer_unverified_thread.join();
            }
        })
        .expect("start chain_service thread should ok");
    register_thread("ChainServices", chain_service_thread);

    ChainController::new(process_block_tx, truncate_block_tx, orphan_blocks_broker)
}

/// Chain background service
///
/// The ChainService provides a single-threaded background executor.
#[derive(Clone)]
pub(crate) struct ChainService {
    shared: Shared,

    process_block_rx: Receiver<ProcessBlockRequest>,

    lonely_block_tx: Sender<LonelyBlockWithCallback>,
    verify_failed_blocks_tx: tokio::sync::mpsc::UnboundedSender<VerifyFailedBlockInfo>,
}
impl ChainService {
    /// Create a new ChainService instance with shared and initial proposal_table.
    pub(crate) fn new(
        shared: Shared,
        process_block_rx: Receiver<ProcessBlockRequest>,

        lonely_block_tx: Sender<LonelyBlockWithCallback>,
        verify_failed_blocks_tx: tokio::sync::mpsc::UnboundedSender<VerifyFailedBlockInfo>,
    ) -> ChainService {
        ChainService {
            shared,
            process_block_rx,
            lonely_block_tx,
            verify_failed_blocks_tx,
        }
    }

    pub(crate) fn start_process_block(self) {
        let signal_receiver = new_crossbeam_exit_rx();

        loop {
            select! {
                recv(self.process_block_rx) -> msg => match msg {
                    Ok(Request { responder, arguments: lonely_block }) => {
                        // asynchronous_process_block doesn't interact with tx-pool,
                        // no need to pause tx-pool's chunk_process here.
                        self.asynchronous_process_block(lonely_block);
                        let _ = responder.send(());
                    },
                    _ => {
                        error!("process_block_receiver closed");
                        break;
                    },
                },
                recv(signal_receiver) -> _ => {
                    info!("ChainService received exit signal, exit now");
                    break;
                }
            }
        }
    }

    fn non_contextual_verify(&self, block: &BlockView) -> Result<(), Error> {
        let consensus = self.shared.consensus();
        BlockVerifier::new(consensus).verify(block).map_err(|e| {
            debug!("[process_block] BlockVerifier error {:?}", e);
            e
        })?;

        NonContextualBlockTxsVerifier::new(consensus)
            .verify(block)
            .map_err(|e| {
                debug!(
                    "[process_block] NonContextualBlockTxsVerifier error {:?}",
                    e
                );
                e
            })
            .map(|_| ())
    }

    // make block IO and verify asynchronize
    fn asynchronous_process_block(&self, lonely_block: LonelyBlockWithCallback) {
        let block_number = lonely_block.block().number();
        let block_hash = lonely_block.block().hash();
        if block_number < 1 {
            warn!("receive 0 number block: 0-{}", block_hash);
        }

        if lonely_block.switch().is_none()
            || matches!(lonely_block.switch(), Some(switch) if !switch.disable_non_contextual())
        {
            let result = self.non_contextual_verify(lonely_block.block());
            if let Err(err) = result {
                tell_synchronizer_to_punish_the_bad_peer(
                    self.verify_failed_blocks_tx.clone(),
                    lonely_block.peer_id_with_msg_bytes(),
                    lonely_block.block().hash(),
                    &err,
                );

                lonely_block.execute_callback(Err(err));
                return;
            }
        }

        match self.lonely_block_tx.send(lonely_block) {
            Ok(_) => {}
            Err(SendError(lonely_block)) => {
                error!("failed to notify new block to orphan pool");

                let err: Error = InternalErrorKind::System
                    .other("OrphanBlock broker disconnected")
                    .into();

                tell_synchronizer_to_punish_the_bad_peer(
                    self.verify_failed_blocks_tx.clone(),
                    lonely_block.peer_id_with_msg_bytes(),
                    lonely_block.block().hash(),
                    &err,
                );

                let verify_result = Err(err);
                lonely_block.execute_callback(verify_result);
                return;
            }
        }
        debug!(
            "processing block: {}-{}, (tip:unverified_tip):({}:{})",
            block_number,
            block_hash,
            self.shared.snapshot().tip_number(),
            self.shared.get_unverified_tip().number(),
        );
    }
}