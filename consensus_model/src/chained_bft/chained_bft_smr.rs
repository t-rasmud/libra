// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{
    chained_bft::{
        block_storage::{BlockReader, BlockStore},
        common::{Payload, Round},
        event_processor::{EventProcessor, ProcessProposalResult},
        liveness::{
            local_pacemaker::{ExponentialTimeInterval, LocalPacemaker},
            pacemaker::{NewRoundEvent, Pacemaker},
            pacemaker_timeout_manager::HighestTimeoutCertificates,
            proposal_generator::ProposalGenerator,
            proposer_election::{ProposalInfo, ProposerElection, ProposerInfo},
            rotating_proposer_election::RotatingProposer,
            timeout_msg::TimeoutMsg,
        },
        network::{
            BlockRetrievalRequest, ChunkRetrievalRequest, ConsensusNetworkImpl, NetworkReceivers,
        },
        persistent_storage::{PersistentLivenessStorage, PersistentStorage, RecoveryData},
        safety::{safety_rules::SafetyRules, vote_msg::VoteMsg},
    },
    counters,
    state_replication::{StateComputer, TxnManager},
    state_synchronizer::SyncStatus,
    util::time_service::{TimeService},
};
use channel;
use failure::prelude::*;
use futures::{
    compat::Future01CompatExt,
    executor::block_on,
    future::{FutureExt, TryFutureExt},
    stream::StreamExt,
};
use nextgen_crypto::ed25519::*;
use types::validator_signer::ValidatorSigner;

use config::config::ConsensusConfig;
use logger::prelude::*;
use std::{
    sync::{Arc, RwLock},
    thread,
    time::{Duration, Instant},
};
use tokio::runtime::{Runtime, TaskExecutor};

type ConcurrentEventProcessor<T, P> = Arc<futures_locks::RwLock<EventProcessor<T, P>>>;

/// Consensus configuration derived from ConsensusConfig
pub struct ChainedBftSMRConfig {
    /// Keep up to this number of committed blocks before cleaning them up from the block store.
    pub max_pruned_blocks_in_mem: usize,
    /// Initial timeout for pacemaker
    pub pacemaker_initial_timeout: Duration,
    /// Contiguous rounds for proposer
    pub contiguous_rounds: u32,
    /// Max block size (number of transactions) that consensus pulls from mempool
    pub max_block_size: u64,
}

impl ChainedBftSMRConfig {
    pub fn from_node_config(cfg: &ConsensusConfig) -> ChainedBftSMRConfig {
        let pacemaker_initial_timeout_ms = cfg.pacemaker_initial_timeout_ms().unwrap_or(1000);
        ChainedBftSMRConfig {
            max_pruned_blocks_in_mem: cfg.max_pruned_blocks_in_mem().unwrap_or(10000) as usize,
            pacemaker_initial_timeout: Duration::from_millis(pacemaker_initial_timeout_ms),
            contiguous_rounds: cfg.contiguous_rounds(),
            max_block_size: cfg.max_block_size(),
        }
    }
}

/// ChainedBFTSmr is the one to generate the components (BLockStore, Proposer, etc.) and start the
/// driver. ChainedBftSMR implements the StateMachineReplication, it is going to be used by
/// ConsensusProvider for the e2e flow.
pub struct ChainedBftSMR<T, P> {
    author: P,
    // TODO [Reconfiguration] quorum size is just a function of current validator set.
    quorum_size: usize,
    signer: Option<ValidatorSigner<Ed25519PrivateKey>>,
    proposers: Vec<P>,
    runtime: Option<Runtime>,
    block_store: Option<Arc<BlockStore<T>>>,
    network: ConsensusNetworkImpl,
    config: ChainedBftSMRConfig,
    storage: Arc<dyn PersistentStorage<T>>,
    initial_data: Option<RecoveryData<T>>,
}

#[allow(dead_code)]
impl<T: Payload, P: ProposerInfo> ChainedBftSMR<T, P> {

    async fn process_new_round_events(
        mut receiver: channel::Receiver<NewRoundEvent>,
        event_processor: ConcurrentEventProcessor<T, P>,
    ) {
        while let Some(new_round_event) = receiver.next().await {
            let guard = event_processor.read().compat().await.unwrap();
            guard.process_new_round_event(new_round_event).await;
        }
    }

    async fn process_proposals(
        executor: TaskExecutor,
        mut receiver: channel::Receiver<ProposalInfo<T, P>>,
        event_processor: ConcurrentEventProcessor<T, P>,
    ) {
        while let Some(proposal_info) = receiver.next().await {
            let guard = event_processor.read().compat().await.unwrap();
            match guard.process_proposal(proposal_info).await {
                ProcessProposalResult::Done => (),
                // Spawn a new task that would start retrieving the missing
                // blocks in the background.
                ProcessProposalResult::NeedFetch(deadline, proposal) => executor.spawn(
                    Self::fetch_and_process_proposal(
                        Arc::clone(&event_processor),
                        deadline,
                        proposal,
                    )
                    .boxed()
                    .unit_error()
                    .compat(),
                ),
                // Spawn a new task that would start state synchronization
                // in the background.
                ProcessProposalResult::NeedSync(deadline, proposal) => executor.spawn(
                    Self::sync_and_process_proposal(
                        Arc::clone(&event_processor),
                        deadline,
                        proposal,
                    )
                    .boxed()
                    .unit_error()
                    .compat(),
                ),
            }
        }
    }

    async fn fetch_and_process_proposal(
        event_processor: ConcurrentEventProcessor<T, P>,
        deadline: Instant,
        proposal: ProposalInfo<T, P>,
    ) {
        let guard = event_processor.read().compat().await.unwrap();
        guard.fetch_and_process_proposal(deadline, proposal).await
    }

    async fn sync_and_process_proposal(
        event_processor: ConcurrentEventProcessor<T, P>,
        deadline: Instant,
        proposal: ProposalInfo<T, P>,
    ) {
        let mut guard = event_processor.write().compat().await.unwrap();
        guard.sync_and_process_proposal(deadline, proposal).await
    }

    async fn process_winning_proposals(
        mut receiver: channel::Receiver<ProposalInfo<T, P>>,
        event_processor: ConcurrentEventProcessor<T, P>,
    ) {
        while let Some(proposal_info) = receiver.next().await {
            let guard = event_processor.read().compat().await.unwrap();
            guard.process_winning_proposal(proposal_info).await;
        }
    }

    async fn process_votes(
        mut receiver: channel::Receiver<VoteMsg>,
        event_processor: ConcurrentEventProcessor<T, P>,
        quorum_size: usize,
    ) {
        while let Some(vote) = receiver.next().await {
            let guard = event_processor.read().compat().await.unwrap();
            guard.process_vote(vote, quorum_size).await;
        }
    }

    async fn process_timeout_msg(
        mut receiver: channel::Receiver<TimeoutMsg>,
        event_processor: ConcurrentEventProcessor<T, P>,
    ) {
        while let Some(timeout_msg) = receiver.next().await {
            let mut guard = event_processor.write().compat().await.unwrap();
            guard.process_timeout_msg(timeout_msg).await;
        }
    }

    async fn process_outgoing_pacemaker_timeouts(
        mut receiver: channel::Receiver<Round>,
        event_processor: ConcurrentEventProcessor<T, P>,
        mut network: ConsensusNetworkImpl,
    ) {
        while let Some(round) = receiver.next().await {
            // Update the last voted round and generate the timeout message
            let guard = event_processor.read().compat().await.unwrap();
            let timeout_msg = guard.process_outgoing_pacemaker_timeout(round).await;
            match timeout_msg {
                Some(timeout_msg) => {
                    network.broadcast_timeout_msg(timeout_msg).await;
                }
                None => {
                    info!("Broadcast not sent as the processing of the timeout failed.  Will retry again on the next timeout.");
                }
            }
        }
    }

    async fn process_block_retrievals(
        mut receiver: channel::Receiver<BlockRetrievalRequest<T>>,
        event_processor: ConcurrentEventProcessor<T, P>,
    ) {
        while let Some(request) = receiver.next().await {
            let guard = event_processor.read().compat().await.unwrap();
            guard.process_block_retrieval(request).await;
        }
    }

    async fn process_chunk_retrievals(
        mut receiver: channel::Receiver<ChunkRetrievalRequest>,
        event_processor: ConcurrentEventProcessor<T, P>,
    ) {
        while let Some(request) = receiver.next().await {
            let guard = event_processor.read().compat().await.unwrap();
            guard.process_chunk_retrieval(request).await;
        }
    }
}