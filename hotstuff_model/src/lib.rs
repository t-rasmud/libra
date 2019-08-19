#![feature(async_await)]
use mirai_annotations::{assume};
use rand::Rng;

// TODO: check
const NUM_BLOCKS:usize = 10000;
const NUM_REPLICAS:usize = 10000;

type BlockId = usize;

#[derive(Copy, Clone, Hash)]
struct Block {
    parent: BlockId,
    height: usize,
    // BlockId for which QC has been gathered.
    justify: BlockId,
}

// Returns a random number between 0 and NUM_BLOCKS.
// axiom forall id: BlockId. Hash(blocks[id]) == id
// checked at use site.
fn hash(_bl: Block) -> BlockId {
    let mut rng = rand::thread_rng();
    let res:usize = rng.gen_range(0, NUM_BLOCKS);
    return res;
}

const ROOT: BlockId = 0;               // root of block tree
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

/// start things off
// f: number of faulty replicas
// h: number of honest replicas
// blocks: constant set of blocks
fn main_method(f: usize, h: usize, blocks: [Block; NUM_BLOCKS]) {
    //axiom h >= 2f + 1
    assume!(h >= 2*f + 1);
    //axiom blocks[root].height == 0
    assume!(blocks[ROOT].height == 0);

    // NUM_REPLICAS = h + f
    assume!(NUM_REPLICAS == h + f);

//    set_model_field!(&modelVoteStore[root], numVotes, h + f);
//    for i in 1..numBlocks {
//        set_model_field!(&modelVoteStore[i], numVotes, f);
//    }

    /// global model variables [TODO: convert these to model fields?]
    // all collected votes
    // initial value: lambda id: BlockId. if id == root then h+f else f (maximum equivocation)
    let vote_store: & mut [usize; NUM_BLOCKS] = &mut [0; NUM_BLOCKS];
    vote_store[ROOT] = h + f;
    for i in 1..NUM_BLOCKS {
        vote_store[i] = f;
    }

    let replica_store: & mut [ReplicaState; NUM_REPLICAS] =
        &mut [ReplicaState{vheight: 0, locked_block_id: ROOT, locally_committed: ROOT }; NUM_REPLICAS];


    let r = havoc_replica(h);
    let new_block_id = havoc_block_id();
    on_receive_proposal(h, f, blocks, r, replica_store, new_block_id, vote_store);
}

fn havoc_replica(max_replicas: usize) -> HonestReplicaId {
    let mut rng = rand::thread_rng();
    let res:usize = rng.gen_range(0, max_replicas);
    return res;
}

fn havoc_block_id() -> BlockId {
    let mut rng = rand::thread_rng();
    let res:usize = rng.gen_range(1, NUM_BLOCKS);
    return res;
}

async fn async_main(f: usize, h: usize, blocks: [Block; NUM_BLOCKS],
                    replica_store: & mut [ReplicaState; NUM_REPLICAS],
                    vote_store: & mut [usize; NUM_BLOCKS]) {
    let r = havoc_replica(h);
    let new_block_id = havoc_block_id();
    on_receive_proposal(h, f, blocks, r, replica_store, new_block_id, vote_store).await;
}

/// top-level event handler at a replica to update vheight and "send" vote
async fn on_receive_proposal(h: usize, f: usize, blocks: [Block; NUM_BLOCKS], r: HonestReplicaId,
                           replica_store: & mut [ReplicaState; NUM_REPLICAS], new_block_id : BlockId,
                             vote_store: & mut [usize; NUM_BLOCKS]) {
    let new_block: Block;
    new_block = blocks[new_block_id];

    //axiom forall id: BlockId. id == ROOT || blocks[id].height == blocks[blocks[id].parent].height + 1
    assume!(new_block_id == ROOT || blocks[new_block_id].height == blocks[blocks[new_block_id].parent].height + 1);

    let mut rng = rand::thread_rng();
    let havoc_bool:bool = rng.gen();

    if havoc_bool && vote_store[new_block.justify] >= h {
        if new_block.height > replica_store[r].vheight &&
            (extends(blocks, new_block_id, replica_store[r].locked_block_id) ||
                blocks[new_block.justify].height > blocks[replica_store[r].locked_block_id].height) {
            replica_store[r].vheight = new_block.height;
            vote_store[new_block_id] = vote_store[new_block_id] + 1;
        }
    }

    async_update(blocks, r, replica_store, new_block.justify).await;
    async_main(f, h, blocks, replica_store, vote_store);
}

/// Internal event handler at a replica to update lockedBlockId, locallyCommitted, and committed
/// and assert consensus safety
async fn async_update(blocks: [Block; NUM_BLOCKS], r: HonestReplicaId, replica_store: & mut [ReplicaState; NUM_REPLICAS], id_double_prime: BlockId) {
    let b_double_prime = blocks[id_double_prime];
    let id_prime = b_double_prime.justify;
    let b_prime = blocks[id_prime];
    let id = b_prime.justify;
    let b = blocks[id];

    //axiom forall id: BlockId. Hash(blocks[id]) == id
    assume!(hash(b_prime) == id_prime);
    assume!(hash(b) == id);

    if b_prime.height > blocks[replica_store[r].locked_block_id].height {
        replica_store[r].locked_block_id = id_prime;
    }

    if b_double_prime.parent == hash(b_prime) && b_prime.parent == hash(b) {
        assert!(consistent(blocks, b, blocks[replica_store[r].locally_committed]));
        replica_store[r].locally_committed = std::cmp::max(id, replica_store[r].locally_committed);

        unsafe{
            assert!(consistent(blocks, b, blocks[COMMITTED]));
            COMMITTED = std::cmp::max(id, COMMITTED);
        }
    }
}

fn extends(blocks: [Block; NUM_BLOCKS], child_block_id: BlockId, parent_block_id: BlockId) -> bool {
    if child_block_id < ROOT || parent_block_id < ROOT {
        return false;
    }
    if child_block_id == parent_block_id ||
        blocks[child_block_id].parent == parent_block_id {
        return true;
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