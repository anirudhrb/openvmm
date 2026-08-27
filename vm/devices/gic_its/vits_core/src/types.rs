// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Type aliases for GICv3 ITS identifiers.

/// A device identifier (typically derived from PCI segment and BDF).
pub type DeviceId = u32;

/// An interrupt collection identifier.
pub type CollectionId = u32;

/// A device-specific event identifier written to `GITS_TRANSLATER`.
pub type EventId = u32;

/// A physical LPI interrupt identifier (INTID >= 8192).
pub type InterruptId = u32;

/// A target processor / redistributor identifier.
pub type ProcessorId = u32;
