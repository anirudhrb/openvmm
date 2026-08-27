// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The interface between the emulated ITS control plane and whatever actually
//! delivers interrupts.

use vits_core::DeviceId;
use vits_core::EventId;
use vits_core::InvalidateTarget;
use vits_core::Translation;

/// Programs the interrupt data plane on behalf of the emulated ITS.
///
/// The ITS control plane resolves `(DeviceID, EventID)` pairs to
/// `(vCPU, LPI INTID)` translations, but does not deliver anything. An
/// implementation of this trait turns those decisions into whatever the
/// platform needs — on the Microsoft hypervisor, a set of hypercalls that
/// reserve, retarget and assert virtual interrupts.
///
/// Implementations must expect to be called with the chipset device lock held,
/// on the vCPU thread that trapped the guest's MMIO access, so they should not
/// block for long.
///
/// Every method takes `&self` because the device shares the programmer with the
/// MSI signalling path, which cannot take the device lock.
pub trait ItsDataPlane: Send + Sync {
    /// A subscribed interrupt's translation changed, or vanished.
    ///
    /// `translation` is `None` when the interrupt is no longer mapped, e.g.
    /// after `DISCARD` or `MAPD` with `V=0`.
    ///
    /// A single `MAPC` or `MOVALL` command can retarget every interrupt in a
    /// collection, so this may be called many times for one command.
    fn retarget(&self, device_id: DeviceId, event_id: EventId, translation: Option<Translation>);

    /// The guest issued `INV` or `INVALL`, meaning the LPI Configuration table
    /// in guest memory changed (enable bit, priority).
    ///
    /// The ITS does not own that table, so this is purely a notification that
    /// whoever caches it must re-read it.
    fn invalidate(&self, target: InvalidateTarget);

    /// An emulated device signalled an MSI, or the guest issued the ITS `INT`
    /// command, meaning the interrupt should be made pending.
    fn assert(&self, device_id: DeviceId, event_id: EventId);
}
