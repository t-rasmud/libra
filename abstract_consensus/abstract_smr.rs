/// Leader:
///
/// This event is triggered by a new quorum certificate at the previous round or a
/// timeout certificate at the previous round.  In either case, if this replica is the new
/// proposer for this round, it is ready to propose and guarantee that it can create a proposal
/// that all honest replicas can vote for.  While this method should only be invoked at most
/// once per round, we ensure that only at most one proposal can get generated per round to
/// avoid accidental equivocation of proposals.
///
/// Replica:
///
/// Do nothing

pub async fn process_new_round_event(NewRoundEvent) {
    let proposer_info = match self
        .proposer_election
        .is_valid_proposer(self.author, new_round_event.round)
        {
            Some(pi) => pi,
            None => {
                return;
            }
        };

    // Proposal generator will ensure that at most one proposal is generated per round
    let proposal = match self
        .proposal_generator
        .generate_proposal(
            new_round_event.round,
        )
        .await
        {
            Err(e) => {
                error!("Error while generating proposal: {:?}", e);
                return;
            }
            Ok(proposal) => proposal,
        };

    let highest_ledger_info = (*self.block_store.highest_ledger_info()).clone();
    network
        .broadcast_proposal(ProposalInfo {
            proposal,
            proposer_info,
            highest_ledger_info,
        })
        .await;
}

/// The function is responsible for processing the incoming proposals and the Quorum
/// Certificate. 1. commit to the committed state the new QC carries
/// 2. fetch all the blocks from the committed state to the QC
/// 3. forwarding the proposals to the ProposerElection queue,
/// which is going to eventually trigger one winning proposal per round
/// (to be processed via a separate function).
/// The reason for separating `process_proposal` from `process_winning_proposal` is to
/// (a) asynchronously prefetch dependencies and
/// (b) allow the proposer election to choose one proposal out of many.
pub async fn process_proposal(ProposalInfo<T, P>,) {
    if (proposal_from_sync_manager_daemon) {
        // How to distinguish proposals?
        // Round numbers?
        // call fetch and sync methods from sync_manager
    }

    let current_round = pacemaker_daemon.current_round();
    // Old proposal
    if proposal.proposal.round() < pacemaker_daemon.current_round() {
        return;
    }

    // Invalid proposer
    if self
        .proposer_election
        .is_valid_proposer(proposal.proposer_info, proposal.proposal.round())
        .is_none()
    {
        return;
    }

    // Need sync, wait for Proposal from SyncManager daemon!!
    if let Some(committed_block_id) = proposal.highest_ledger_info.committed_block_id() {
        if self
            .block_store
            .need_sync_for_quorum_cert(committed_block_id, &proposal.highest_ledger_info)
        {
            return;
        }
    }

    // Need fetch, wait for Proposal from SyncManager daemon!!
    match self
        .block_store
        .need_fetch_for_quorum_cert(proposal.proposal.quorum_cert())
        {
            NeedFetchResult::NeedFetch => {
                return;
            }
            NeedFetchResult::QCRoundBeforeRoot => {
                warn!("Proposal {} has a highest quorum certificate with round older than root round {}", proposal, self.block_store.root().round());
                return;
            }
            NeedFetchResult::QCBlockExist => {
                if let Err(e) = self
                    .block_store
                    .insert_single_quorum_cert(proposal.proposal.quorum_cert().clone())
                    .await
                {
                    warn!(
                        "Quorum certificate for proposal {} could not be inserted to the block store: {:?}",
                        proposal, e
                    );
                    return;
                }
            }
            NeedFetchResult::QCAlreadyExist => {
                // Process winning proposal!!
                if pacemaker_daemon.current_round() != proposal.proposal.round() {
                    warn!(
                        "Proposal {} is ignored because its round {} != current round {}",
                        proposal,
                        proposal.proposal.round(),
                        current_round
                    );
                    return;
                }


                // This calls process_winning_proposal
                self.proposer_election.process_proposal(proposal).await;
            },
        }

}

/// Upon new commit:
/// 1. Notify state computer with the finality proof.
/// 2. After the state is finalized, update the txn manager with the status of the committed
/// transactions.
/// 3. Prune the tree.
async fn process_commit(
    committed_block: Arc<Block<T>>,
    finality_proof: LedgerInfoWithSignatures,
) {
    // Verify that the ledger info is indeed for the block we're planning to
    // commit.
    assert_eq!(
        finality_proof.ledger_info().consensus_block_id(),
        committed_block.id()
    );

    // Update the pacemaker_daemon with the highest committed round so that on the next round
    // duration it calculates, the initial round index is reset
    pacemaker_daemon
        .update_highest_committed_round(committed_block.round());

    if let Err(e) = self.state_computer.commit(finality_proof).await {
        // We assume that state computer cannot enter an inconsistent state that might
        // violate safety of the protocol. Specifically, an executor service is going to panic
        // if it fails to persist the commit requests, which would crash the whole process
        // including consensus.
        error!(
            "Failed to persist commit, mempool will not be notified: {:?}",
            e
        );
        return;
    }
    // At this moment the new state is persisted and we can notify the clients.
    // Multiple blocks might be committed at once: notify about all the transactions in the
    // path from the old root to the new root.
    for committed in self
        .block_store
        .path_from_root(Arc::clone(&committed_block))
        .unwrap_or_else(Vec::new)
        {
            let compute_result = self
                .block_store
                .get_compute_result(committed.id())
                .expect("Compute result of a pending block is unknown");
            if let Err(e) = self
                .txn_manager
                .commit_txns(
                    committed.get_payload(),
                    compute_result.as_ref(),
                    committed.timestamp_usecs(),
                )
                .await
            {
                error!("Failed to notify mempool: {:?}", e);
            }
        }

    // self.block_store.prune_tree(committed_block.id()).await;
}

/// This function processes a proposal that was chosen as a representative of its round:
/// 1. Add it to a block store.
/// 2. Try to vote for it following the safety rules.
/// 3. In case a validator chooses to vote, send the vote to the representatives at the next
/// position.
async fn process_winning_proposal(ProposalInfo<T, P>,
) {
    let qc = proposal.proposal.quorum_cert();
    let update_res = self.safety_rules.write().unwrap().update(qc);
    if let Some(new_commit) = update_res {
        let finality_proof = qc.ledger_info().clone();
        self.process_commit(new_commit, finality_proof).await;
    }

    let block = match self
        .sync_manager
        .execute_and_insert_block(proposal.proposal)
        .await
        {
            Err(e) => {
                debug!(
                    "Block proposal could not be added to the block store: {:?}",
                    e
                );
                return;
            }
            Ok(block) => block,
        };

    // Checking pacemaker_daemon round again, because multiple proposal can now race
    // during async block retrieval
    if pacemaker_daemon.current_round() != block.round() {
        debug!(
            "Skip voting for winning proposal {} rejected because round is incorrect. Pacemaker: {}, proposal: {}",
            block,
            pacemaker_daemon.current_round(),
            block.round()
        );
        return;
    }

    let vote_info = match self
        .safety_rules
        .write()
        .unwrap()
        .voting_rule(Arc::clone(&block))
        {
            Err(e) => {
                debug!("{}Rejected{} {}: {:?}", Fg(Red), Fg(Reset), block, e);
                return;
            }
            Ok(vote_info) => vote_info,
        };

    let proposal_id = vote_info.proposal_id();
    let executed_state = self
        .block_store
        .get_state_for_block(proposal_id)
        .expect("Block proposal: no execution state found for inserted block.");

    let ledger_info_placeholder = self
        .block_store
        .ledger_info_placeholder(vote_info.potential_commit_id());
    let vote_msg = VoteMsg::new(
        proposal_id,
        executed_state,
        block.round(),
        self.author.get_author(),
        ledger_info_placeholder,
        self.block_store.signer(),
    );

    let recipients: Vec<Author> = self
        .proposer_election
        .get_valid_proposers(block.round() + 1)
        .iter()
        .map(ProposerInfo::get_author)
        .collect();
    self.network.send_vote(vote_msg, recipients).await;
}

// ----------------------------------

/// Upon new vote:
/// 1. Filter out votes for rounds that should not be processed by this validator (to avoid
/// potential attacks).
/// 2. Add the vote to the store and check whether it finishes a QC.
/// 3. Once the QC successfully formed, notify the Pacemaker.
async fn process_votes(VoteMsg>,
                       quorum_size: usize,
) {
    // Check whether this validator is a valid recipient of the vote.
    let next_round = vote.round() + 1;
    if self
        .proposer_election
        .is_valid_proposer(self.author, next_round)
        .is_none()
    {
        debug!(
            "Received {}, but I am not a valid proposer for round {}, ignore.",
            vote, next_round
        );
        security_log(SecurityEvent::InvalidConsensusVote)
            .error("InvalidProposer")
            .data(vote)
            .data(next_round)
            .log();
        return;
    }

    // TODO [Reconfiguration] Verify epoch of the vote message.
    // Add the vote and check whether it completes a new QC.
    match self
        .block_store
        .insert_vote(vote.clone(), quorum_size)
        .await
        {
            VoteReceptionResult::DuplicateVote => {
                // This should not happen in general.
                security_log(SecurityEvent::DuplicateConsensusVote)
                    .error(VoteReceptionResult::DuplicateVote)
                    .data(vote)
                    .log();
            }
            VoteReceptionResult::NewQuorumCertificate(qc) => {
                if self.block_store.need_fetch_for_quorum_cert(&qc) == NeedFetchResult::NeedFetch {
                    if let Err(e) = self
                        .sync_manager
                        .fetch_quorum_cert(qc.as_ref().clone(), vote.author())
                        .await
                    {
                        error!("Error syncing to qc {}: {:?}", qc, e);
                        return;
                    }
                } else {
                    if let Err(e) = self
                        .block_store
                        .insert_single_quorum_cert(qc.as_ref().clone())
                        .await
                    {
                        error!("Error inserting qc {}: {:?}", qc, e);
                        return;
                    }
                }
                // Notify the Pacemaker about the new QC round.
                pacemaker_daemon
                    .process_certificates(vote.round(), None)
                    .await;
            }
            // nothing interesting with votes arriving for the QC that has been formed
            _ => {}
        };
}

/// Upon receiving TimeoutMsg, ensure that any branches with higher quorum certificates are
/// populated to this replica prior to processing the pacemaker timeout.  This ensures that when
/// a pacemaker timeout certificate is formed with 2f+1 timeouts, the next proposer will be
/// able to chain a proposal block to a highest quorum certificate such that all honest replicas
/// can vote for it.
async fn process_timeout_msg(TimeoutMsg,
) {
    let current_highest_quorum_cert_round = self
        .block_store
        .highest_quorum_cert()
        .certified_block_round();
    let new_round_highest_quorum_cert_round = timeout_msg
        .highest_quorum_certificate()
        .certified_block_round();

    if current_highest_quorum_cert_round < new_round_highest_quorum_cert_round {
        // The timeout message carries a QC higher than what this node has seen before:
        // run state synchronization.
        match self
            .sync_manager
            .sync_to(
                SyncInfo {
                    highest_ledger_info: timeout_msg.highest_ledger_info().clone(),
                    highest_quorum_cert: timeout_msg.highest_quorum_certificate().clone(),
                    peer: timeout_msg.author(),
                },
            )
            .await
            {
                Ok(()) => debug!(
                    "Successfully added new highest quorum certificate at round {} from old round {}",
                    new_round_highest_quorum_cert_round, current_highest_quorum_cert_round
                ),
                Err(e) => warn!(
                    "Unable to insert new highest quorum certificate {} from old round {} due to {:?}",
                    timeout_msg.highest_quorum_certificate(),
                    current_highest_quorum_cert_round,
                    e
                ),
            }
    }
}

// Daemons:

//Propose only blocks that have been previously proposed? Random blocks?
async fn sync_manager_daemon() {
    if (*) {
        proposal = new ProposalInfo();
        network.broadcast(proposal);
    }
}

pub struct pacemaker_daemon {
    current_round: Round,
    highest_committed_round : Round
}

impl pacemaker_daemon {
    async fn daemon() {
        current_round.inc();
        if (*) {
            broadcast(TimeoutMsg::new(
                self.block_store.highest_quorum_cert().as_ref().clone(),
                self.block_store.highest_ledger_info().as_ref().clone(),
                PacemakerTimeout::new(round, self.block_store.signer()),
                self.block_store.signer()),
            )
        }
    }

    /// Synchronous function to return the current round.
    fn current_round(&self) -> Round {}

    fn update_highest_committed_round(&self, highest_committed_round: Round);
}

// Helper functions
/// The function is invoked upon receiving a remote timeout message from another validator.
fn process_remote_timeout(
    &self,
    pacemaker_timeout: PacemakerTimeout,
) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    return update_current_round();
}

/// Combines highest_qc_certified_round, highest_local_tc and highest_received_tc into
/// effective round of this pacemaker.
/// Generates new_round event if effective round changes and ensures it is
/// monotonically increasing
fn update_current_round(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    let (mut best_round, mut best_reason) = (self.highest_qc_round, NewRoundReason::QCReady);
    if let Some(highest_timeout_certificate) =
    self.pacemaker_timeout_manager.highest_timeout_certificate()
    {
        if highest_timeout_certificate.round() > best_round {
            best_round = highest_timeout_certificate.round();
            best_reason = NewRoundReason::Timeout {
                cert: highest_timeout_certificate.clone(),
            };
        }
    }

    let new_round = best_round + 1;
    if self.current_round == new_round {
        return async {}.boxed();
    }
    assert!(
        new_round > self.current_round,
        "Round illegally decreased from {} to {}",
        self.current_round,
        new_round
    );
    self.current_round = new_round;
    self.create_new_round_task(best_reason).boxed()
}

/// Trigger an event to create a new round interval and ignore any events from previous round
/// intervals.  The reason for the event is given by the caller, the timeout is
/// deterministically determined by the reason and the internal state.
fn create_new_round_task(&mut self, reason: NewRoundReason) -> impl Future<Output = ()> + Send {
    let round = self.current_round;
    let timeout = self.setup_timeout();
    let mut sender = self.new_round_events_sender.clone();
    async move {
        if let Err(e) = sender
            .send(NewRoundEvent {
                round,
                reason,
                timeout,
            })
            .await
        {
            debug!("Error in sending new round interval event: {:?}", e);
        }
    }
}

// -----------------------------------------------------
// SafetyRules impl:
/// Learn about a new quorum certificate. Several things can happen as a result of that:
/// 1) update the preferred block to a higher value.
/// 2) commit some blocks.
/// In case of commits the last committed block is returned.
/// Requires that all the ancestors of the block are available for at least up to the last
/// committed block, might panic otherwise.
/// The update function is invoked whenever a system learns about a potentially high QC.
pub fn update(&mut self, qc: &QuorumCert) -> Option<Arc<Block<T>>> {
    // Preferred block rule: choose the highest 2-chain head.
    if let Some(one_chain_head) = self.block_tree.get_block(qc.certified_block_id()) {
        if let Some(two_chain_head) = self.block_tree.get_block(one_chain_head.parent_id()) {
            if two_chain_head.round() >= self.state.preferred_block_round() {
                self.state.set_preferred_block_round(two_chain_head.round());
            }
        }
    }
    self.process_ledger_info(qc.ledger_info())
}

/// Check to see if a processing a new LedgerInfoWithSignatures leads to a commit.  Return a
/// new committed block if there is one.
pub fn process_ledger_info(
    &mut self,
    ledger_info: &LedgerInfoWithSignatures,
) -> Option<Arc<Block<T>>> {
    // While voting for a block the validators have already calculated the potential commits.
    // In case there are no commits enabled by this ledger info, the committed block id is going
    // to carry some placeholder value (e.g., zero).
    let committed_block_id = ledger_info.ledger_info().consensus_block_id();
    if let Some(committed_block) = self.block_tree.get_block(committed_block_id) {
        // We check against the root of the tree instead of last committed round to avoid
        // double commit.
        // Because we only persist the ConsensusState before sending out the vote, it could
        // be lagged behind the reality if we crash between committing and sending the vote.
        if committed_block.round() > self.block_tree.root().round() {
            self.state.last_committed_round = committed_block.round();
            return Some(committed_block);
        }
    }
    None
}

/// Check if a one-chain at round r+2 causes a commit at round r and return the committed
/// block at round r if possible
pub fn commit_rule_for_certified_block(
    &self,
    one_chain_head: Arc<Block<T>>,
) -> Option<Arc<Block<T>>> {
    if let Some(two_chain_head) = self.block_tree.get_block(one_chain_head.parent_id()) {
        if let Some(three_chain_head) = self.block_tree.get_block(two_chain_head.parent_id()) {
            // We're using a so-called 3-chain commit rule: B0 (as well as its prefix)
            // can be committed if there exist certified blocks B1 and B2 that satisfy:
            // 1) B0 <- B1 <- B2 <--
            // 2) round(B0) + 1 = round(B1), and
            // 3) round(B1) + 1 = round(B2).
            if three_chain_head.round() + 1 == two_chain_head.round()
                && two_chain_head.round() + 1 == one_chain_head.round()
            {
                return Some(three_chain_head);
            }
        }
    }
    None
}

/// Return the highest known committed round
pub fn last_committed_round(&self) -> Round {
    self.state.last_committed_round
}

/// Return the new state if the voting round was increased, otherwise ignore.  Increasing the
/// last vote round is always safe, but can affect liveness and must be increasing
/// to protect safety.
pub fn increase_last_vote_round(&mut self, round: Round) -> Option<ConsensusState> {
    self.state.set_last_vote_round(round)
}

/// Clones the up-to-date state of consensus (for monitoring / debugging purposes)
#[allow(dead_code)]
pub fn consensus_state(&self) -> ConsensusState {
    self.state.clone()
}

/// Attempts to vote for a given proposal following the voting rules.
/// The returned value is then going to be used for either sending the vote or doing nothing.
/// In case of a vote a cloned consensus state is returned (to be persisted before the vote is
/// sent).
/// Requires that all the ancestors of the block are available for at least up to the last
/// committed block, might panic otherwise.
pub fn voting_rule(
    &mut self,
    proposed_block: Arc<Block<T>>,
) -> Result<VoteInfo, ProposalReject> {
    if proposed_block.round() <= self.state.last_vote_round() {
        return Err(ProposalReject::OldProposal {
            proposal_round: proposed_block.round(),
            last_vote_round: self.state.last_vote_round(),
        });
    }

    let parent_block = self
        .block_tree
        .get_block(proposed_block.parent_id())
        .expect("Parent block not found");
    let parent_block_round = parent_block.round();
    let respects_preferred_block = parent_block_round >= self.state.preferred_block_round();
    if respects_preferred_block {
        self.state.set_last_vote_round(proposed_block.round());

        // If the vote for the given proposal is gathered into QC, then this QC might eventually
        // commit another block following the rules defined in
        // `commit_rule_for_certified_block()` function.
        let potential_commit =
            self.commit_rule_for_certified_block(Arc::clone(&proposed_block));
        let potential_commit_id = match potential_commit {
            None => None,
            Some(commit_block) => Some(commit_block.id()),
        };

        Ok(VoteInfo {
            proposal_id: proposed_block.id(),
            proposal_round: proposed_block.round(),
            consensus_state: self.state.clone(),
            potential_commit_id,
        })
    } else {
        Err(ProposalReject::ProposalRoundLowerThenPreferredBlock {
            preferred_block_round: self.state.preferred_block_round(),
        })
    }
}
// ----------------------------------------------------------------

// Block store APIs -----------------------------------------------

impl<T: Payload> BlockStore<T> {

    pub fn signer(&self) -> &ValidatorSigner<Ed25519PrivateKey> {
        &self.validator_signer
    }

    /// Execute and insert a block if it passes all validation tests.
    /// Returns the Arc to the block kept in the block store after persisting it to storage
    ///
    /// This function assumes that the ancestors are present (returns MissingParent otherwise).
    ///
    /// Duplicate inserts will return the previously inserted block (
    /// note that it is considered a valid non-error case, for example, it can happen if a validator
    /// receives a certificate for a block that is currently being added).
    pub async fn execute_and_insert_block(
        &self,
        block: Block<T>,
    ) -> Result<Arc<Block<T>>, InsertError> {
        if let Some(existing_block) = self.inner.read().unwrap().get_block(block.id()) {
            return Ok(existing_block);
        }
        let (parent_id, parent_exec_version) = match self.verify_and_get_parent_info(&block) {
            Ok(t) => t,
            Err(e) => {
                security_log(SecurityEvent::InvalidBlock)
                    .error(&e)
                    .data(&block)
                    .log();
                return Err(e);
            }
        };
        let compute_res = self
            .state_computer
            .compute(parent_id, block.id(), block.get_payload())
            .await
            .map_err(|e| {
                error!("Execution failure for block {}: {:?}", block, e);
                InsertError::StateComputerError
            })?;

        let version = parent_exec_version + compute_res.num_successful_txns;

        let state = ExecutedState {
            state_id: compute_res.new_state_id,
            version,
        };
        self.storage
            .save_tree(vec![block.clone()], vec![])
            .map_err(|_| InsertError::StorageFailure)?;
        self.inner
            .write()
            .unwrap()
            .insert_block(block, state, compute_res)
            .map_err(|e| e.into())
    }

    /// Check if we're far away from this ledger info and need to sync.
    /// Returns false if we have this block in the tree or the root's round is higher than the
    /// block.
    pub fn need_sync_for_quorum_cert(
        &self,
        committed_block_id: HashValue,
        qc: &QuorumCert,
    ) -> bool {
        // LedgerInfo doesn't carry the information about the round of the committed block. However,
        // the 3-chain safety rules specify that the round of the committed block must be
        // certified_block_round() - 2. In case root().round() is greater than that the committed
        // block carried by LI is older than my current commit.
        !(self.block_exists(committed_block_id)
            || self.root().round() + 2 >= qc.certified_block_round())
    }

    /// Checks if quorum certificate can be inserted in block store without RPC
    /// Returns the enum to indicate the detailed status.
    pub fn need_fetch_for_quorum_cert(&self, qc: &QuorumCert) -> NeedFetchResult {
        if qc.certified_block_round() < self.root().round() {
            return NeedFetchResult::QCRoundBeforeRoot;
        }
        if self
            .get_quorum_cert_for_block(qc.certified_block_id())
            .is_some()
        {
            return NeedFetchResult::QCAlreadyExist;
        }
        if self.block_exists(qc.certified_block_id()) {
            return NeedFetchResult::QCBlockExist;
        }
        NeedFetchResult::NeedFetch
    }

    /// Validates quorum certificates and inserts it into block tree assuming dependencies exist.
    pub async fn insert_single_quorum_cert(&self, qc: QuorumCert) -> Result<(), InsertError> {
        // Ensure executed state is consistent with Quorum Cert, otherwise persist the quorum's
        // state and hopefully we restart and agree with it.
        let executed_state = self
            .get_state_for_block(qc.certified_block_id())
            .ok_or_else(|| InsertError::MissingParentBlock(qc.certified_block_id()))?;
        assert_eq!(
            executed_state,
            qc.certified_state(),
            "We have inconsistent executed state with the executed state from the quorum \
             certificate for block {}, will kill this validator and rely on state synchronization \
             to try to achieve consistent state with the quorum certificate.",
            qc.certified_block_id(),
        );
        self.storage
            .save_tree(vec![], vec![qc.clone()])
            .map_err(|_| InsertError::StorageFailure)?;
        self.inner
            .write()
            .unwrap()
            .insert_quorum_cert(qc)
            .map_err(|e| e.into())
    }

    /// Adds a vote for the block.
    /// The returned value either contains the vote result (with new / old QC etc.) or a
    /// verification error.
    /// A block store does not verify that the block, which is voted for, is present locally.
    /// It returns QC, if it is formed, but does not insert it into block store, because it might
    /// not have required dependencies yet
    /// Different execution ids are treated as different blocks (e.g., if some proposal is
    /// executed in a non-deterministic fashion due to a bug, then the votes for execution result
    /// A and the votes for execution result B are aggregated separately).
    pub async fn insert_vote(
        &self,
        vote_msg: VoteMsg,
        min_votes_for_qc: usize,
    ) -> VoteReceptionResult {
        self.inner
            .write()
            .unwrap()
            .insert_vote(&vote_msg, min_votes_for_qc)
    }

    /// If block id information is found, returns the ledger info placeholder, otherwise, return
    /// a placeholder with info of the genesis block.
    pub fn ledger_info_placeholder(&self, id: Option<HashValue>) -> LedgerInfo {
        let block_id = match id {
            None => return Self::zero_ledger_info_placeholder(),
            Some(id) => id,
        };
        let block = match self.get_block(block_id) {
            Some(b) => b,
            None => {
                return Self::zero_ledger_info_placeholder();
            }
        };
        let (state_id, version) = match self.get_state_for_block(block_id) {
            Some(state) => (state.state_id, state.version),
            None => {
                return Self::zero_ledger_info_placeholder();
            }
        };
        LedgerInfo::new(
            version,
            state_id,
            HashValue::zero(),
            block_id,
            0, // TODO [Reconfiguration] use the real epoch number.
            block.timestamp_usecs(),
        )
    }

    /// Used in case we're using a ledger info just as a placeholder for signing the votes / QCs
    /// and there is no real block committed.
    /// It's all pretty much zeroes.
    fn zero_ledger_info_placeholder() -> LedgerInfo {
        LedgerInfo::new(
            0,
            HashValue::zero(),
            HashValue::zero(),
            HashValue::zero(),
            0,
            0,
        )
    }

    fn verify_and_get_parent_info(
        &self,
        block: &Block<T>,
    ) -> Result<(HashValue, u64), InsertError> {
        if block.round() <= self.inner.read().unwrap().root().round() {
            return Err(InsertError::OldBlock);
        }

        let block_hash = block.hash();
        if block.id() != block_hash {
            return Err(InsertError::InvalidBlockHash);
        }

        if block.quorum_cert().certified_block_id() != block.parent_id() {
            return Err(InsertError::ParentNotCertified);
        }

        let parent = match self.inner.read().unwrap().get_block(block.parent_id()) {
            None => {
                return Err(InsertError::MissingParentBlock(block.parent_id()));
            }
            Some(parent) => parent,
        };
        if parent.height() + 1 != block.height() {
            return Err(InsertError::InvalidBlockHeight);
        }
        if parent.round() >= block.round() {
            return Err(InsertError::InvalidBlockRound);
        }
        if self.enforce_increasing_timestamps && parent.timestamp_usecs() >= block.timestamp_usecs()
        {
            return Err(InsertError::NonIncreasingTimestamp);
        }
        let parent_id = parent.id();
        match self.inner.read().unwrap().get_state_for_block(parent_id) {
            Some(ExecutedState { version, .. }) => Ok((parent.id(), version)),
            None => Err(InsertError::ParentVersionNotFound),
        }
    }
}

impl<T: Payload> BlockReader for BlockStore<T> {
    type Payload = T;

    fn block_exists(&self, block_id: HashValue) -> bool {
        self.inner.read().unwrap().block_exists(block_id)
    }

    fn get_block(&self, block_id: HashValue) -> Option<Arc<Block<Self::Payload>>> {
        self.inner.read().unwrap().get_block(block_id)
    }

    fn get_state_for_block(&self, block_id: HashValue) -> Option<ExecutedState> {
        self.inner.read().unwrap().get_state_for_block(block_id)
    }

    fn get_compute_result(&self, block_id: HashValue) -> Option<Arc<StateComputeResult>> {
        self.inner.read().unwrap().get_compute_result(block_id)
    }

    fn root(&self) -> Arc<Block<Self::Payload>> {
        self.inner.read().unwrap().root()
    }

    fn get_quorum_cert_for_block(&self, block_id: HashValue) -> Option<Arc<QuorumCert>> {
        self.inner
            .read()
            .unwrap()
            .get_quorum_cert_for_block(block_id)
    }

    fn is_ancestor(
        &self,
        ancestor: &Block<Self::Payload>,
        block: &Block<Self::Payload>,
    ) -> Result<bool, BlockTreeError> {
        self.inner.read().unwrap().is_ancestor(ancestor, block)
    }

    fn path_from_root(&self, block: Arc<Block<T>>) -> Option<Vec<Arc<Block<T>>>> {
        self.inner.read().unwrap().path_from_root(block)
    }

    fn highest_certified_block(&self) -> Arc<Block<Self::Payload>> {
        self.inner.read().unwrap().highest_certified_block()
    }

    fn highest_quorum_cert(&self) -> Arc<QuorumCert> {
        self.inner.read().unwrap().highest_quorum_cert()
    }

    fn highest_ledger_info(&self) -> Arc<QuorumCert> {
        self.inner.read().unwrap().highest_ledger_info()
    }
}
// ------------------------------------------