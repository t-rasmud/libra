// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

#[derive(Clone, Debug, PartialEq)]
pub enum SyncStatus {
    Finished,
    ExecutionFailed,
    StorageReadFailed,
    DownloadFailed,
    DownloaderNotAvailable,
    ChunkIsEmpty,
}