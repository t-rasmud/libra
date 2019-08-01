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

pub async fn process_new_round_events(NewRoundEvent) {
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
            self.pacemaker.current_round_deadline(),
        )
        .await
        {
            Err(e) => {
                error!("Error while generating proposal: {:?}", e);
                return;
            }
            Ok(proposal) => proposal,
        };

    let timeout_certificate = match new_round_event.reason {
        NewRoundReason::Timeout { cert } => Some(cert),
        _ => None,
    };

    let mut network = self.network.clone();
    let highest_ledger_info = (*self.block_store.highest_ledger_info()).clone();
    network
        .broadcast_proposal(ProposalInfo {
            proposal,
            proposer_info,
            timeout_certificate,
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
pub async fn process_proposals(ProposalInfo<T, P>,) {
    match guard.process_proposal(proposal_info).await {
        ProcessProposalResult::Done => (),
        // Spawn a new task that would start retrieving the missing
        // blocks in the background.

        ProcessProposalResult::NeedFetch(deadline, proposal) => executor.spawn(
            Self::fetch_and_process_proposal(
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
                proposal,
            )
                .boxed()
                .unit_error()
                .compat(),
        ),
    }
}

// Internal concurrency----------

/// The function is responsible for processing the incoming proposals and the Quorum
/// Certificate. 1. commit to the committed state the new QC carries
/// 2. fetch all the blocks from the committed state to the QC
/// 3. forwarding the proposals to the ProposerElection queue,
/// which is going to eventually trigger one winning proposal per round
/// (to be processed via a separate function).
/// The reason for separating `process_proposal` from `process_winning_proposal` is to
/// (a) asynchronously prefetch dependencies and
/// (b) allow the proposer election to choose one proposal out of many.
pub async fn process_proposal(
    proposal: ProposalInfo<T, P>,
) -> ProcessProposalResult<T, P> {
    debug!("Receive proposal {}", proposal);
    let current_round = self.pacemaker.current_round();
    if proposal.proposal.round() < self.pacemaker.current_round() {
        warn!(
            "Proposal {} is ignored because its round {} != current round {}",
            proposal,
            proposal.proposal.round(),
            current_round
        );
        return ProcessProposalResult::Done;
    }
    if self
        .proposer_election
        .is_valid_proposer(proposal.proposer_info, proposal.proposal.round())
        .is_none()
    {
        warn!(
            "Proposer {} for block {} is not a valid proposer for this round",
            proposal.proposal.author(),
            proposal.proposal
        );
        return ProcessProposalResult::Done;
    }

    let deadline = self.pacemaker.current_round_deadline();
    if let Some(committed_block_id) = proposal.highest_ledger_info.committed_block_id() {
        if self
            .block_store
            .need_sync_for_quorum_cert(committed_block_id, &proposal.highest_ledger_info)
        {
            return ProcessProposalResult::NeedSync(deadline, proposal);
        }
    } else {
        warn!("Highest ledger info {} has no committed block", proposal);
        return ProcessProposalResult::Done;
    }

    match self
        .block_store
        .need_fetch_for_quorum_cert(proposal.proposal.quorum_cert())
        {
            NeedFetchResult::NeedFetch => {
                return ProcessProposalResult::NeedFetch(deadline, proposal)
            }
            NeedFetchResult::QCRoundBeforeRoot => {
                warn!("Proposal {} has a highest quorum certificate with round older than root round {}", proposal, self.block_store.root().round());
                return ProcessProposalResult::Done;
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
                    return ProcessProposalResult::Done;
                }
            }
            NeedFetchResult::QCAlreadyExist => (),
        }

    self.finish_proposal_processing(proposal).await;
    ProcessProposalResult::Done
}

/// Finish proposal processing: note that multiple tasks can execute this function in parallel
/// so be careful with the updates. The safest thing to do is to pass the proposal further
/// to the proposal election.
/// This function is invoked when all the dependencies for the given proposal are ready.
async fn finish_proposal_processing(proposal: ProposalInfo<T, P>) {
    let qc = proposal.proposal.quorum_cert();
    self.pacemaker
        .process_certificates(
            qc.certified_block_round(),
            proposal.timeout_certificate.as_ref(),
        )
        .await;

    let current_round = self.pacemaker.current_round();
    if self.pacemaker.current_round() != proposal.proposal.round() {
        warn!(
            "Proposal {} is ignored because its round {} != current round {}",
            proposal,
            proposal.proposal.round(),
            current_round
        );
        return;
    }

    self.proposer_election.process_proposal(proposal).await;
}

/// Fetches and completes processing proposal in dedicated task
async fn fetch_and_process_proposal(
    proposal: ProposalInfo<T, P>,
) {
    if let Err(e) = self
        .sync_manager
        .fetch_quorum_cert(
            proposal.proposal.quorum_cert().clone(),
            proposal.proposer_info.get_author(),
            deadline,
        )
        .await
    {
        warn!(
            "Quorum certificate for proposal {} could not be added to the block store: {:?}",
            proposal, e
        );
        return;
    }
    self.finish_proposal_processing(proposal).await;
}

/// Takes mutable reference to avoid race with other processing and perform state
/// synchronization, then completes processing proposal in dedicated task
async fn sync_and_process_proposal(
    proposal: ProposalInfo<T, P>,
) {
    // check if we still need sync
    if let Err(e) = self
        .sync_manager
        .sync_to(
            deadline,
            SyncInfo {
                highest_ledger_info: proposal.highest_ledger_info.clone(),
                highest_quorum_cert: proposal.proposal.quorum_cert().clone(),
                peer: proposal.proposer_info.get_author(),
            },
        )
        .await
    {
        warn!(
            "Quorum certificate for proposal {} could not be added to the block store: {:?}",
            proposal, e
        );
        return;
    }
    self.finish_proposal_processing(proposal).await;
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

    // Update the pacemaker with the highest committed round so that on the next round
    // duration it calculates, the initial round index is reset
    self.pacemaker
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

// ----------------------------------

/// This function processes a proposal that was chosen as a representative of its round:
/// 1. Add it to a block store.
/// 2. Try to vote for it following the safety rules.
/// 3. In case a validator chooses to vote, send the vote to the representatives at the next
/// position.
async fn process_winning_proposals(ProposalInfo<T, P>,
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

    // Checking pacemaker round again, because multiple proposal can now race
    // during async block retrieval
    if self.pacemaker.current_round() != block.round() {
        debug!(
            "Skip voting for winning proposal {} rejected because round is incorrect. Pacemaker: {}, proposal: {}",
            block,
            self.pacemaker.current_round(),
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
    if let Err(e) = self
        .storage
        .save_consensus_state(vote_info.consensus_state().clone())
    {
        debug!("Fail to persist consensus state: {:?}", e);
        return;
    }
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
                self.pacemaker
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

    // ---- Updates current round and creates NewRoundEvent (see functions below in "Helper functions")
    self.pacemaker
        .process_remote_timeout(timeout_msg.pacemaker_timeout().clone())
        .await;
}

/// The replica stops voting for this round and saves its consensus state.  Voting is halted
/// to ensure that the next proposer can make a proposal that can be voted on by all replicas.
///// Saving the consensus state ensures that on restart, the replicas will not waste time
///// on previous rounds.
async fn process_outgoing_pacemaker_timeouts(Round
) {
    while let Some(round) = receiver.next().await {
        // Update the last voted round and generate the timeout message
        let timeout_msg = process_outgoing_pacemaker_timeout(round).await;
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

/// The replica stops voting for this round and saves its consensus state.  Voting is halted
/// to ensure that the next proposer can make a proposal that can be voted on by all replicas.
/// Saving the consensus state ensures that on restart, the replicas will not waste time
/// on previous rounds.
pub async fn process_outgoing_pacemaker_timeout(round: Round) -> Option<TimeoutMsg> {
    // Stop voting at this round, persist the consensus state to support restarting from
    // a recent round (i.e. > the last vote round)  and then send the highest quorum
    // certificate known
    let consensus_state = self
        .safety_rules
        .write()
        .unwrap()
        .increase_last_vote_round(round);
    if let Some(consensus_state) = consensus_state {
        if let Err(e) = self.storage.save_consensus_state(consensus_state) {
            error!("Failed to persist consensus state after increasing the last vote round due to {:?}", e);
            return None;
        }
    }

    let last_vote_round = self
        .safety_rules
        .read()
        .unwrap()
        .consensus_state()
        .last_vote_round();
    warn!(
        "Round {} timed out and {}, expected round proposer was {:?}, broadcasting new round to all replicas",
        round,
        if last_vote_round == round { "already executed and voted at this round" } else { "will never vote at this round" },
        self.proposer_election.get_valid_proposers(round),
    );

    Some(TimeoutMsg::new(
        self.block_store.highest_quorum_cert().as_ref().clone(),
        self.block_store.highest_ledger_info().as_ref().clone(),
        PacemakerTimeout::new(round, self.block_store.signer()),
        self.block_store.signer(),
    ))
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