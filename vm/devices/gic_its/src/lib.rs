// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Virtual GICv3 Interrupt Translation Service (ITS) for OpenVMM.
//!
//! An arm64 guest without an ITS receives MSIs as SPIs, which the GIC
//! architecture caps at INTID 1019 — roughly 960 usable vectors per VM.
//! Presenting an ITS lets the guest use LPIs (INTID >= 8192) instead, breaking
//! that ceiling.
//!
//! # Plane split
//!
//! An ITS is a translation and configuration engine, not a delivery engine,
//! which is why its control plane can live entirely in userspace:
//!
//! * **Control plane — this crate.** The `GITS_*` register frame, the ITS
//!   command queue, and the device / collection / interrupt-translation tables.
//!   This answers *"which (vCPU, LPI) does this (DeviceID, EventID) map to?"*
//!   The emulation itself lives in [`vits_core`]; this crate adapts it to
//!   OpenVMM's [`ChipsetDevice`](chipset_device::ChipsetDevice) model.
//! * **Data plane — the hypervisor.** Actually marking an LPI pending on a
//!   vCPU, and programming the physical ITS for assigned devices. This crate
//!   only describes what the data plane must do, through [`ItsDataPlane`].

#![forbid(unsafe_code)]

mod data_plane;
mod device;
mod env;

pub use data_plane::ItsDataPlane;
pub use device::GicItsDevice;
pub use device::MMIO_REGION_SIZE;
