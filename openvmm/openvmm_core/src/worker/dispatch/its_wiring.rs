// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![cfg(guest_arch = "aarch64")]

//! GICv3 ITS wiring for aarch64 VMs.
//!
//! Instantiates the emulated ITS and builds the MSI target that connects PCIe
//! devices to it.

use closeable_mutex::CloseableMutex;
use gic_its::GicItsDevice;
use guestmem::GuestMemory;
use pci_core::msi::SignalMsi;
use std::sync::Arc;
use vmcore::its::ItsDataPlane;
use vmotherboard::ChipsetBuilder;

/// Offset of `GITS_TRANSLATER` within the ITS MMIO window: the second 64 KiB
/// frame, offset `0x40`.
const GITS_TRANSLATER_OFFSET: u64 = 0x1_0040;

/// Creates the emulated ITS and returns the [`SignalMsi`] that routes PCIe
/// MSIs to it.
///
/// Returns `None` when the backend cannot program LPI delivery, in which case
/// the caller should fall back to the partition's own MSI interface.
pub(super) fn setup_its(
    its_base: u64,
    data_plane: Option<Arc<dyn ItsDataPlane>>,
    chipset_builder: &ChipsetBuilder<'_>,
    gm: &GuestMemory,
) -> anyhow::Result<Option<Arc<dyn SignalMsi>>> {
    let Some(data_plane) = data_plane else {
        tracing::warn!(
            "ITS is configured but the hypervisor backend cannot deliver LPIs; \
             MSIs will not be routed through it"
        );
        return Ok(None);
    };

    // The device is built from the data plane rather than the other way round,
    // which is what keeps this acyclic: the data plane comes from the
    // partition, the device from the data plane, and the MSI target from both.
    let device = {
        let gm = gm.clone();
        let data_plane = data_plane.clone();
        chipset_builder
            .arc_mutex_device("gic_its")
            .add(move |_services| GicItsDevice::new(its_base, gm, data_plane))?
    };

    Ok(Some(Arc::new(ItsMsiTarget {
        device,
        data_plane,
        translater_addr: its_base + GITS_TRANSLATER_OFFSET,
    })))
}

/// Routes PCIe MSIs to the emulated ITS.
///
/// Interrupt delivery and interrupt configuration take different paths here,
/// deliberately. Delivery goes straight to the data plane, which holds its own
/// translation state, so the hot path never touches the ITS device or its lock.
/// Only configuration — a guest enabling or disabling a vector — takes the
/// device lock, because only the ITS itself knows which interrupts it must
/// track.
struct ItsMsiTarget {
    device: Arc<CloseableMutex<GicItsDevice>>,
    data_plane: Arc<dyn ItsDataPlane>,
    /// Address a device must write to for the write to be an MSI.
    translater_addr: u64,
}

impl ItsMsiTarget {
    /// Checks that an MSI is aimed at the ITS doorbell.
    ///
    /// The address is guest-programmed, so a mismatch is not a VMM bug; it
    /// means the guest pointed a device's MSI somewhere the ITS does not own.
    fn is_doorbell(&self, address: u64, data: u32) -> bool {
        if address == self.translater_addr {
            return true;
        }
        tracelimit::warn_ratelimited!(
            address,
            data,
            expected = self.translater_addr,
            "its: MSI address is not the ITS doorbell"
        );
        false
    }
}

impl SignalMsi for ItsMsiTarget {
    fn signal_msi(&self, devid: Option<u32>, address: u64, data: u32) {
        let Some(devid) = devid else {
            return;
        };
        if !self.is_doorbell(address, data) {
            return;
        }
        // The data payload of an MSI write is the EventID.
        self.data_plane.assert(devid, data);
    }

    fn enable_msi(&self, devid: Option<u32>, address: u64, data: u32) {
        let Some(devid) = devid else {
            return;
        };
        if !self.is_doorbell(address, data) {
            return;
        }
        self.device.lock().subscribe_interrupt(devid, data);
    }

    fn disable_msi(&self, devid: Option<u32>, address: u64, data: u32) {
        let Some(devid) = devid else {
            return;
        };
        if !self.is_doorbell(address, data) {
            return;
        }
        self.device.lock().unsubscribe_interrupt(devid, data);
    }
}
