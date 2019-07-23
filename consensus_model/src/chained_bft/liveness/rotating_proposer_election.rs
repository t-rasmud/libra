// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::chained_bft::{
    common::{Payload, Round},
    liveness::proposer_election::{ProposalInfo, ProposerElection, ProposerInfo},
};
use channel;
use futures::{Future, FutureExt, SinkExt};
use logger::prelude::*;
use std::pin::Pin;

/// The rotating proposer maps a round to an author according to a round-robin rotation.
/// A fixed proposer strategy loses liveness when the fixed proposer is down. Rotating proposers
/// won't gather quorum certificates to machine loss/byzantine behavior on f/n rounds.
pub struct RotatingProposer<T, P> {
    // Ordering of proposers to rotate through (all honest replicas must agree on this)
    proposers: Vec<P>,
    // Number of contiguous rounds (i.e. round numbers increase by 1) a proposer is active
    // in a row
    contiguous_rounds: u32,
    // Output stream to send the chosen proposals
    winning_proposals_sender: channel::Sender<ProposalInfo<T, P>>,
}