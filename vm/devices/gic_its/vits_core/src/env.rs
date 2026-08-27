// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Interface between the ITS emulator and its host VMM.

use crate::types::CollectionId;
use crate::types::DeviceId;
use crate::types::EventId;
use crate::types::InterruptId;
use crate::types::ProcessorId;

/// A resolved interrupt translation: the target processor and the physical LPI
/// interrupt ID that a `(DeviceID, EventID)` pair maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Translation {
    /// Target processor / redistributor identifier.
    pub processor_id: ProcessorId,
    /// Physical LPI interrupt identifier.
    pub interrupt_id: InterruptId,
}

/// The scope of an ITS cache invalidation requested by the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidateTarget {
    /// `INV DeviceID, EventID`, resolved to the affected LPI interrupt ID.
    Interrupt(InterruptId),
    /// `INVALL ICID`: every interrupt belonging to the given collection.
    Collection(CollectionId),
}

/// The guest-physical location of the ITS command queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandQueue {
    /// Base guest physical address of the command queue ring.
    pub base_gpa: u64,
    /// Size of the command queue ring in bytes.
    pub size_bytes: u64,
}

/// Everything the ITS emulator needs from its host VMM.
///
/// The emulator is deliberately ignorant of how the host reads guest memory or
/// delivers interrupts, so that the core remains decoupled from VMM internals.
pub trait ItsEnvironment {
    /// Error type returned by guest memory reads.
    type Error: core::fmt::Debug;

    /// Reads one 32-byte ITS command from guest physical memory.
    ///
    /// Called once per command while draining the queue. The emulator never
    /// retains a pointer into guest memory, both because the guest may remap it
    /// at any time and because OpenVMM exposes guest memory only through
    /// explicit accessors.
    fn read_command(&self, gpa: u64) -> Result<[u64; 4], Self::Error>;

    /// Notifies the host that the guest enabled or disabled the command queue.
    ///
    /// `None` means the queue has been disabled.
    fn command_queue_changed(&mut self, queue: Option<CommandQueue>) {
        let _ = queue;
    }

    /// Notifies the host that the guest issued `INV` or `INVALL`, and that any
    /// cached translation state it holds for the given scope is now stale.
    fn invalidate(&mut self, target: InvalidateTarget) {
        let _ = target;
    }

    /// Notifies the host that the translation for a subscribed interrupt has
    /// changed, so that it can retarget the interrupt accordingly.
    ///
    /// `None` means the interrupt is no longer mapped.
    fn translation_changed(
        &mut self,
        device_id: DeviceId,
        event_id: EventId,
        translation: Option<Translation>,
    ) {
        let _ = (device_id, event_id, translation);
    }
}
