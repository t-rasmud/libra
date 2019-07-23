// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{
    chained_bft::{
        common::Payload,
        consensus_types::{block::Block, quorum_cert::QuorumCert},
        liveness::pacemaker_timeout_manager::HighestTimeoutCertificates,
        safety::safety_rules::ConsensusState,
    },
    consensus_provider::create_storage_read_client,
};
use config::config::NodeConfig;
use crypto::HashValue;
use failure::Result;
use logger::prelude::*;
use rmp_serde::{from_slice, to_vec_named};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// Persistent storage for liveness data
pub trait PersistentLivenessStorage: Send + Sync {
    /// Persist the highest timeout certificate for improved liveness - proof for other replicas
    /// to jump to this round
    fn save_highest_timeout_cert(
        &self,
        highest_timeout_certs: HighestTimeoutCertificates,
    ) -> Result<()>;
}

/// Persistent storage is essential for maintaining safety when a node crashes.  Specifically,
/// upon a restart, a correct node will not equivocate.  Even if all nodes crash, safety is
/// guaranteed.  This trait also also supports liveness aspects (i.e. highest timeout certificate)
/// and supports clean up (i.e. tree pruning).
/// Blocks persisted are proposed but not yet committed.  The committed state is persisted
/// via StateComputer.
pub trait PersistentStorage<T>: PersistentLivenessStorage + Send + Sync {
    /// Get an Arc to an instance of PersistentLivenessStorage
    /// (workaround for trait downcasting
    fn persistent_liveness_storage(&self) -> Box<dyn PersistentLivenessStorage>;

    /// Persist the blocks and quorum certs into storage atomically.
    fn save_tree(&self, blocks: Vec<Block<T>>, quorum_certs: Vec<QuorumCert>) -> Result<()>;

    /// Delete the corresponding blocks and quorum certs atomically.
    fn prune_tree(&self, block_ids: Vec<HashValue>) -> Result<()>;

    /// Persist the consensus state.
    fn save_consensus_state(&self, state: ConsensusState) -> Result<()>;

    /// When the node restart, construct the instance and returned the data read from db.
    /// This could guarantee we only read once during start, and we would panic if the
    /// read fails.
    /// It makes sense to be synchronous since we can't do anything else until this finishes.
    fn start(config: &NodeConfig) -> (Arc<Self>, RecoveryData<T>)
    where
        Self: Sized;
}

/// The recovery data constructed from raw consensusdb data, it'll find the root value and
/// blocks that need cleanup or return error if the input data is inconsistent.
#[derive(Debug)]
pub struct RecoveryData<T> {
    // Safety data
    state: ConsensusState,
    root: (Block<T>, QuorumCert, QuorumCert),
    // 1. the blocks guarantee the topological ordering - parent <- child.
    // 2. all blocks are children of the root.
    blocks: Vec<Block<T>>,
    quorum_certs: Vec<QuorumCert>,
    blocks_to_prune: Option<Vec<HashValue>>,

    // Liveness data
    highest_timeout_certificates: HighestTimeoutCertificates,

    // whether root is consistent with StateComputer, if not we need to do the state sync before
    // starting
    need_sync: bool,
}

impl<T: Payload> RecoveryData<T> {

    pub fn take(
        self,
    ) -> (
        (Block<T>, QuorumCert, QuorumCert),
        Vec<Block<T>>,
        Vec<QuorumCert>,
    ) {
        (self.root, self.blocks, self.quorum_certs)
    }
}