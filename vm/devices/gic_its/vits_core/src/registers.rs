// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! GICv3 ITS register offsets and bit definitions.

pub(crate) const PAGE_SIZE: u64 = 1 << 12;

// ITS register offsets
pub(crate) const GITS_CTLR_OFFSET: u32 = 0x000;
pub(crate) const GITS_IIDR_OFFSET: u32 = 0x004;
pub(crate) const GITS_TYPER_OFFSET: u32 = 0x0008;
pub(crate) const GITS_CBASER_OFFSET: u32 = 0x0080;
pub(crate) const GITS_CWRITER_OFFSET: u32 = 0x0088;
pub(crate) const GITS_CREADR_OFFSET: u32 = 0x0090;
pub(crate) const GITS_BASER0_OFFSET: u32 = 0x0100;
pub(crate) const GITS_BASER1_OFFSET: u32 = 0x0108;
pub(crate) const GITS_PIDR2_OFFSET: u32 = 0xFFE8;

// GITS_CTLR register bits
pub(crate) const GITS_CTLR_ENABLE: u32 = 1 << 0;
pub(crate) const GITS_CTLR_QUIESCENT: u32 = 1 << 31;

// ITS identification registers
pub(crate) const GITS_IIDR_MSFT: u32 = 0x115;
pub(crate) const GITS_PIDR2_GICV3: u32 = 0x30;

// GITS_TYPER register bits
pub(crate) const GITS_TYPER_PLPIS: u64 = 1 << 0;

// Device ID configuration (16-bit device ID)
pub(crate) const GITS_TYPER_DEVBITS: u64 = 15;
pub(crate) const GITS_TYPER_DEVBITS_SHIFT: u64 = 13;

// Event ID configuration (16-bit event ID)
pub(crate) const GITS_TYPER_IDBITS: u64 = 15;
pub(crate) const GITS_TYPER_IDBITS_SHIFT: u64 = 8;

// ITTE size configuration (8-byte ITTE size)
pub(crate) const GITS_TYPER_ITTE_SIZE_SHIFT: u64 = 4;
pub(crate) const GITS_TYPER_ITTE_SIZE: u64 = 7;

// Command queue constants
pub(crate) const GITS_CREADR_SHIFT: u64 = 5;
pub(crate) const GITS_CWRITER_SHIFT: u64 = 5;
pub(crate) const GITS_COMMAND_SIZE: u64 = 32;

// Command buffer base address register masks.
//
// Per ARM IHI 0069, GITS_CBASER.Valid is bit 63 and Size is bits [7:0]. Size
// holds the number of 4KB pages in the command queue, minus one.
pub(crate) const GITS_CBASER_VALID: u64 = 1 << 63;
pub(crate) const GITS_CBASER_SIZE_MASK: u64 = 0xFF;
pub(crate) const GITS_CBASER_ADDRESS_MASK: u64 = 0xFFFF_FFFF_F000;

// GITS_BASER_TYPE field values
pub(crate) const GITS_BASER_TYPE_NONE: u64 = 0;
pub(crate) const GITS_BASER_TYPE_DEVICE: u64 = 1;
pub(crate) const GITS_BASER_TYPE_COLLECTION: u64 = 4;
pub(crate) const GITS_BASER_TYPE_MASK: u64 = 0b111;

pub(crate) const GITS_BASER_ENTRY_SIZE_MASK: u64 = 0b1_1111;

// GITS_BASER_CACHE field values
pub(crate) const GITS_BASER_CACHE_SAME_AS_INNER: u64 = 0;
pub(crate) const GITS_BASER_CACHE_RA_WB: u64 = 3;

// GITS_BASER_SHAREABILITY field values
pub(crate) const GITS_BASER_INNER_SHAREABLE: u64 = 1;

// GITS_BASER bit field shift values
pub(crate) const GITS_BASER_SHAREABILITY_SHIFT: u64 = 10;
pub(crate) const GITS_BASER_ENTRY_SIZE_SHIFT: u64 = 48;
pub(crate) const GITS_BASER_OUTER_CACHEABILITY_SHIFT: u64 = 53;
pub(crate) const GITS_BASER_TYPE_SHIFT: u64 = 56;
pub(crate) const GITS_BASER_INNER_CACHEABILITY_SHIFT: u64 = 59;

// Read-only mask for GITS_BASER registers
pub(crate) const GITS_BASER_RO_MASK: u64 = (GITS_BASER_TYPE_MASK << GITS_BASER_TYPE_SHIFT)
    | (GITS_BASER_ENTRY_SIZE_MASK << GITS_BASER_ENTRY_SIZE_SHIFT);
