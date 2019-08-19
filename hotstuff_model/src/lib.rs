#![feature(async_await)]
use mirai_annotations::{assume, assumed_postcondition, precondition, result};

const NUM_BLOCKS:usize = 10000;
const NUM_REPLICAS:usize = 10000;

type BlockId = usize;

#[derive(Copy, Clone, Hash)]
struct Block {
    parent: BlockId,
    // Distance from the root (genesis) block.
    height: usize,
    // BlockId for which QC has been gathered and is
    // known to the leader that's proposing this block.
    justify: BlockId,
    // Unique id for this block.
    id: BlockId,
}

// axiom forall id: BlockId. Hash(blocks[id]) == id
fn hash(bl: Block) -> BlockId {
    return bl.id;
}

// root of block tree
const ROOT: BlockId = 0;
// ids of honest replicas (1..h)
type HonestReplicaId = usize;

// id of the latest globally-committed block
// initial value: root
static mut COMMITTED:BlockId = ROOT;

/// per-replica state
#[derive(Copy, Clone)]
struct ReplicaState {
    // monotonically increasing height of the last voted block
    // initial value: 0
    vheight: usize,
    // id of the locked block
    // initial value: root
    locked_block_id: BlockId,
    // id of the latest locally-committed block
    // initial value: root
    locally_committed: BlockId,
}

struct VoteStore {
    map_block_votes: &'static mut [usize; NUM_BLOCKS],
}

struct ReplicaStore {
    map_replica_state: &'static mut [ReplicaState; NUM_REPLICAS],
}

/// start things off
// f: number of faulty replicas
// h: number of honest replicas
// blocks: constant set of blocks
// TODO: reference arrays
fn main(f: usize, h: usize, blocks: [Block; NUM_BLOCKS], vote_store: VoteStore, replica_store: &mut ReplicaStore) {
    //axiom h >= 2f + 1
    precondition!(h >= 2*f + 1);
    //axiom blocks[root].height == 0
    precondition!(blocks[ROOT].height == 0);

    // NUM_REPLICAS = h + f
    precondition!(NUM_REPLICAS == h + f);

//    set_model_field!(&modelVoteStore[root], numVotes, h + f);
//    for i in 1..numBlocks {
//        set_model_field!(&modelVoteStore[i], numVotes, f);
//    }


    // global model variables [TODO: convert these to model fields?]
    // all collected votes
    // initial value: lambda id: BlockId. if id == root then h+f else f (maximum equivocation)
    vote_store.map_block_votes[ROOT] = h + f;
    for i in 1..NUM_BLOCKS {
        vote_store.map_block_votes[i] = f;
    }

    for i in 0..NUM_REPLICAS {
        replica_store.map_replica_state[i] = ReplicaState{vheight: 0, locked_block_id: ROOT, locally_committed: ROOT};
    }      &mut [ReplicaState{vheight: 0, locked_block_id: ROOT, locally_committed: ROOT }; NUM_REPLICAS];


    let r = havoc_replica(h);
    let new_block_id = havoc_block_id();
    on_receive_proposal(h, f, blocks, r, replica_store, new_block_id, vote_store);
}

fn havoc_replica(max_replicas: usize) -> HonestReplicaId {
    let res = result!();
    assumed_postcondition!(res <= max_replicas);
    return res;
}

fn havoc_block_id() -> BlockId {
    let res = result!();
    assumed_postcondition!(res >= 1 && res <= NUM_BLOCKS);
    return res;
}

fn havoc_bool() -> bool {
    return result!();
}

fn block_invariant(blocks: [Block; NUM_BLOCKS], block_id: BlockId) -> bool {
    return block_id == ROOT || blocks[block_id].height == blocks[blocks[block_id].parent].height + 1;
}

//TODO: introduce async + static contracts
fn main_loop(f: usize, h: usize, blocks: [Block; NUM_BLOCKS],
             replica_store: &mut ReplicaStore,
             vote_store: VoteStore) {
    let r = havoc_replica(h);
    let new_block_id = havoc_block_id();
    on_receive_proposal(h, f, blocks, r, replica_store, new_block_id, vote_store);
}

/// top-level event handler at a replica to update vheight and "send" vote
fn on_receive_proposal(h: usize, f: usize, blocks: [Block; NUM_BLOCKS], r: HonestReplicaId,
                       replica_store: &mut ReplicaStore, new_block_id : BlockId,
                       vote_store: VoteStore) {
    let new_block: Block;
    new_block = blocks[new_block_id];

    //axiom forall id: BlockId. id == ROOT || blocks[id].height == blocks[blocks[id].parent].height + 1
    precondition!(block_invariant(blocks, new_block_id) == true);

    let nondet_bool:bool = havoc_bool();

    if nondet_bool && vote_store.map_block_votes[new_block.justify] >= h {
        if new_block.height > replica_store.map_replica_state[r].vheight &&
            (extends(blocks, new_block_id, replica_store.map_replica_state[r].locked_block_id) ||
                blocks[new_block.justify].height > blocks[replica_store.map_replica_state[r].locked_block_id].height) {
            replica_store.map_replica_state[r].vheight = new_block.height;
            vote_store.map_block_votes[new_block_id] = vote_store.map_block_votes[new_block_id] + 1;
        }
    }

    update(blocks, r, replica_store, new_block.justify);
    main_loop(f, h, blocks, replica_store, vote_store);
}

/// Internal event handler at a replica to update lockedBlockId, locallyCommitted, and committed
/// and assert consensus safety
fn update(blocks: [Block; NUM_BLOCKS], r: HonestReplicaId, replica_store: &mut ReplicaStore, id_double_prime: BlockId) {
    let b_double_prime = blocks[id_double_prime];
    let id_prime = b_double_prime.justify;
    let b_prime = blocks[id_prime];
    let id = b_prime.justify;
    let b = blocks[id];

    //axiom forall id: BlockId. Hash(blocks[id]) == id
    assume!(hash(b_prime) == id_prime);
    assume!(hash(b) == id);

    assume!(hash(blocks[replica_store.map_replica_state[r].locally_committed]) == replica_store.map_replica_state[r].locally_committed);

    if b_prime.height > blocks[replica_store.map_replica_state[r].locked_block_id].height {
        replica_store.map_replica_state[r].locked_block_id = id_prime;
    }

    if b_double_prime.parent == hash(b_prime) && b_prime.parent == hash(b) {
        assert!(consistent(blocks, b, blocks[replica_store.map_replica_state[r].locally_committed]));
        replica_store.map_replica_state[r].locally_committed = std::cmp::max(id, replica_store.map_replica_state[r].locally_committed);

        unsafe{
            assume!(hash(blocks[COMMITTED]) == COMMITTED);
            assert!(consistent(blocks, b, blocks[COMMITTED]));
            COMMITTED = std::cmp::max(id, COMMITTED);
        }
    }
}

fn extends(blocks: [Block; NUM_BLOCKS], child_block_id: BlockId, parent_block_id: BlockId) -> bool {
    if child_block_id == parent_block_id ||
        blocks[child_block_id].parent == parent_block_id {
        return true;
    }
    if child_block_id < ROOT || parent_block_id < ROOT {
        return false;
    }
    return extends(blocks, blocks[child_block_id].parent, parent_block_id);
}

fn consistent(blocks: [Block; NUM_BLOCKS], b1: Block, b2: Block) -> bool {
    if extends(blocks, hash(b1), hash(b2)) ||
        extends(blocks, hash(b2), hash(b1)) {
        return true;
    }
    return false;
}