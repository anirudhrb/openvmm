// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The [`ItsEnvironment`] implementation that connects [`vits_core`] to
//! OpenVMM's guest memory and to the interrupt data plane.

use guestmem::GuestMemory;
use guestmem::GuestMemoryError;
use std::sync::Arc;
use vits_core::DeviceId;
use vits_core::EventId;
use vits_core::InvalidateTarget;
use vits_core::ItsEnvironment;
use vits_core::Translation;
use vmcore::its::ItsDataPlane;
use vmcore::its::ItsInvalidate;
use vmcore::its::ItsTranslation;

/// Everything [`vits_core::ItsCore`] needs from OpenVMM.
pub struct GicItsEnv {
    guest_memory: GuestMemory,
    programmer: Arc<dyn ItsDataPlane>,
    /// The virtual processor that trapped the MMIO write currently being
    /// serviced.
    ///
    /// [`ItsEnvironment::invalidate`] carries no VP, and that signature is
    /// shared with the Windows implementation, so the device records the
    /// trapping processor here immediately before handing control to the core.
    /// The reference implementation does the same thing for the same reason,
    /// holding the intercepted processor index as a member.
    ///
    /// Only meaningful for the duration of one MMIO write. `invalidate` is
    /// reached solely from `GITS_CWRITER` command queue processing, which runs
    /// inside that write.
    trapping_vp: Option<u32>,
}

impl GicItsEnv {
    pub(crate) fn new(guest_memory: GuestMemory, programmer: Arc<dyn ItsDataPlane>) -> Self {
        Self {
            guest_memory,
            programmer,
            trapping_vp: None,
        }
    }

    /// Records the processor servicing the MMIO write about to be dispatched.
    pub(crate) fn set_trapping_vp(&mut self, vp_index: u32) {
        self.trapping_vp = Some(vp_index);
    }

    /// Clears the recorded processor once the MMIO write completes.
    pub(crate) fn clear_trapping_vp(&mut self) {
        self.trapping_vp = None;
    }
}

impl ItsEnvironment for GicItsEnv {
    type Error = GuestMemoryError;

    fn read_command(&self, gpa: u64) -> Result<[u64; 4], Self::Error> {
        self.guest_memory.read_plain::<[u64; 4]>(gpa)
    }

    fn invalidate(&mut self, target: InvalidateTarget) {
        // Only reachable from command queue processing, which runs inside an
        // MMIO write, so the trapping processor is always recorded. Fall back
        // rather than assume, since guessing silently would be worse than a
        // trace: the hypervisor reads the LPI configuration table through the
        // named redistributor.
        let vp_index = self.trapping_vp.unwrap_or_else(|| {
            tracelimit::error_ratelimited!(
                ?target,
                "its: invalidate outside an MMIO write; attributing to vp 0"
            );
            0
        });

        let target = match target {
            InvalidateTarget::Interrupt(intid) => ItsInvalidate::Interrupt(intid),
            InvalidateTarget::Collection(icid) => ItsInvalidate::Collection(icid),
        };

        self.programmer.invalidate(vp_index, target);
    }

    fn translation_changed(
        &mut self,
        device_id: DeviceId,
        event_id: EventId,
        translation: Option<Translation>,
    ) {
        let translation = translation.map(|t| ItsTranslation {
            intid: t.interrupt_id,
            vp_index: t.processor_id,
        });

        self.programmer.retarget(device_id, event_id, translation);
    }
}
