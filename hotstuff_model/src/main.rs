
//axiom forall id: BlockId. Hash(blocks[id]) == id
//axiom blocks[root].height == 0
//axiom forall id: BlockId. id == root || blocks[id].height == blocks[blocks[id].parent].height + 1

#![feature(async_await)]

use mirai_annotations::{assumed_postcondition, result};

static root: BlockId = 0;

type BlockId = usize;

#[derive(Copy, Clone)]
struct Block {
    parent: BlockId,
    height: usize,
    justify: BlockId,
}
type ReplicaId = usize;

fn hash(bl: Block) -> BlockId {
    let res = result!();
    return res;
}

// global model variables [TODO: convert these to model fields]
static mut voteStore: &'static mut [usize; 10000] = &mut [0; 10000];        // initially: lambda BlockId. f
static mut committed:BlockId = 0;                                               // initially: root

fn main() {
    let f: usize = result!();
    let n: usize = 2*f + 1;
    let blocks: [Block; 10000] = result!();

    loop {
        let r = havocReplica(n + f);
        let newBlockId = havocBlockId();
        OnReceiveProposal(n, f, blocks, r, newBlockId);
    }
}

fn havocReplica(maxReplicas: usize) -> ReplicaId {
    let res = result!();
    assumed_postcondition!(res >=1 && res <= maxReplicas);
    return res;
}

fn havocBlockId() -> BlockId {
    result!()
}

// per-replica state
static mut replicaStore: &'static mut [usize; 10000] = &mut [0; 10000];
struct ReplicaState {
    lockedBlockId: BlockId,         // initially root
    vheight: usize,                  // initially 0
    locallyCommitted: BlockId,      // initially: root
}

//per-replica code
async fn OnReceiveProposal(n: usize, f: usize, blocks: [Block; 10000], r: ReplicaId, newBlockId : BlockId) {
    let newBlock: Block;
    newBlock = blocks[newBlockId];

    unsafe{
        if voteStore[newBlock.justify] < n {
            return
        }
    }
    if newBlock.height > vheight[r] &&
        (extends(newBlockId, lockedBlockId[r]) ||
            blocks[newBlock.justify].height > blocks[lockedBlockId[r]].height) {
        vheight[r] = newBlock.height;

        unsafe{
            voteStore[newBlockId] = voteStore[newBlockId] + 1;
        }
  }
  Update(blocks, r, newBlock.justify)
}

fn Update(blocks: [Block; 10000], r: ReplicaId, id_double_prime: BlockId) {
    let b_double_prime = blocks[id_double_prime];
    let id_prime = b_double_prime.justify;
    let b_prime = blocks[id_prime];
    let id = b_prime.justify;
    let b = blocks[id];
    if b_prime.height > blocks[lockedBlockId[r]].height {
        lockedBlockId[r] = id_prime;
    }
    if b_double_prime.parent == hash(b_prime) && b_prime.parent == hash(b) {
        assert!(extends(b, locallyCommitted[r]));
        locallyCommitted[r] = b;
        unsafe{
            if reaches(b, committed) {
                committed = hash(b);
            } else {
                assert!(reaches(committed, b));
            }
        }
    }
}