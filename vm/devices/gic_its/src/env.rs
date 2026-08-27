// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The [`ItsEnvironment`] implementation that connects [`vits_core`] to
//! OpenVMM's guest memory and to the interrupt data plane.

use crate::data_plane::ItsDataPlane;
use guestmem::GuestMemory;
use guestmem::GuestMemoryError;
use std::sync::Arc;
use vits_core::DeviceId;
use vits_core::EventId;
use vits_core::InvalidateTarget;
use vits_core::ItsEnvironment;
use vits_core::Translation;

/// Everything [`vits_core::ItsCore`] needs from OpenVMM.
pub struct GicItsEnv {
    guest_memory: GuestMemory,
    programmer: Arc<dyn ItsDataPlane>,
}

impl GicItsEnv {
    pub(crate) fn new(guest_memory: GuestMemory, programmer: Arc<dyn ItsDataPlane>) -> Self {
        Self {
            guest_memory,
            programmer,
        }
    }
}

impl ItsEnvironment for GicItsEnv {
    type Error = GuestMemoryError;

    fn read_command(&self, gpa: u64) -> Result<[u64; 4], Self::Error> {
        self.guest_memory.read_plain::<[u64; 4]>(gpa)
    }

    fn invalidate(&mut self, target: InvalidateTarget) {
        self.programmer.invalidate(target);
    }

    fn translation_changed(
        &mut self,
        device_id: DeviceId,
        event_id: EventId,
        translation: Option<Translation>,
    ) {
        self.programmer.retarget(device_id, event_id, translation);
    }
}
