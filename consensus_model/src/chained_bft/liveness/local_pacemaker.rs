// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{
    chained_bft::{
        common::Round,
        liveness::{
            pacemaker::{NewRoundEvent, NewRoundReason, Pacemaker},
            pacemaker_timeout_manager::{HighestTimeoutCertificates, PacemakerTimeoutManager},
            timeout_msg::{PacemakerTimeout, PacemakerTimeoutCertificate},
        },
        persistent_storage::PersistentLivenessStorage,
    },
    counters,
    util::time_service::{TimeService},
};
use std::{
    cmp::{self, max},
    pin::Pin,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use termion::color::*;
use tokio::runtime::TaskExecutor;

/// Determines the maximum round duration based on the round difference between the current
/// round and the committed round
pub trait PacemakerTimeInterval: Send + Sync + 'static {
    /// Use the index of the round after the highest quorum certificate to commit a block and
    /// return the duration for this round
    ///
    /// Round indices start at 0 (round index = 0 is the first round after the round that led
    /// to the highest committed round).  Given that round r is the highest round to commit a
    /// block, then round index 0 is round r+1.  Note that for genesis does not follow the
    /// 3-chain rule for commits, so round 1 has round index 0.  For example, if one wants
    /// to calculate the round duration of round 6 and the highest committed round is 3 (meaning
    /// the highest round to commit a block is round 5, then the round index is 0.
    fn get_round_duration(&self, round_index_after_committed_qc: usize) -> Duration;
}

/// Round durations increase exponentially
/// Basically time interval is base * mul^power
/// Where power=max(rounds_since_qc, max_exponent)
#[derive(Clone)]
pub struct ExponentialTimeInterval {
    // Initial time interval duration after a successful quorum commit.
    base_ms: u64,
    // By how much we increase interval every time
    exponent_base: f64,
    // Maximum time interval won't exceed base * mul^max_pow.
    // Theoretically, setting it means
    // that we rely on synchrony assumptions when the known max messaging delay is
    // max_interval.  Alternatively, we can consider using max_interval to meet partial synchrony
    // assumptions where while delta is unknown, it is <= max_interval.
    max_exponent: usize,
}

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

/// `LocalPacemaker` is a wrapper to make the `LocalPacemakerInner` thread-safe.
pub struct LocalPacemaker {
    inner: Arc<RwLock<LocalPacemakerInner>>,
}