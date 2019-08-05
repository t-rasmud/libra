// From EventProcessor,

pub struct NodeState {
    author: P,
    proposal_generator: ProposalGenerator<T>,
    safety_rules: Arc<RwLock<SafetyRules<T>>>,
    block_store: Arc<BlockStore<T>>,
}

// Structures contained in NodeState
pub struct ProposalGenerator<T> {
    // Block store is queried both for finding the branch to extend and for generating the
    // proposed block.
    block_store: Arc<dyn BlockReader<Payload = T> + Send + Sync>,
    // Last round that a proposal was generated
    last_round_generated: Mutex<Round>,
}

pub struct BlockStore<T> {
    inner: Arc<RwLock<BlockTree<T>>>,
    validator_signer: ValidatorSigner<Ed25519PrivateKey>,
    state_computer: Arc<dyn StateComputer<Payload = T>>,
    enforce_increasing_timestamps: bool,
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