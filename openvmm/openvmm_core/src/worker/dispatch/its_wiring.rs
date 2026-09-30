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
use pci_core::msi::MsiRouteVector;
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
) -> anyhow::Result<Option<ItsMsiTargets>> {
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

    let translater_addr = its_base + GITS_TRANSLATER_OFFSET;
    Ok(Some(ItsMsiTargets {
        emulated: Arc::new(ItsMsiTarget {
            device: device.clone(),
            data_plane: data_plane.clone(),
            translater_addr,
            assigned: false,
        }),
        assigned: Arc::new(ItsMsiTarget {
            device,
            data_plane,
            translater_addr,
            assigned: true,
        }),
    }))
}

/// The ITS MSI targets, one per device ownership model.
///
/// The two behave differently on the configuration path because the hypervisor
/// makes the two LPI ownership models mutually exclusive: an emulated device's
/// LPI is reserved and asserted by this VMM, whereas a passthrough device's is
/// mapped into the physical ITS by the kernel. Reserving an LPI the kernel is
/// about to map makes the map fail, so the ITS must know which it is dealing
/// with. Both share one ITS device and data plane.
#[derive(Clone)]
pub(super) struct ItsMsiTargets {
    /// For devices this VMM emulates.
    pub emulated: Arc<dyn SignalMsi>,
    /// For devices assigned to the guest through VFIO.
    pub assigned: Arc<dyn SignalMsi>,
}

impl ItsMsiTargets {
    /// Picks the target for a device by whether it is assigned to the guest.
    pub fn select(&self, assigned: bool) -> Arc<dyn SignalMsi> {
        if assigned {
            self.assigned.clone()
        } else {
            self.emulated.clone()
        }
    }
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
    /// Whether this target serves VFIO-assigned devices. See [`ItsMsiTargets`].
    assigned: bool,
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

    fn enable_msi(&self, devid: Option<u32>, address: u64, data: u32) -> MsiRouteVector {
        let Some(devid) = devid else {
            return MsiRouteVector::Unresolved;
        };
        if !self.is_doorbell(address, data) {
            return MsiRouteVector::Unresolved;
        }
        // Subscribing reserves the LPI and claims it for software delivery.
        // For an assigned device the kernel maps that same LPI into the
        // physical ITS instead, and the hypervisor rejects the map if it is
        // already reserved, so leave it alone.
        if !self.assigned {
            self.device.lock().subscribe_interrupt(devid, data);
        }
        // A kernel route delivers by LPI, but the guest wrote an EventID, so
        // resolve it. An unmapped interrupt has no LPI yet; the guest maps it
        // through the ITS command queue, which is not ordered against this.
        match self.device.lock().translate_interrupt(devid, data) {
            Some(intid) => MsiRouteVector::Vector(intid),
            None => MsiRouteVector::Unresolved,
        }
    }

    fn disable_msi(&self, devid: Option<u32>, address: u64, data: u32) {
        let Some(devid) = devid else {
            return;
        };
        if !self.is_doorbell(address, data) {
            return;
        }
        if !self.assigned {
            self.device.lock().unsubscribe_interrupt(devid, data);
        }
    }
}
