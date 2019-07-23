// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{
    chained_bft::{
        common::Round,
        safety::vote_msg::{VoteMsg, VoteMsgVerificationError},
    },
    state_replication::ExecutedState,
};
use crypto::{
    hash::{CryptoHash, ACCUMULATOR_PLACEHOLDER_HASH, GENESIS_BLOCK_ID},
    HashValue,
};
use failure::Result;
use network::proto::QuorumCert as ProtoQuorumCert;
use nextgen_crypto::ed25519::*;
use proto_conv::{FromProto, IntoProto};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
};
use types::{
    ledger_info::{LedgerInfo, LedgerInfoWithSignatures},
    validator_signer::ValidatorSigner,
    validator_verifier::ValidatorVerifier,
};

#[derive(Deserialize, Serialize, Clone, Debug, Eq, PartialEq)]
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

impl Display for QuorumCert {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(
            f,
            "QuorumCert: [block id: {}, round: {:02}, {}]",
            self.certified_block_id, self.certified_block_round, self.signed_ledger_info
        )
    }
}

#[allow(dead_code)]
impl QuorumCert {
    pub fn new(
        block_id: HashValue,
        state: ExecutedState,
        round: Round,
        signed_ledger_info: LedgerInfoWithSignatures,
    ) -> Self {
        QuorumCert {
            certified_block_id: block_id,
            certified_state: state,
            certified_block_round: round,
            signed_ledger_info,
        }
    }

    pub fn certified_block_id(&self) -> HashValue {
        self.certified_block_id
    }

    pub fn certified_state(&self) -> ExecutedState {
        self.certified_state
    }

    pub fn certified_block_round(&self) -> Round {
        self.certified_block_round
    }

    pub fn ledger_info(&self) -> &LedgerInfoWithSignatures {
        &self.signed_ledger_info
    }

    pub fn committed_block_id(&self) -> Option<HashValue> {
        let id = self.ledger_info().ledger_info().consensus_block_id();
        if id.is_zero() {
            None
        } else {
            Some(id)
        }
    }

    pub fn verify(
        &self,
        validator: &ValidatorVerifier<Ed25519PublicKey>,
    ) -> ::std::result::Result<(), VoteMsgVerificationError> {
        let vote_hash = VoteMsg::vote_digest(
            self.certified_block_id,
            self.certified_state,
            self.certified_block_round,
        );
        if self.ledger_info().ledger_info().consensus_data_hash() != vote_hash {
            return Err(VoteMsgVerificationError::ConsensusDataMismatch);
        }
        // Genesis is implicitly agreed upon, it doesn't have real signatures.
        if self.certified_block_round == 0
            && self.certified_block_id == *GENESIS_BLOCK_ID
            && self.certified_state == ExecutedState::state_for_genesis()
        {
            return Ok(());
        }
        self.ledger_info()
            .verify(validator)
            .map_err(VoteMsgVerificationError::SigVerifyError)
    }
}

impl IntoProto for QuorumCert {
    type ProtoType = ProtoQuorumCert;

    fn into_proto(self) -> Self::ProtoType {
        let mut proto = Self::ProtoType::new();
        proto.set_block_id(self.certified_block_id.into());
        proto.set_state_id(self.certified_state.state_id.into());
        proto.set_version(self.certified_state.version);
        proto.set_round(self.certified_block_round);
        proto.set_signed_ledger_info(self.signed_ledger_info.into_proto());
        proto
    }
}

impl FromProto for QuorumCert {
    type ProtoType = ProtoQuorumCert;

    fn from_proto(object: Self::ProtoType) -> Result<Self> {
        let certified_block_id = HashValue::from_slice(object.get_block_id())?;
        let state_id = HashValue::from_slice(object.get_state_id())?;
        let version = object.get_version();
        let certified_block_round = object.get_round();
        let signed_ledger_info =
            LedgerInfoWithSignatures::from_proto(object.get_signed_ledger_info().clone())?;
        Ok(QuorumCert {
            certified_block_id,
            certified_state: ExecutedState { state_id, version },
            certified_block_round,
            signed_ledger_info,
        })
    }
}
