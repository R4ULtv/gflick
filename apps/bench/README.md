# GFlick benchmark

`gflick-bench` is a native release-mode recorder for repeatable measurements of
long-running process groups on Windows and macOS. It is intended for comparisons such
as GFlick versus the complete resident G Hub stack.

The recorder measures operating-system process counters. It does not inject code into
the target and does not communicate with GFlick IPC. It writes:

- a versioned JSON report containing metadata, a summary, and every sample;
- a CSV file beside the JSON report for plotting and independent analysis.

## Build

```powershell
cargo build --release -p gflick-bench
```

Always run `target/release/gflick-bench`, not `cargo run`, during a real measurement.
This prevents Cargo and compiler activity from contaminating the system-load baseline.

## Recommended test conditions

For comparable results:

1. Use the same computer, OS build, power mode, mouse, connection type, DPI, polling
   rate, and battery-query conditions for every run.
2. Run only the mouse stack being tested. Close its settings window but leave its normal
   background components running.
3. After starting the stack, leave the mouse and computer idle for at least five minutes.
4. Keep the machine on the same power source and do not run builds, updates, browsers,
   games, or other foreground work during a measurement.
5. Record at least three runs per stack. Alternate their order when practical so time
   and temperature do not consistently favor one application.
6. Use 30-minute measurements for a serious idle comparison. This covers several GFlick
  five-minute battery queries and makes very small CPU-time totals measurable.

The default two-second interval captures accumulated CPU and I/O between samples; it
does not lose CPU work that happens between samples. New matching processes are
rediscovered every five seconds. Both values are stored in the report.

## Initial Windows result

An initial 30-minute comparison was recorded on August 3, 2026. It is a strong early
result, but it is not yet the final isolated three-run benchmark described above.

### Environment and scope

| Item | Value |
| --- | --- |
| Operating system | Windows 10 Pro, build 19045, x86-64 |
| Processor | AMD Ryzen 7 5700X, 8 cores / 16 logical processors |
| Memory | 32 GB |
| Warm-up | 120 seconds per recorder |
| Recorded duration | 1,800 seconds per recorder |
| Sample interval | 2 seconds |
| Process rediscovery | 5 seconds |
| GFlick group | `gflick-agent` |
| G Hub group | `lghub_agent`, `lghub_system_tray`, `lghub_updater` |

GFlick recording began at 10:39:16 CEST and G Hub recording began at 10:43:22 CEST.
The two recorders therefore overlapped for approximately 25 minutes 54 seconds. Both
mouse stacks were resident during the measurements, and two recorder processes sampled
concurrently for most of the test. The counters below still belong directly to their
selected target processes, but this run should be described as a co-resident comparison,
not an isolated sequential A/B test.

### Target process results

| Metric | GFlick | G Hub | GFlick / G Hub | Reduction |
| --- | ---: | ---: | ---: | ---: |
| Accumulated CPU time | 1,890 ms | 6,937 ms | 0.272x | 72.8% |
| Average CPU, one-core basis | 0.1050% | 0.3854% | 0.272x | 72.8% |
| Average CPU, whole-system basis | 0.006562% | 0.024087% | 0.272x | 72.8% |
| CPU sample p95 | 0.8000% | 2.3512% | 0.340x | 66.0% |
| CPU sample maximum | 1.6008% | 10.9055% | 0.147x | 85.3% |
| Resident memory mean | 7.579 MiB | 227.550 MiB | 0.033x | 96.7% |
| Resident memory p95 | 7.590 MiB | 231.418 MiB | 0.033x | 96.7% |
| Virtual memory mean | 1.602 MiB | 287.534 MiB | 0.006x | 99.4% |
| I/O read total | 24,140 bytes | 20,058,492 bytes | 0.001x | 99.9% |
| I/O read rate | 13.411 bytes/s | 11,143.606 bytes/s | 0.001x | 99.9% |
| I/O write total | 26,962 bytes | 11,877,360 bytes | 0.002x | 99.8% |
| I/O write rate | 14.979 bytes/s | 6,598.533 bytes/s | 0.002x | 99.8% |
| Resident process count | 1 | 3 | 0.333x | 66.7% |

In this run G Hub used approximately 3.7 times as much target CPU time, 30 times as much
resident memory, 831 times as much read I/O per second, and 441 times as much write I/O
per second. The G Hub I/O values are Windows all-I/O process counters and should not be
described as physical disk traffic alone.

### Validity and observer overhead

| Check | GFlick run | G Hub run |
| --- | ---: | ---: |
| Matched samples | 900 / 900 | 900 / 900 |
| Missing samples | 0 | 0 |
| Target process start events | 0 | 0 |
| System CPU mean | 8.83% | 8.28% |
| System CPU p95 | 19.57% | 17.30% |
| System CPU maximum | 66.20% | 52.98% |
| Recorder CPU time | 12,984 ms | 12,734 ms |
| Recorder CPU, one-core basis | 0.721% | 0.707% |

Every requested target remained present and stable. Recorder overhead differed by about
2%, which supports the fairness of the process-counter comparison. However, recorder
CPU exceeded either target's CPU usage, and background system load was not perfectly
quiet. These facts matter for whole-system energy testing even though recorder CPU is
not included in the target CPU totals.

## Initial Apple Silicon macOS result

A sequential 30-minute-per-stack comparison was recorded on August 3, 2026, after the
native menu-bar tray and wireless-link status work landed. This is one run per stack,
not the final alternating three-run comparison recommended above.

### Environment and scope

| Item | Value |
| --- | --- |
| Operating system | macOS 15.7.7, build 24G720 |
| Computer | MacBook Air (Mac16,12), Apple M4 |
| Processor | 10 logical cores (4 performance, 6 efficiency) |
| Memory | 16 GB |
| Mouse | PRO X Wireless through its USB receiver, 1600 DPI |
| Warm-up | 120 seconds per recorder |
| Recorded duration | 1,800 seconds per recorder |
| Sample interval | 2 seconds |
| GFlick group | `gflick-agent`, `gflick-tray` |
| G Hub group | `lghub_agent`, `lghub_system_tray`, `lghub_updater` |

G Hub was measured first with its settings window closed. GFlick was then measured
from commit `0b648be`, with the mouse in host/local control so its saved 1600 DPI was
restored. The stacks were not allowed to own the mouse concurrently because starting
G Hub immediately changed the live DPI to its configured value. Quitting G Hub stopped
its user-session agent and tray, but its root-owned updater remained resident during the
GFlick run. That updater is excluded from the GFlick target counters, so the target
comparison is useful, but the run is not a fully isolated whole-system energy test.

### Target process results

| Metric | GFlick | G Hub | GFlick / G Hub | Reduction |
| --- | ---: | ---: | ---: | ---: |
| Accumulated CPU time | 3,621 ms | 3,403 ms | 1.064x | -6.4% |
| Average CPU, one-core basis | 0.2012% | 0.1891% | 1.064x | -6.4% |
| Average CPU, whole-system basis | 0.020117% | 0.018906% | 1.064x | -6.4% |
| CPU sample p95 | 0.5008% | 0.3003% | 1.668x | -66.8% |
| CPU sample maximum | 9.2546% | 1.1483% | 8.060x | -706.0% |
| Resident memory mean | 34.959 MiB | 258.728 MiB | 0.135x | 86.5% |
| Resident memory p95 | 38.078 MiB | 297.953 MiB | 0.128x | 87.2% |
| Virtual memory mean | 802,945.779 MiB | 804,619.839 MiB | 0.998x | 0.2% |
| I/O read total | 4,096 bytes | 18,399,232 bytes | 0.0002x | 99.98% |
| I/O read rate | 2.276 bytes/s | 10,221.767 bytes/s | 0.0002x | 99.98% |
| I/O write total | 0 bytes | 0 bytes | n/a | n/a |
| Resident process count | 2 | 3 | 0.667x | 33.3% |

GFlick used approximately 7.4 times less mean resident memory and 4,492 times less
read I/O per second. It did not use less CPU in this run: accumulated target CPU was
6.4% higher than G Hub, and its short sample peaks were higher. macOS virtual-memory
figures include large shared address-space mappings and should not be interpreted as
physical memory pressure.

### Validity and observer overhead

| Check | GFlick run | G Hub run |
| --- | ---: | ---: |
| Matched samples | 900 / 900 | 900 / 900 |
| Missing samples | 0 | 0 |
| Target process start events | 0 | 0 |
| System CPU mean | 9.89% | 9.34% |
| System CPU p95 | 45.94% | 36.85% |
| System CPU maximum | 76.29% | 55.99% |
| Recorder CPU time | 4,783 ms | 5,602 ms |
| Recorder CPU, one-core basis | 0.266% | 0.311% |

Both reports are complete and every target process remained stable. System load was
somewhat noisier during the GFlick run, and the recorder itself used more CPU than
either target group. Repeat at least three alternating runs before treating the small
CPU difference as representative. These process counters are a development baseline,
not a direct energy or battery-life measurement.

## Record GFlick

The installed agent must already be running. This command excludes two minutes of
warm-up and then records 30 minutes:

```powershell
./target/release/gflick-bench record `
  --label gflick `
  --process gflick-agent `
  --warmup-seconds 120 `
  --duration-seconds 1800 `
  --interval-ms 2000 `
  --output benchmark-results/gflick-1.json
```

Repeat with `gflick-2.json` and `gflick-3.json`.

## Record G Hub on Windows

G Hub currently uses three resident components on the validation machine. Treat them as
one product rather than measuring only its smallest process:

```powershell
./target/release/gflick-bench record `
  --label g-hub `
  --process lghub_agent `
  --process lghub_system_tray `
  --process lghub_updater `
  --warmup-seconds 120 `
  --duration-seconds 1800 `
  --interval-ms 2000 `
  --output benchmark-results/g-hub-1.json
```

The executable-name comparison is exact and case-insensitive; `.exe` is optional. A
component may appear after recording starts, because the process group is periodically
rediscovered. Check Task Manager if a future G Hub version changes its process names.

Repeat with `g-hub-2.json` and `g-hub-3.json`.

## Compare the median of three runs

The ratio is `candidate / baseline`. With G Hub as the baseline and GFlick as the
candidate, a value below `1.0x` means GFlick used less of that resource.

```powershell
./target/release/gflick-bench compare `
  --baseline benchmark-results/g-hub-1.json `
  --baseline benchmark-results/g-hub-2.json `
  --baseline benchmark-results/g-hub-3.json `
  --candidate benchmark-results/gflick-1.json `
  --candidate benchmark-results/gflick-2.json `
  --candidate benchmark-results/gflick-3.json
```

The comparison uses the median value across each report group. I/O is normalized to
bytes per second, so slightly different elapsed durations do not bias it. Reports from
different platforms or sample intervals produce a warning.

## Reading the result

- `CPU, one-core average` is accumulated target CPU time divided by wall time. `100%`
  means one logical core was continuously busy. This is the primary idle-efficiency
  metric and remains understandable across CPUs with different core counts.
- `CPU system percent` in each report normalizes the target by the machine's logical CPU
  count.
- `Resident memory` is physical memory currently resident for every matching process,
  summed as one product. Use the mean for steady cost and p95 for typical peaks.
- `Virtual memory` is address space, not physical RAM, and should not be interpreted as
  direct memory pressure.
- `I/O` is accumulated between samples. On Windows the underlying process counter covers
  all I/O, not only disk traffic.
- `Process count` and `process_start_events` reveal helper-process churn.
- `System CPU mean` helps identify a noisy run. Large differences between runs warrant a
  rerun even when the target counters look favorable.
- `Sampler CPU` reports the recorder's own overhead. A two-second or longer interval is
  preferred if this becomes material.
- `Completeness` must show every sample matched. A report with missing samples is retained
  for diagnosis but should not be used as a valid comparison.

## What this cannot prove

CPU time, wake frequency, I/O, and memory pressure influence power use, but they are not
watts. Windows does not provide a stable, precise, cross-hardware per-process energy
counter suitable for this comparison. For an actual energy claim, use the same native
reports alongside a repeatable whole-system measurement: an external power meter for a
desktop, or several long battery-discharge trials for a laptop. The recorder itself
reports its CPU overhead so that contribution is visible.
