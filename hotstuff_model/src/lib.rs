//type HonestReplicaId = 1..h // ids of honest replicas

#![feature(async_await)]
use mirai_annotations::assume;
use std::env;
use rand::Rng;

// TODO: check
const numBlocks:usize = 10000;
const numReplicas:usize = 10000;

type BlockId = usize;

#[derive(Copy, Clone)]
struct Block {
    parent: BlockId,
    height: usize,
    justify: BlockId,
}

fn hash(bl: Block) -> BlockId {
    return 0;
}

static root: BlockId = 0;               // root of block tree
type HonestReplicaId = usize;


/// global model variables [TODO: convert these to model fields]
// all collected votes
// initial value: lambda id: BlockId. if id == root then h+f else f (maximum equivocation)
static mut voteStore: &'static mut [usize; numBlocks] = &mut [0; numBlocks];

// id of the latest globally-committed block
// initial value: root
static mut committed:BlockId = root;

/// per-replica state
#[derive(Copy, Clone)]
struct ReplicaState {
    // monotonically increasing height of the last voted block
    // initial value: 0
    vheight: usize,
    // id of the locked block
    // initial value: root
    lockedBlockId: BlockId,
    // id of the latest locally-committed block
    // initial value: root
    locallyCommitted: BlockId,
}

/// start things off
// f: number of faulty replicas
// h: number of honest replicas
// blocks: constant set of blocks
fn main_method(f: usize, h: usize, blocks: [Block; numBlocks]) {

    let args: Vec<String> = env::args().collect();

    //axiom h >= 2f + 1
    assume!(h >= 2*f + 1);
    //axiom blocks[root].height == 0
    assume!(blocks[root].height == 0);

    // initial value: lambda id: BlockId. if id == root then h+f else f (maximum equivocation)
    unsafe{
        voteStore[root] = h + f;
        for i in 1..numBlocks {
            voteStore[i] = f;
        }
    }

    let mut replicaStore: & mut [ReplicaState; numReplicas] =
        &mut [ReplicaState{vheight: 0, lockedBlockId: root, locallyCommitted: root}; numReplicas];


    let r = havocReplica(h);
    let newBlockId = havocBlockId();
    OnReceiveProposal(h, f, blocks, r, replicaStore, newBlockId);
}

fn havocReplica(maxReplicas: usize) -> HonestReplicaId {
    let mut rng = rand::thread_rng();
    let res:usize = rng.gen_range(0, numReplicas);
    return res;
}

fn havocBlockId() -> BlockId {
    let mut rng = rand::thread_rng();
    let res:usize = rng.gen_range(1, numBlocks);
    return res;
}

async fn async_main(f: usize, h: usize, blocks: [Block; numBlocks], replicaStore: & mut [ReplicaState; numReplicas]) {
    let r = havocReplica(h);
    let newBlockId = havocBlockId();
    OnReceiveProposal(h, f, blocks, r, replicaStore, newBlockId);
}

/// top-level event handler at a replica to update vheight and "send" vote

async fn OnReceiveProposal(h: usize, f: usize, blocks: [Block; numBlocks], r: HonestReplicaId,
                           replicaStore: & mut [ReplicaState; numReplicas], newBlockId : BlockId) {
    let newBlock: Block;
    newBlock = blocks[newBlockId];

    //axiom forall id: BlockId. id == root || blocks[id].height == blocks[blocks[id].parent].height + 1
    assume!(newBlockId == root || blocks[newBlockId].height == blocks[blocks[newBlockId].parent].height + 1);

    unsafe{
        let mut rng = rand::thread_rng();
        let havocBool:bool = rng.gen();

        if havocBool && voteStore[newBlock.justify] >= h {
            if newBlock.height > replicaStore[r].vheight &&
                (extends(blocks, newBlockId, replicaStore[r].lockedBlockId) ||
                    blocks[newBlock.justify].height > blocks[replicaStore[r].lockedBlockId].height) {
                replicaStore[r].vheight = newBlock.height;
                voteStore[newBlockId] = voteStore[newBlockId] + 1;
            }
        }
    }
    async_update(blocks, r, replicaStore, newBlock.justify);
    async_main(f, h, blocks, replicaStore);
}

/// Internal event handler at a replica to update lockedBlockId, locallyCommitted, and committed
/// and assert consensus safety
async fn async_update(blocks: [Block; numBlocks], r: HonestReplicaId, replicaStore: & mut [ReplicaState; numReplicas], id_double_prime: BlockId) {
    let b_double_prime = blocks[id_double_prime];
    let id_prime = b_double_prime.justify;
    let b_prime = blocks[id_prime];
    let id = b_prime.justify;
    let b = blocks[id];

    //axiom forall id: BlockId. Hash(blocks[id]) == id
    assume!(hash(b_prime) == id_prime);
    assume!(hash(b) == id);

    unsafe{
        if b_prime.height > blocks[replicaStore[r].lockedBlockId].height {
            replicaStore[r].lockedBlockId = id_prime;
        }
    }
    if b_double_prime.parent == hash(b_prime) && b_prime.parent == hash(b) {
        assert!(consistent(blocks, b, blocks[replicaStore[r].locallyCommitted]));
        replicaStore[r].locallyCommitted = std::cmp::max(id, replicaStore[r].locallyCommitted);

        unsafe{
            assert!(consistent(blocks, b, blocks[committed]));
            committed = std::cmp::max(id, committed);
        }
    }
}

fn extends(blocks: [Block; numBlocks], child_block_id: BlockId, parent_block_id: BlockId) -> bool {
    if child_block_id < root || parent_block_id < root {
        return false;
    }
    if child_block_id == parent_block_id ||
        blocks[child_block_id].parent == parent_block_id {
        return true;
    }
    return extends(blocks, blocks[child_block_id].parent, parent_block_id);
}

fn consistent(blocks: [Block; numBlocks], b1: Block, b2: Block) -> bool {
    if extends(blocks, hash(b1), hash(b2)) ||
        extends(blocks, hash(b2), hash(b1)) {
        return true;
    }
    return false;
}