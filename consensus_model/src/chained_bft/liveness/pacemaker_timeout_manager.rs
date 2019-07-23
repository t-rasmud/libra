// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::chained_bft::{
    common::Author,
    liveness::timeout_msg::{PacemakerTimeout, PacemakerTimeoutCertificate},
    persistent_storage::PersistentLivenessStorage,
};
use logger::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Tracks the highest round known local and received timeout certificates
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HighestTimeoutCertificates {
    // Highest timeout certificate gathered locally
    highest_local_timeout_certificate: Option<PacemakerTimeoutCertificate>,
    // Highest timeout certificate received from another replica
    highest_received_timeout_certificate: Option<PacemakerTimeoutCertificate>,
}

/// Manages the PacemakerTimeout structs received from replicas.
///
/// A replica can generate and track TimeoutCertificates of the highest round (locally and received)
/// to allow a pacemaker to advance to the latest certificate round.
pub struct PacemakerTimeoutManager {
    // The minimum quorum to generate a timeout certificate
    timeout_certificate_quorum_size: usize,
    // Track the PacemakerTimeoutMsg for highest timeout round received from this node
    author_to_received_timeouts: HashMap<Author, PacemakerTimeout>,
    // Highest timeout certificates
    highest_timeout_certificates: HighestTimeoutCertificates,
    // Used to persistently store the latest known timeout certificate
    persistent_liveness_storage: Box<dyn PersistentLivenessStorage>,
}