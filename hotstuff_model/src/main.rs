
//axiom forall id: BlockId. Hash(blocks[id]) == id
//axiom blocks[root].height == 0
//axiom forall id: BlockId. id == root || blocks[id].height == blocks[blocks[id].parent].height + 1

#![feature(async_await)]

use mirai_annotations::{assume, assumed_postcondition, result};
use rand::Rng;

static root: BlockId = 0;

type BlockId = u64;

#[derive(Copy, Clone)]
struct Block {
    parent: BlockId,
    height: u64,
    justify: BlockId,
}
type ReplicaId = u64;

fn hash(bl: Block) -> BlockId {
    let res = result!();
    return res;
}

fn main() {
    let f: u64 = result!();
    let n: u64 = 2*f + 1;
    let blocks: [Block; 10000] = result!();

    loop {
        let r = havocReplica(n + f);
        let newBlockId = havocBlockId();
        OnReceiveProposal(n, f, blocks, r, newBlockId);
    }
}

fn havocReplica(maxReplicas: u64) -> ReplicaId {
    let res = result!();
    assumed_postcondition!(res >=1 && res <= maxReplicas);
    return res;
}

fn havocBlockId() -> BlockId {
    result!()
}

async fn OnReceiveProposal(n: u64, f: u64, blocks: [Block; 10000], r: ReplicaId, newBlockId : BlockId) {

}