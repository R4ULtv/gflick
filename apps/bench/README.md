# Open Hub benchmark

`open-hub-bench` is a native release-mode recorder for repeatable measurements of
long-running process groups on Windows and macOS. It is intended for comparisons such
as Open Hub versus the complete resident G Hub stack.

The recorder measures operating-system process counters. It does not inject code into
the target and does not communicate with Open Hub IPC. It writes:

- a versioned JSON report containing metadata, a summary, and every sample;
- a CSV file beside the JSON report for plotting and independent analysis.

## Build

```powershell
cargo build --release -p open-hub-bench
```

Always run `target/release/open-hub-bench`, not `cargo run`, during a real measurement.
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
6. Use 30-minute measurements for a serious idle comparison. This covers several Open
   Hub five-minute battery queries and makes very small CPU-time totals measurable.

The default two-second interval captures accumulated CPU and I/O between samples; it
does not lose CPU work that happens between samples. New matching processes are
rediscovered every five seconds. Both values are stored in the report.

## Record Open Hub

The installed agent must already be running. This command excludes two minutes of
warm-up and then records 30 minutes:

```powershell
./target/release/open-hub-bench record `
  --label open-hub `
  --process open-hub-agent `
  --warmup-seconds 120 `
  --duration-seconds 1800 `
  --interval-ms 2000 `
  --output benchmark-results/open-hub-1.json
```

Repeat with `open-hub-2.json` and `open-hub-3.json`.

## Record G Hub on Windows

G Hub currently uses three resident components on the validation machine. Treat them as
one product rather than measuring only its smallest process:

```powershell
./target/release/open-hub-bench record `
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

The ratio is `candidate / baseline`. With G Hub as the baseline and Open Hub as the
candidate, a value below `1.0x` means Open Hub used less of that resource.

```powershell
./target/release/open-hub-bench compare `
  --baseline benchmark-results/g-hub-1.json `
  --baseline benchmark-results/g-hub-2.json `
  --baseline benchmark-results/g-hub-3.json `
  --candidate benchmark-results/open-hub-1.json `
  --candidate benchmark-results/open-hub-2.json `
  --candidate benchmark-results/open-hub-3.json
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
