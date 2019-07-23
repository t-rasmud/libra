// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{
    chained_bft::{
        block_storage::BlockRetrievalFailure,
        common::{Author, Payload},
        consensus_types::{block::Block, quorum_cert::QuorumCert},
        liveness::{
            proposer_election::{ProposalInfo, ProposerInfo},
            timeout_msg::TimeoutMsg,
        },
        safety::vote_msg::VoteMsg,
    },
    counters,
};
use bytes::Bytes;
use channel;
use crypto::HashValue;
use failure;
use futures::{
    channel::oneshot, stream::select, FutureExt, SinkExt, Stream, StreamExt, TryFutureExt,
    TryStreamExt,
};
use logger::prelude::*;
use network::{
    proto::{BlockRetrievalStatus, ConsensusMsg, RequestBlock, RespondBlock, RespondChunk},
    validator_network::{ConsensusNetworkEvents, ConsensusNetworkSender, Event, RpcError},
};
use nextgen_crypto::ed25519::*;
use proto_conv::{FromProto, IntoProto};
use protobuf::Message;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::runtime::TaskExecutor;
use types::{transaction::TransactionListWithProof, validator_verifier::ValidatorVerifier};

/// The response sent back from EventProcessor for the BlockRetrievalRequest.
#[derive(Debug)]
pub struct BlockRetrievalResponse<T> {
    pub status: BlockRetrievalStatus,
    pub blocks: Vec<Block<T>>,
}

impl<T: Payload> BlockRetrievalResponse<T> {
    pub fn verify(&self, mut block_id: HashValue, num_blocks: u64) -> Result<(), failure::Error> {
        if self.status == BlockRetrievalStatus::SUCCEEDED && self.blocks.len() as u64 != num_blocks
        {
            return Err(format_err!(
                "not enough blocks returned, expect {}, get {}",
                num_blocks,
                self.blocks.len(),
            ));
        }
        for block in self.blocks.iter() {
            if block.id() != block_id {
                return Err(format_err!(
                    "blocks doesn't form a chain: expect {}, get {}",
                    block.id(),
                    block_id
                ));
            }
            block_id = block.parent_id();
        }
        Ok(())
    }
}

/// BlockRetrievalRequest carries a block id for the requested block as well as the
/// oneshot sender to deliver the response.
pub struct BlockRetrievalRequest<T> {
    pub block_id: HashValue,
    pub num_blocks: u64,
    pub response_sender: oneshot::Sender<BlockRetrievalResponse<T>>,
}

/// Represents a request to get up to batch_size transactions starting from start_version
/// with the oneshot sender to deliver the response.
pub struct ChunkRetrievalRequest {
    pub start_version: u64,
    pub target: QuorumCert,
    pub batch_size: u64,
    pub response_sender: oneshot::Sender<Result<TransactionListWithProof, failure::Error>>,
}

/// Just a convenience struct to keep all the network proxy receiving queues in one place.
/// 1. proposals
/// 2. votes
/// 3. block retrieval requests (the request carries a oneshot sender for returning the Block)
/// 4. pacemaker timeouts
/// Will be returned by the networking trait upon startup.
pub struct NetworkReceivers<T, P> {
    pub proposals: channel::Receiver<ProposalInfo<T, P>>,
    pub votes: channel::Receiver<VoteMsg>,
    pub block_retrieval: channel::Receiver<BlockRetrievalRequest<T>>,
    pub timeout_msgs: channel::Receiver<TimeoutMsg>,
    pub chunk_retrieval: channel::Receiver<ChunkRetrievalRequest>,
}

/// Implements the actual networking support for all consensus messaging.
pub struct ConsensusNetworkImpl {
    author: Author,
    network_sender: ConsensusNetworkSender,
    network_events: Option<ConsensusNetworkEvents>,
    // Self sender and self receivers provide a shortcut for sending the messages to itself.
    // (self sending is not supported by the networking API).
    // Note that we do not support self rpc requests as it might cause infinite recursive calls.
    self_sender: channel::Sender<Result<Event<ConsensusMsg>, failure::Error>>,
    self_receiver: Option<channel::Receiver<Result<Event<ConsensusMsg>, failure::Error>>>,
    peers: Arc<Vec<Author>>,
    validator: Arc<ValidatorVerifier<Ed25519PublicKey>>,
}

impl Clone for ConsensusNetworkImpl {
    fn clone(&self) -> Self {
        Self {
            author: self.author,
            network_sender: self.network_sender.clone(),
            network_events: None,
            self_sender: self.self_sender.clone(),
            self_receiver: None,
            peers: self.peers.clone(),
            validator: Arc::clone(&self.validator),
        }
    }
}

impl ConsensusNetworkImpl {

    /// Tries to retrieve num of blocks backwards starting from id from the given peer: the function
    /// returns a future that is either fulfilled with BlockRetrievalResponse, or with a
    /// BlockRetrievalFailure.
    pub async fn request_block<T: Payload>(
        &mut self,
        block_id: HashValue,
        num_blocks: u64,
        from: Author,
        timeout: Duration,
    ) -> Result<BlockRetrievalResponse<T>, BlockRetrievalFailure> {
        if from == self.author {
            return Err(BlockRetrievalFailure::SelfRetrieval);
        }
        let mut req_msg = RequestBlock::new();
        req_msg.set_block_id(block_id.into());
        req_msg.set_num_blocks(num_blocks);
        counters::BLOCK_RETRIEVAL_COUNT.inc_by(num_blocks as i64);
        let pre_retrieval_instant = Instant::now();

        let mut res_block = self
            .network_sender
            .request_block(from, req_msg, timeout)
            .await?;
        let mut blocks = vec![];
        for block in res_block.take_blocks().into_iter() {
            if let Ok(block) = Block::from_proto(block) {
                if block.verify(self.validator.as_ref()).is_err() {
                    return Err(BlockRetrievalFailure::InvalidSignature);
                }
                blocks.push(block);
            } else {
                return Err(BlockRetrievalFailure::InvalidResponse);
            }
        }
        counters::BLOCK_RETRIEVAL_DURATION_MS
            .observe(pre_retrieval_instant.elapsed().as_millis() as f64);
        let response = BlockRetrievalResponse {
            status: res_block.get_status(),
            blocks,
        };
        if response.verify(block_id, num_blocks).is_err() {
            return Err(BlockRetrievalFailure::InvalidResponse);
        }
        Ok(response)
    }

    /// Tries to send the given proposal (block and proposer metadata) to all the participants.
    /// A validator on the receiving end is going to be notified about a new proposal in the
    /// proposal queue.
    ///
    /// The future is fulfilled as soon as the message put into the mpsc channel to network
    /// internal(to provide back pressure), it does not indicate the message is delivered or sent
    /// out. It does not give indication about when the message is delivered to the recipients,
    /// as well as there is no indication about the network failures.
    pub async fn broadcast_proposal<T: Payload, P: ProposerInfo>(
        &mut self,
        proposal: ProposalInfo<T, P>,
    ) {
        let mut msg = ConsensusMsg::new();
        msg.set_proposal(proposal.into_proto());
        self.broadcast(msg).await
    }

    async fn broadcast(&mut self, msg: ConsensusMsg) {
        for peer in self.peers.iter() {
            if self.author == *peer {
                let self_msg = Event::Message((self.author, msg.clone()));
                if let Err(err) = self.self_sender.send(Ok(self_msg)).await {
                    error!("Error delivering a self proposal: {:?}", err);
                }
                continue;
            }
            if let Err(err) = self.network_sender.send_to(*peer, msg.clone()).await {
                error!(
                    "Error broadcasting proposal to peer: {:?}, error: {:?}, msg: {:?}",
                    peer, err, msg
                );
            }
        }
    }

    /// Sends the vote to the chosen recipients (typically that would be the recipients that
    /// we believe could serve as proposers in the next round). The recipients on the receiving
    /// end are going to be notified about a new vote in the vote queue.
    ///
    /// The future is fulfilled as soon as the message put into the mpsc channel to network
    /// internal(to provide back pressure), it does not indicate the message is delivered or sent
    /// out. It does not give indication about when the message is delivered to the recipients,
    /// as well as there is no indication about the network failures.
    pub async fn send_vote(&self, vote_msg: VoteMsg, recipients: Vec<Author>) {
        let mut network_sender = self.network_sender.clone();
        let mut self_sender = self.self_sender.clone();
        let mut msg = ConsensusMsg::new();
        msg.set_vote(vote_msg.into_proto());
        for peer in recipients {
            if self.author == peer {
                let self_msg = Event::Message((self.author, msg.clone()));
                if let Err(err) = self_sender.send(Ok(self_msg)).await {
                    error!("Error delivering a self vote: {:?}", err);
                }
                continue;
            }
            if let Err(e) = network_sender.send_to(peer, msg.clone()).await {
                error!("Failed to send a vote to peer {:?}: {:?}", peer, e);
            }
        }
    }

    /// Broadcasts timeout message to all validators
    pub async fn broadcast_timeout_msg(&mut self, timeout_msg: TimeoutMsg) {
        let mut msg = ConsensusMsg::new();
        msg.set_timeout_msg(timeout_msg.into_proto());
        self.broadcast(msg).await
    }
}

struct NetworkTask<T, P, S> {
    proposal_tx: channel::Sender<ProposalInfo<T, P>>,
    vote_tx: channel::Sender<VoteMsg>,
    block_request_tx: channel::Sender<BlockRetrievalRequest<T>>,
    chunk_request_tx: channel::Sender<ChunkRetrievalRequest>,
    timeout_msg_tx: channel::Sender<TimeoutMsg>,
    all_events: S,
    validator: Arc<ValidatorVerifier<Ed25519PublicKey>>,
}