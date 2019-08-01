// From EventProcessor,

pub struct NodeState {
    author: P,
    proposal_generator: ProposalGenerator<T>,
    pacemaker: Arc<dyn Pacemaker>,
    sync_manager: SyncManager<T>,
    safety_rules: Arc<RwLock<SafetyRules<T>>>,
    block_store: Arc<BlockStore<T>>,
    storage: Arc<dyn PersistentStorage<T>>,
}

// Structures contained in NodeState
pub struct ProposalGenerator<T> {
    // Block store is queried both for finding the branch to extend and for generating the
    // proposed block.
    block_store: Arc<dyn BlockReader<Payload = T> + Send + Sync>,
    // Transaction manager is delivering the transactions.
    txn_manager: Arc<dyn TxnManager<Payload = T>>,
    // Time service to generate block timestamps
    time_service: Arc<dyn TimeService>,
    // Max number of transactions to be added to a proposed block.
    max_block_size: u64,
    // Support increasing block timestamps
    enforce_increasing_timestamps: bool,
    // Last round that a proposal was generated
    last_round_generated: Mutex<Round>,
}

pub struct BlockStore<T> {
    inner: Arc<RwLock<BlockTree<T>>>,
    validator_signer: ValidatorSigner<Ed25519PrivateKey>,
    state_computer: Arc<dyn StateComputer<Payload = T>>,
    enforce_increasing_timestamps: bool,
    /// The persistent storage backing up the in-memory data structure, every write should go
    /// through this before in-memory tree.
    storage: Arc<dyn PersistentStorage<T>>,
}

/// This structure maintains a consistent block tree of parent and children links. Blocks contain
/// parent links and are immutable.  For all parent links, a child link exists. This structure
/// should only be used internally in BlockStore.
pub struct BlockTree<T> {
    /// All the blocks known to this replica (with parent links)
    id_to_block: HashMap<HashValue, Arc<Block<T>>>,
    /// All child links (i.e. reverse parent links) for easy cleaning.  Note that a block may
    /// have multiple child links.  There should be id_to_blocks.len() - 1 total
    /// id_to_child entries.
    id_to_child: HashMap<HashValue, Vec<Arc<Block<T>>>>,
    /// Mapping between proposals(Block) to execution results.
    id_to_state: HashMap<HashValue, ExecutedState>,
    /// Keeps the state compute results of the executed blocks.
    /// The state compute results is calculated for all the pending blocks prior to insertion to
    /// the tree (the initial root node might not have it, because it's been already
    /// committed). The execution results are not persisted: they're recalculated again for the
    /// pending blocks upon restart.
    id_to_compute_result: HashMap<HashValue, Arc<StateComputeResult>>,
    /// Root of the tree.
    root: Arc<Block<T>>,
    /// A certified block with highest round
    highest_certified_block: Arc<Block<T>>,
    /// The quorum certificate of highest_certified_block
    highest_quorum_cert: Arc<QuorumCert>,
    /// The quorum certificate that carries a highest ledger info
    highest_ledger_info: Arc<QuorumCert>,

    /// `id_to_votes` might keep multiple LedgerInfos per proposed block in order
    /// to tolerate non-determinism in execution: given a proposal, a QuorumCertificate is going
    /// to be collected only for all the votes that have identical state id.
    /// The vote digest is a hash that covers both the proposal id and the state id.
    /// Thus, the structure of `id_to_votes` is as follows:
    /// HashMap<proposed_block_id, HashMap<vote_digest, LedgerInfoWithSignatures>>
    id_to_votes: HashMap<HashValue, HashMap<HashValue, LedgerInfoWithSignatures>>,
    /// Map of block id to its completed quorum certificate (2f + 1 votes)
    id_to_quorum_cert: HashMap<HashValue, Arc<QuorumCert>>,
    /// To keep the IDs of the elements that have been pruned from the tree but not cleaned up yet.
    pruned_block_ids: VecDeque<HashValue>,
    /// Num pruned blocks to keep in memory.
    max_pruned_blocks_in_mem: usize,
}

/// SyncManager is responsible for fetching dependencies and 'catching up' for given qc/ledger info
pub struct SyncManager<T> {
    block_store: Arc<BlockStore<T>>,
    storage: Arc<dyn PersistentStorage<T>>,
    network: ConsensusNetworkImpl,
    state_computer: Arc<dyn StateComputer<Payload = T>>,
    block_mutex_map: MutexMap<HashValue>,
}

pub struct SafetyRules<T> {
    // To query about the relationships between blocks and QCs.
    block_tree: Arc<dyn BlockReader<Payload = T>>,
    // Keeps the state.
    state: ConsensusState,
}

pub struct ConsensusState {
    last_vote_round: Round,
    last_committed_round: Round,

    // A "preferred block" is the two-chain head with the highest block round.
    // We're using the `head` / `tail` terminology for describing the chains of QCs for describing
    // `head` <-- <block>* <-- `tail` chains.

    // A new proposal is voted for only if it's previous block's round is higher or equal to
    // the preferred_block_round.
    // 1) QC chains follow direct parenthood relations because a node must carry a QC to its
    // parent. 2) The "max round" rule applies to the HEAD of the chain and not its TAIL (one
    // does not necessarily apply the other).
    preferred_block_round: Round,
}

pub struct Block<T> {
    /// This block's id as a hash value
    id: HashValue,
    /// Parent block id of this block as a hash value (all zeros to indicate the genesis block)
    parent_id: HashValue,
    /// T of the block (e.g. one or more transaction(s)
    payload: T,
    /// The round of a block is an internal monotonically increasing counter used by Consensus
    /// protocol.
    round: Round,
    /// The height of a block is its position in the chain (block height = parent block height + 1)
    height: Height,
    /// Contains the quorum certified ancestor and whether the quorum certified ancestor was
    /// voted on successfully
    quorum_cert: QuorumCert,
    /// Author of the block that can be validated by the author's public key and the signature
    author: Author,
    /// Signature that the hash of this block has been authored by the owner of the private key
    signature: Ed25519Signature,
}

pub struct QuorumCert {
    /// The id of a block that is certified by this QuorumCertificate.
    certified_block_id: HashValue,
    /// The execution state of the corresponding block.
    certified_state: ExecutedState,
    /// The round of a certified block.
    certified_block_round: Round,
    /// The signed LedgerInfo of a committed block that carries the data about the certified block.
    signed_ledger_info: LedgerInfoWithSignatures,
}

// Related traits

/// Pacemaker is responsible for generating the new round events, which are driving the actions
/// of the rest of the system (e.g., for generating new proposals).
/// Ideal pacemaker provides an abstraction of a "shared clock". In reality pacemaker
/// implementations use external signals like receiving new votes / QCs plus internal
/// communication between other nodes' pacemaker instances in order to synchronize the logical
/// clocks.
/// The trait doesn't specify the starting conditions or the executor that is responsible for
/// driving the logic.
pub trait Pacemaker: Send + Sync {
    /// Returns deadline for current round
    fn current_round_deadline(&self) -> Instant;

    /// Synchronous function to return the current round.
    fn current_round(&self) -> Round;

    /// Function to update current round based on received certificates.
    /// Both round of latest received QC and timeout certificates are taken into account.
    /// This function guarantees to update pacemaker state when promise that it returns is fulfilled
    fn process_certificates(
        &self,
        qc_round: Round,
        timeout_certificate: Option<&PacemakerTimeoutCertificate>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>>;

    /// The function is invoked upon receiving a remote timeout message from another validator.
    fn process_remote_timeout(
        &self,
        pacemaker_timeout: PacemakerTimeout,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>>;

    /// Update the highest committed round
    fn update_highest_committed_round(&self, highest_committed_round: Round);
}

/// ProposerElection incorporates the logic of choosing a leader among multiple candidates.
/// We are open to a possibility for having multiple proposers per round, the ultimate choice
/// of a proposal is exposed by the election protocol via the stream of proposals.
pub trait ProposerElection<T, P> {
    /// If a given author is a valid candidate for being a proposer, generate the info,
    /// otherwise return None.
    /// Note that this function is synchronous.
    fn is_valid_proposer(&self, author: P, round: Round) -> Option<P>;

    /// Return all the possible valid proposers for a given round (this information can be
    /// used by e.g., voters for choosing the destinations for sending their votes to).
    fn get_valid_proposers(&self, round: Round) -> Vec<P>;

    /// Notify proposer election about a new proposal. The function doesn't return any information:
    /// proposer election is going to notify the client about the chosen proposal via a dedicated
    /// channel (to be passed in constructor).
    fn process_proposal(
        &self,
        proposal: ProposalInfo<T, P>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>>;
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

// Local pacemaker
/// `LocalPacemakerInner` is a Pacemaker implementation that relies on increasing local timeouts
/// in order to eventually come up with the timeout that is large enough to guarantee overlap of the
/// "current round" of multiple participants.
///
/// The protocol is as follows:
/// * `LocalPacemakerInner` manages the `highest_certified_round` that is keeping the round of the
/// highest certified block known to the validator.
/// * Once a new QC arrives with a round larger than that of `highest_certified_round`,
/// local pacemaker is going to increment a round with a default timeout.
/// * Upon every timeout `LocalPacemaker` increments a round and doubles the timeout.
///
/// `LocalPacemakerInner` does not require clock synchronization to maintain the property of
/// liveness - although clock synchronization can improve the time necessary to get a large enough
/// timeout overlap.
/// It does rely on an assumption that when an honest replica receives a quorum certificate
/// indicating to move to the next round, all other honest replicas will move to the next round
/// within a bounded time. This can be guaranteed via all honest replicas gossiping their highest
/// QC to f+1 other replicas for instance.
struct LocalPacemakerInner {
    // Determines the time interval for a round interval
    time_interval: Box<dyn PacemakerTimeInterval>,
    // Highest round that a block was committed
    highest_committed_round: Round,
    // Highest round known certified by QC.
    highest_qc_round: Round,
    // Current round (current_round - highest_qc_round determines the timeout).
    // Current round is basically max(highest_qc_round, highest_received_tc, highest_local_tc) + 1
    // update_current_round take care of updating current_round and sending new round event if
    // it changes
    current_round: Round,
    // Approximate deadline when current round ends
    current_round_deadline: Instant,
    // Service for timer
    time_service: Arc<dyn TimeService>,
    // To send new round events.
    new_round_events_sender: channel::Sender<NewRoundEvent>,
    // To send timeout events to itself.
    local_timeout_sender: channel::Sender<Round>,
    // To send timeout events to other pacemakers
    external_timeout_sender: channel::Sender<Round>,
    // Manages the PacemakerTimeout and PacemakerTimeoutCertificate structs
    pacemaker_timeout_manager: PacemakerTimeoutManager,
}
