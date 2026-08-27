// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Error types returned by the virtual ITS emulator.

use thiserror::Error;

/// Errors returned by ITS register accesses and command processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ItsError {
    /// Unaligned or invalid-width register access, or failed guest memory read.
    #[error("invalid memory access")]
    InvalidMemoryAccess,
    /// Access to an unimplemented or read-only ITS register.
    #[error("invalid register access")]
    InvalidRegisterAccess,
    /// Unknown ITS command opcode.
    #[error("invalid ITS command")]
    InvalidCommand,
    /// Command specified a DeviceID exceeding the configured device table limit.
    #[error("command device ID out of range")]
    CommandDeviceIdOutOfRange,
    /// Command specified a CollectionID exceeding the configured collection table limit.
    #[error("command collection ID out of range")]
    CommandCollectionIdOutOfRange,
    /// Command referenced an `(DeviceID, EventID)` pair that is not mapped.
    #[error("command interrupt not mapped")]
    CommandInterruptNotMapped,
    /// A table operation requested by a command failed.
    #[error("ITS command execution failed")]
    CommandError,
    /// Valid ITS command opcode that is not currently supported.
    #[error("ITS command not supported")]
    CommandNotSupported,
}
