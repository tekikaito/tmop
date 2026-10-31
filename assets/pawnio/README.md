# PawnIO modules

Signed, unmodified modules from the official PawnIO.Modules repository, release 0.2.11:
<https://github.com/namazso/PawnIO.Modules/releases/tag/0.2.11> (source:
<https://github.com/namazso/PawnIO.Modules>).

| File | SHA-256 | Used for |
|---|---|---|
| `AMDFamily17.bin` | `dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91` | AMD Ryzen (family 17h to 1Ah) Tctl/Tdie, `ioctl_read_smn` |
| `IntelMSR.bin` | `d6ed85d65ab17a22f813ef98207d6d537155ee2ded5976a21cb48413c9b92e5f` | Intel package temperature, `ioctl_read_msr` |

Copyright (C) namazso. The modules are licensed under the GNU Lesser General Public License,
version 2.1 or (at your option) any later version (`COPYING`), which allows them in TMOP
(GPL-3.0). The PawnIO driver only loads modules signed with its key, so these files can't be
rebuilt from source and must stay byte for byte as released. A unit test
(`metrics::pawnio::tests::modules_are_the_released_ones`) checks the hashes above.

TMOP embeds them in the Windows program and hands them to the PawnIO driver, which the user
installs separately (<https://pawnio.eu>). TMOP talks to the driver only through its device I/O
control interface, which PawnIO's licence (GPL-2.0-or-later with an exception for that
interface) allows. TMOP does not include or load PawnIOLib.dll.
