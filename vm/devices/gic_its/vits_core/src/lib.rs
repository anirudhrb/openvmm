// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Virtual ARM GICv3 Interrupt Translation Service (ITS) control-plane
//! emulator.
//!
//! This crate emulates the control plane of an ARM GICv3 Interrupt Translation
//! Service: the `GITS_*` register frame, the command queue, and the device,
//! collection and interrupt translation tables that the guest programs through
//! it. It does not deliver interrupts. Instead it answers the question "which
//! (processor, LPI) does this (DeviceID, EventID) map to?", which the host VMM
//! uses to program whatever really routes the interrupt.
//!
//! The host supplies guest memory access and notifications by implementing
//! [`ItsEnvironment`].

#![forbid(unsafe_code)]

mod cache;
mod commands;
mod core;
mod env;
mod error;
mod registers;
mod tables;
mod types;

pub use crate::core::ItsCore;
pub use crate::env::CommandQueue;
pub use crate::env::InvalidateTarget;
pub use crate::env::ItsEnvironment;
pub use crate::env::Translation;
pub use crate::error::ItsError;
pub use crate::types::CollectionId;
pub use crate::types::DeviceId;
pub use crate::types::EventId;
pub use crate::types::InterruptId;
pub use crate::types::ProcessorId;
