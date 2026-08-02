# Feature coverage

This matrix distinguishes complete end-user mouse functionality from HID++ entries
that merely happen to be advertised by the firmware. `open-hub-probe features` is the
runtime source of truth and prints the same classification for any connected device.

| End-user capability | PRO X Superlight | PRO X Superlight 2 | G305 | Open Hub status |
|---|---:|---:|---:|---|
| Device name, model, unit ID, firmware | Yes | Yes | Yes | Complete |
| Battery percentage and charge state | Yes | Yes | Yes (`0x1000`) | Complete |
| Live DPI and supported range | Yes (`0x2201`) | Yes, X/Y (`0x2202`) | Yes (`0x2201`) | Complete |
| Lift-off distance | Not advertised | Yes (`0x2202`) | Not advertised | Complete where supported |
| Wired polling rate | Yes (`0x8060`) | Yes (`0x8061`) | Shared rate (`0x8060`) | Complete |
| Wireless polling rate up to 8 kHz | Up to 1 kHz | Up to 8 kHz (`0x8061`) | Up to 1 kHz | Complete |
| Performance/endurance mode | Not advertised | Read-only status | Read/write (`0x8090`) | Complete where supported |
| Gaming Surface Mode | Not advertised | Yes (`0x8090`) | Not advertised | Complete where supported |
| Color LED Effects | Internal indicator only | Not advertised | One public RGB zone (`0x8070`) | Complete; all four G305 effects passed live volatile round trips |
| BHOP | Not advertised | Yes, live and stored (`0x80e0`, profile `0x07`) | Not advertised | Core/probe complete; stored encoder unit-tested |
| Host/onboard mode and active profile | Yes | Yes | Yes | Complete |
| Onboard profile list and decoding | Format `0x04` | Format `0x07` | Format `0x03` | Complete |
| Transactional profile-sector writes | Live flash proof passed | Encoder unit-tested; live flash intentionally not tested | Encoder unit-tested; live flash intentionally not tested | Core complete |
| Profile enable/disable directory | Five profiles | Five profiles | One writable plus one factory profile | Core/probe complete; live legacy proof passed on Superlight |
| DPI stages and active/shift stage | Stored in profile | Stored in profile | Stored in profile | Core/probe complete |
| Onboard button assignments | Five buttons | Five buttons | Six buttons | Core/probe complete; macros excluded |
| Profile names and power timers | Yes | Yes | Yes | Core/probe complete |
| Host button mapping/filter | Yes (`0x8110`) | Yes (`0x8110`) | Yes (`0x8110`) | Core read/write complete |
| Host shortcuts and macros | Possible through an agent | Possible through an agent | Possible through an agent | Intentionally out of scope |
| Onboard macro creation | Macro format `0x01` advertised | Macro format `0x01` advertised | Macro format `0x01` advertised | Intentionally out of scope |
| Per-application profiles | Possible through a host service | Possible through a host service | Possible through a host service | Intentionally out of scope |

## Diagnostics, not settings

The Superlight 2 advertises X/Y motion statistics (`0x2250`) and wheel statistics
(`0x2251`). They are useful for a diagnostics screen but do not configure normal mouse
behaviour. The public feature `0x1602` remains undocumented and is not called until its
semantics are known.

## Intentionally excluded

Hidden/internal `0x18xx` and `0x1Exx` features are firmware manufacturing, RF-test,
out-of-box, reset, or hidden-feature facilities. Force Pairing (`0x1500`) and signed
DFU control (`0x00c2`) are maintenance workflows, not settings. They stay blocked from
the normal settings API so a UI cannot accidentally reset, re-pair, or put a mouse into
a factory/test mode.

The original Superlight advertises Color LED Effects (`0x8070`) with hidden+internal
flags. It is the small firmware-controlled indicator, not a public RGB zone, so it is
not presented as a lighting setting.

## Profile-write safety proof

The original Superlight accepted an identical 255-byte write to disabled profile 5
(sector `0x0005`) and returned the same bytes with valid CRC `0x519b`. The writer uses
an optimistic stale-data check, byte-preserving edits, CRC regeneration, read-back
verification, and automatic rollback. Formats `0x03`, `0x04`, and `0x07` pass encoder
unit tests. A temporary name change to `OPEN_HUB_TEST` was semantically and byte-wise
verified before all 255 original bytes were restored with CRC `0x519b`. Profile 5 also
passed an enable/disable directory round trip without becoming active. At the owner's
request, no profile write was sent to the G305 or Superlight 2.

## G305 live-setting proof

The G305 passed fixed red, 1000 ms cycling, 60 ms blue breathing, and disabled
volatile LED writes with effect-aware read-back verification. Firmware LED ownership
was restored afterward. Its shared polling rate passed `1000 -> 500 -> 1000 Hz` while
temporarily host-controlled, followed by verified reactivation of onboard profile
sector `0x0001`. No onboard sector or non-volatile LED storage was written.
