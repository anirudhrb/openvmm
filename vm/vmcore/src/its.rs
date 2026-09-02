// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The interface between an emulated GICv3 ITS and whatever delivers its
//! interrupts.
//!
//! An ITS is a translation engine, not a delivery engine: it answers "which
//! (processor, LPI) does this (DeviceID, EventID) map to?" and nothing more.
//! This trait is the boundary between the emulated device that answers that
//! question and the hypervisor backend that acts on the answer.

/// The resolved target of an interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItsTranslation {
    /// The LPI interrupt ID.
    pub intid: u32,
    /// The virtual processor the interrupt is targeted at.
    pub vp_index: u32,
}

/// The scope of a guest-requested invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItsInvalidate {
    /// A single LPI, identified by interrupt ID.
    Interrupt(u32),
    /// Every interrupt belonging to a collection.
    Collection(u32),
}

/// Programs the interrupt data plane on behalf of an emulated ITS.
///
/// Implementations must expect to be called with the emulated ITS's device lock
/// held, on the vCPU thread that trapped the guest's MMIO access, so they
/// should not block for long.
///
/// Every method takes `&self` because the ITS shares its data plane with the
/// MSI signalling path, which cannot take that lock.
pub trait ItsDataPlane: Send + Sync {
    /// A subscribed interrupt's translation changed, or vanished.
    ///
    /// `translation` is `None` when the interrupt is no longer mapped.
    ///
    /// A single guest command can retarget every interrupt in a collection, so
    /// this may be called many times in a row.
    fn retarget(&self, device_id: u32, event_id: u32, translation: Option<ItsTranslation>);

    /// The guest's LPI configuration table changed (enable bit, priority).
    ///
    /// The ITS does not own that table, so this is purely a notification that
    /// whoever caches it must re-read it.
    ///
    /// `vp_index` is the processor that trapped the command queue write. The
    /// table is read through a specific redistributor, so an implementation
    /// that must name one should use this rather than choosing arbitrarily.
    fn invalidate(&self, vp_index: u32, target: ItsInvalidate);

    /// An interrupt was signalled and should be made pending.
    fn assert(&self, device_id: u32, event_id: u32);

    /// Releases every binding, returning the data plane to its initial state.
    ///
    /// Called when the ITS is reset. A binding is typically ownership of an LPI
    /// in the hypervisor, and that ownership outlives the ITS state that
    /// produced it, so discarding the translations without releasing them would
    /// strand the LPIs.
    fn reset(&self);
}
