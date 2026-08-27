# GICv3 ITS

OpenVMM emulates a GICv3 Interrupt Translation Service (ITS) for aarch64
guests, so that PCIe MSI/MSI-X interrupts are delivered as LPIs rather than
as SPIs.

Without an ITS, an arm64 guest receives MSIs as SPIs through a GICv2m frame.
The GIC architecture caps SPIs at INTID 1019, which leaves roughly **960
usable MSI vectors per VM** — a ceiling that large guests with many virtio
queues or assigned devices reach in practice. An ITS lets the guest use LPIs
(INTID >= 8192) instead, which are limited only by `GICD_TYPER.IDbits`.

The emulation lives in the `gic_its` crate (`vm/devices/gic_its`), which
adapts the shared `vits_core` ITS emulator to OpenVMM's `ChipsetDevice`
model.

## Control plane and data plane

An ITS is a translation and configuration engine, not a delivery engine. That
split is what makes the emulation possible in userspace:

| Concern | Owner |
| --- | --- |
| `GITS_*` register frame | `gic_its` |
| ITS command queue parsing | `gic_its` |
| Device / collection / interrupt translation tables | `gic_its` |
| `(DeviceID, EventID)` -> `(vCPU, LPI INTID)` | `gic_its` |
| LPI configuration and pending tables | hypervisor |
| Marking an LPI pending on a vCPU | hypervisor |
| Physical ITS programming for assigned devices | hypervisor |

`gic_its` answers *"which (vCPU, LPI) does this (DeviceID, EventID) map
to?"*, and reports the answer to an `ItsDataPlane` implementation, which is
responsible for making the interrupt actually arrive.

## MMIO window

The ITS occupies a 128 KiB window — two 64 KiB frames — placed by the VM
topology:

| Offset | Contents | Handling |
| --- | --- | --- |
| `0x0000`–`0xFFFF` | `GITS_*` control registers | emulated |
| `0x10040` | `GITS_TRANSLATER` | ignored, see below |
| everything else | unimplemented | RAZ/WI |

Only 32- and 64-bit accesses are supported, as the GIC architecture requires.
Any other access width is reported as a bus error. Everything else — an
unimplemented register, an unsupported command, a malformed write — is
handled internally and traced, never surfaced as a bus error, so that a
misbehaving guest cannot take a fatal path in the VMM.

Registers `GITS_BASER2` through `GITS_BASER7` fall under this rule: they are
unimplemented, so they read as zero (`GITS_BASER_TYPE_NONE`) and ignore
writes, which is what a guest's table-type probe loop expects.

### `GITS_TRANSLATER` writes from the guest are ignored

`GITS_TRANSLATER` is the MSI doorbell, and it is architecturally written by
*devices*, not by guest software. The DeviceID of a real translation comes
from the bus transaction's requester ID, not from the written data.

Emulated devices signal MSIs out of band rather than through this page, and
assigned devices are programmed with a host-side doorbell that never targets
it. A guest *software* write here is therefore unarchitectural, and is logged
and ignored. Honouring it would let the guest inject interrupts with a forged
DeviceID.

## Device identity

The ITS device ID for a PCIe function is `(segment << 16) | bdf`, composed by
the PCIe layer before the MSI reaches the ITS.

## Shadow tables

The device keeps its own device, collection and interrupt-translation tables
rather than walking the guest-memory tables described by `GITS_BASER`. Those
guest tables are effectively write-only from OpenVMM's perspective.

This is simpler and avoids time-of-check/time-of-use races against a guest
that mutates its tables concurrently. The consequence is that all ITS state
is rebuilt from commands after a reset.

## Command queue

Commands are drained synchronously from the guest's `GITS_CWRITER` write, on
the vCPU thread that trapped it. That is acceptable because command queue
writes are configuration-time events rather than per-interrupt events.

## Current status

**Control plane only.** The device emulates the register frame, the command
queue and the translation tables, but no `ItsDataPlane` implementation exists
yet, so nothing delivers the resulting interrupts.

## Diagnostics

The device's `Inspect` node exposes the `GITS_*` register values, including
`GITS_CBASER` (the command queue location and size) and the
`GITS_CWRITER`/`GITS_CREADR` producer and consumer indices. A queue whose
consumer index has stalled behind its producer index means a command was
rejected, and the guest is likely spinning waiting for completion.

Command processing and translation changes are traced, so the sequence of
mappings a guest programs can be followed there.
