use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Parser)]
#[command(
    name = "open-hub-bench",
    about = "Low-overhead benchmark recorder for resident desktop processes"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Record one process group to JSON and CSV.
    Record(RecordArgs),
    /// Compare summaries from two JSON reports.
    Compare(CompareArgs),
}

#[derive(Debug, Args)]
struct RecordArgs {
    /// Human-readable name stored in the report.
    #[arg(long)]
    label: String,

    /// Exact executable name to include; repeat to aggregate a process family.
    #[arg(long = "process", required = true, value_name = "NAME")]
    processes: Vec<String>,

    /// Seconds excluded before measurement, allowing CPU counters to stabilize.
    #[arg(long, default_value_t = 60)]
    warmup_seconds: u64,

    /// Measurement duration in seconds.
    #[arg(long, default_value_t = 600)]
    duration_seconds: u64,

    /// Time between samples in milliseconds.
    #[arg(long, default_value_t = 2000)]
    interval_ms: u64,

    /// Seconds between scans for new processes in the selected family.
    #[arg(long, default_value_t = 5)]
    rediscovery_seconds: u64,

    /// JSON report path; a CSV file is written beside it.
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CompareArgs {
    /// Baseline report; repeat to compare the median of multiple runs.
    #[arg(long, required = true)]
    baseline: Vec<PathBuf>,

    /// Candidate report; repeat to compare the median of multiple runs.
    #[arg(long, required = true)]
    candidate: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Report {
    schema_version: u32,
    metadata: Metadata,
    summary: Summary,
    samples: Vec<Sample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Metadata {
    label: String,
    process_names: Vec<String>,
    started_unix_ms: u128,
    os: String,
    kernel_version: Option<String>,
    architecture: String,
    logical_cpu_count: usize,
    warmup_seconds: u64,
    requested_duration_seconds: u64,
    interval_ms: u64,
    #[serde(default = "default_rediscovery_seconds")]
    rediscovery_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Summary {
    complete: bool,
    elapsed_seconds: f64,
    sample_count: usize,
    matched_sample_count: usize,
    missing_sample_count: usize,
    unique_process_count: usize,
    process_start_events: usize,
    accumulated_cpu_ms: u64,
    average_cpu_one_core_percent: f64,
    average_cpu_system_percent: f64,
    cpu_one_core_percent: Distribution,
    resident_memory_mib: Distribution,
    virtual_memory_mib: Distribution,
    process_count: Distribution,
    io_read_bytes: u64,
    io_write_bytes: u64,
    system_cpu_percent: Distribution,
    sampler_cpu_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Distribution {
    min: f64,
    mean: f64,
    p50: f64,
    p95: f64,
    max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sample {
    elapsed_ms: u64,
    interval_ms: u64,
    process_count: usize,
    pids: Vec<u32>,
    cpu_delta_ms: u64,
    cpu_one_core_percent: f64,
    cpu_system_percent: f64,
    resident_memory_bytes: u64,
    virtual_memory_bytes: u64,
    io_read_bytes: u64,
    io_write_bytes: u64,
    system_cpu_percent: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ProcessKey {
    pid: u32,
    start_time: u64,
}

#[derive(Debug, Clone, Copy)]
struct Counters {
    cpu_ms: u64,
    read_bytes: u64,
    write_bytes: u64,
}

#[derive(Debug)]
struct Snapshot {
    counters: BTreeMap<ProcessKey, Counters>,
    pids: Vec<u32>,
    resident_memory_bytes: u64,
    virtual_memory_bytes: u64,
    system_cpu_percent: f64,
    sampler: Option<Counters>,
}

struct Sampler {
    system: System,
    names: BTreeSet<String>,
    self_pid: u32,
    tracked: Vec<Pid>,
    last_discovery: Option<Instant>,
    rediscovery_interval: Duration,
}

impl Sampler {
    fn new(names: BTreeSet<String>, rediscovery_interval: Duration) -> Self {
        let mut system = System::new();
        system.refresh_cpu_all();
        Self {
            system,
            names,
            self_pid: std::process::id(),
            tracked: Vec::new(),
            last_discovery: None,
            rediscovery_interval,
        }
    }

    fn snapshot(&mut self) -> Snapshot {
        let now = Instant::now();
        if self
            .last_discovery
            .is_none_or(|last| now.saturating_duration_since(last) >= self.rediscovery_interval)
        {
            self.discover();
            self.last_discovery = Some(now);
        }

        let mut selected = self.tracked.clone();
        if let Some(pid) = self
            .system
            .processes()
            .keys()
            .copied()
            .find(|pid| pid.as_u32() == self.self_pid)
            && !selected.contains(&pid)
        {
            selected.push(pid);
        }

        let metrics = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_disk_usage()
            .without_tasks();
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::Some(&selected), true, metrics);
        self.system.refresh_cpu_usage();
        self.tracked.retain(|pid| {
            self.system
                .process(*pid)
                .is_some_and(|process| self.names.contains(&normalize_process_name(process.name())))
        });

        self.collect(selected)
    }

    fn discover(&mut self) {
        let discovery = ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .without_tasks();
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, discovery);
        self.tracked = self
            .system
            .processes()
            .iter()
            .filter_map(|(pid, process)| {
                (self.names.contains(&normalize_process_name(process.name()))).then_some(*pid)
            })
            .collect::<Vec<_>>();
    }

    fn collect(&self, selected: Vec<Pid>) -> Snapshot {
        let mut counters = BTreeMap::new();
        let mut pids = Vec::new();
        let mut resident_memory_bytes = 0_u64;
        let mut virtual_memory_bytes = 0_u64;
        let mut sampler = None;

        for pid in selected {
            let Some(process) = self.system.process(pid) else {
                continue;
            };
            let disk = process.disk_usage();
            let value = Counters {
                cpu_ms: process.accumulated_cpu_time(),
                read_bytes: disk.total_read_bytes,
                write_bytes: disk.total_written_bytes,
            };
            if pid.as_u32() == self.self_pid {
                sampler = Some(value);
                continue;
            }
            if !self.names.contains(&normalize_process_name(process.name())) {
                continue;
            }
            let key = ProcessKey {
                pid: pid.as_u32(),
                start_time: process.start_time(),
            };
            counters.insert(key, value);
            pids.push(pid.as_u32());
            resident_memory_bytes = resident_memory_bytes.saturating_add(process.memory());
            virtual_memory_bytes = virtual_memory_bytes.saturating_add(process.virtual_memory());
        }
        pids.sort_unstable();

        Snapshot {
            counters,
            pids,
            resident_memory_bytes,
            virtual_memory_bytes,
            system_cpu_percent: f64::from(self.system.global_cpu_usage()),
            sampler,
        }
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Record(args) => record(args),
        Command::Compare(args) => compare(args),
    }
}

fn record(args: RecordArgs) -> Result<()> {
    validate_record_args(&args)?;
    let names = args
        .processes
        .iter()
        .map(|name| normalize_process_name(name.as_ref()))
        .collect::<BTreeSet<_>>();
    if names.iter().any(String::is_empty) {
        bail!("process names cannot be empty");
    }

    let logical_cpu_count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let interval = Duration::from_millis(args.interval_ms);
    let mut sampler = Sampler::new(names.clone(), Duration::from_secs(args.rediscovery_seconds));
    let mut previous = sampler.snapshot();
    if previous.counters.is_empty() {
        bail!(
            "none of the requested processes are running: {}",
            names.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }

    println!(
        "Warm-up: {} s; measurement: {} s; interval: {} ms",
        args.warmup_seconds, args.duration_seconds, args.interval_ms
    );
    println!(
        "Process group: {}",
        names.iter().cloned().collect::<Vec<_>>().join(", ")
    );

    let warmup_end = Instant::now() + Duration::from_secs(args.warmup_seconds);
    while Instant::now() < warmup_end {
        sleep_until((Instant::now() + interval).min(warmup_end));
        previous = sampler.snapshot();
    }
    if previous.counters.is_empty() {
        bail!("the process group stopped during warm-up");
    }

    let started_unix_ms = unix_time_ms()?;
    let measurement_start = Instant::now();
    let measurement_end = measurement_start + Duration::from_secs(args.duration_seconds);
    let mut previous_at = measurement_start;
    let mut next_sample_at = (measurement_start + interval).min(measurement_end);
    let initial_keys = previous.counters.keys().copied().collect::<BTreeSet<_>>();
    let initial_sampler = previous.sampler;
    let mut seen_keys = initial_keys.clone();
    let mut process_start_events = 0_usize;
    let mut samples = Vec::new();

    loop {
        sleep_until(next_sample_at);
        let now = Instant::now();
        let current = sampler.snapshot();
        let interval_elapsed = now.saturating_duration_since(previous_at);
        let (cpu_delta_ms, read_bytes, write_bytes) =
            counter_deltas(&previous.counters, &current.counters);
        for key in current.counters.keys() {
            if seen_keys.insert(*key) && !initial_keys.contains(key) {
                process_start_events += 1;
            }
        }
        let interval_ms = duration_ms(interval_elapsed).max(1);
        let cpu_one_core_percent = cpu_delta_ms as f64 / interval_ms as f64 * 100.0;
        samples.push(Sample {
            elapsed_ms: duration_ms(now.saturating_duration_since(measurement_start)),
            interval_ms,
            process_count: current.counters.len(),
            pids: current.pids.clone(),
            cpu_delta_ms,
            cpu_one_core_percent,
            cpu_system_percent: cpu_one_core_percent / logical_cpu_count as f64,
            resident_memory_bytes: current.resident_memory_bytes,
            virtual_memory_bytes: current.virtual_memory_bytes,
            io_read_bytes: read_bytes,
            io_write_bytes: write_bytes,
            system_cpu_percent: current.system_cpu_percent,
        });
        previous = current;
        previous_at = now;

        if now >= measurement_end {
            break;
        }
        next_sample_at = (next_sample_at + interval).min(measurement_end);
        if next_sample_at <= now {
            next_sample_at = (now + interval).min(measurement_end);
        }
    }

    let elapsed = previous_at.saturating_duration_since(measurement_start);
    let summary = summarize(
        &samples,
        elapsed,
        logical_cpu_count,
        seen_keys.len(),
        process_start_events,
        initial_sampler,
        previous.sampler,
    );
    let metadata = Metadata {
        label: args.label.clone(),
        process_names: names.into_iter().collect(),
        started_unix_ms,
        os: System::long_os_version().unwrap_or_else(|| std::env::consts::OS.to_owned()),
        kernel_version: System::kernel_version(),
        architecture: std::env::consts::ARCH.to_owned(),
        logical_cpu_count,
        warmup_seconds: args.warmup_seconds,
        requested_duration_seconds: args.duration_seconds,
        interval_ms: args.interval_ms,
        rediscovery_seconds: args.rediscovery_seconds,
    };
    let report = Report {
        schema_version: REPORT_SCHEMA_VERSION,
        metadata,
        summary,
        samples,
    };
    let json_path = args
        .output
        .unwrap_or_else(|| default_output_path(&args.label, started_unix_ms));
    let csv_path = json_path.with_extension("csv");
    write_report(&json_path, &report)?;
    write_csv(&csv_path, &report.samples)?;
    print_summary(&report);
    println!("JSON: {}", json_path.display());
    println!("CSV:  {}", csv_path.display());
    if !report.summary.complete {
        eprintln!(
            "Warning: the process group was absent in {} sample(s); do not treat this run as a valid comparison.",
            report.summary.missing_sample_count
        );
    }
    Ok(())
}

fn compare(args: CompareArgs) -> Result<()> {
    let baseline = read_reports(&args.baseline)?;
    let candidate = read_reports(&args.candidate)?;
    warn_if_incompatible(&baseline, &candidate);
    println!(
        "Baseline:  {} ({} run{})",
        report_set_label(&baseline),
        baseline.len(),
        plural_suffix(baseline.len())
    );
    println!(
        "Candidate: {} ({} run{})",
        report_set_label(&candidate),
        candidate.len(),
        plural_suffix(candidate.len())
    );
    println!();
    println!(
        "{:<30} {:>14} {:>14} {:>12}",
        "Metric", "Baseline", "Candidate", "Ratio"
    );
    print_comparison_row(
        "CPU, one-core average (%)",
        report_median(&baseline, |report| {
            report.summary.average_cpu_one_core_percent
        }),
        report_median(&candidate, |report| {
            report.summary.average_cpu_one_core_percent
        }),
    );
    print_comparison_row(
        "Resident memory mean (MiB)",
        report_median(&baseline, |report| report.summary.resident_memory_mib.mean),
        report_median(&candidate, |report| report.summary.resident_memory_mib.mean),
    );
    print_comparison_row(
        "Resident memory p95 (MiB)",
        report_median(&baseline, |report| report.summary.resident_memory_mib.p95),
        report_median(&candidate, |report| report.summary.resident_memory_mib.p95),
    );
    print_comparison_row(
        "Virtual memory mean (MiB)",
        report_median(&baseline, |report| report.summary.virtual_memory_mib.mean),
        report_median(&candidate, |report| report.summary.virtual_memory_mib.mean),
    );
    print_comparison_row(
        "I/O read (bytes/s)",
        report_median(&baseline, |report| {
            io_rate(report.summary.io_read_bytes, report)
        }),
        report_median(&candidate, |report| {
            io_rate(report.summary.io_read_bytes, report)
        }),
    );
    print_comparison_row(
        "I/O write (bytes/s)",
        report_median(&baseline, |report| {
            io_rate(report.summary.io_write_bytes, report)
        }),
        report_median(&candidate, |report| {
            io_rate(report.summary.io_write_bytes, report)
        }),
    );
    print_comparison_row(
        "Process count mean",
        report_median(&baseline, |report| report.summary.process_count.mean),
        report_median(&candidate, |report| report.summary.process_count.mean),
    );
    print_comparison_row(
        "System CPU mean (%)",
        report_median(&baseline, |report| report.summary.system_cpu_percent.mean),
        report_median(&candidate, |report| report.summary.system_cpu_percent.mean),
    );
    print_comparison_row(
        "Sampler CPU, one-core (%)",
        report_median(&baseline, sampler_cpu_percent),
        report_median(&candidate, sampler_cpu_percent),
    );
    println!();
    println!(
        "Completeness: baseline {}/{}, candidate {}/{} matched samples",
        baseline
            .iter()
            .map(|report| report.summary.matched_sample_count)
            .sum::<usize>(),
        baseline
            .iter()
            .map(|report| report.summary.sample_count)
            .sum::<usize>(),
        candidate
            .iter()
            .map(|report| report.summary.matched_sample_count)
            .sum::<usize>(),
        candidate
            .iter()
            .map(|report| report.summary.sample_count)
            .sum::<usize>()
    );
    Ok(())
}

fn validate_record_args(args: &RecordArgs) -> Result<()> {
    if args.label.trim().is_empty() {
        bail!("--label cannot be empty");
    }
    if args.duration_seconds == 0 {
        bail!("--duration-seconds must be greater than zero");
    }
    if args.interval_ms == 0 {
        bail!("--interval-ms must be greater than zero");
    }
    if args.rediscovery_seconds == 0 {
        bail!("--rediscovery-seconds must be greater than zero");
    }
    if args.interval_ms < duration_ms(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL) {
        bail!(
            "--interval-ms must be at least {} ms on this platform",
            duration_ms(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL)
        );
    }
    if Duration::from_millis(args.interval_ms) > Duration::from_secs(args.duration_seconds) {
        bail!("the sample interval cannot exceed the measurement duration");
    }
    Ok(())
}

fn counter_deltas(
    previous: &BTreeMap<ProcessKey, Counters>,
    current: &BTreeMap<ProcessKey, Counters>,
) -> (u64, u64, u64) {
    current.iter().fold((0, 0, 0), |totals, (key, current)| {
        let Some(previous) = previous.get(key) else {
            return totals;
        };
        (
            totals
                .0
                .saturating_add(current.cpu_ms.saturating_sub(previous.cpu_ms)),
            totals
                .1
                .saturating_add(current.read_bytes.saturating_sub(previous.read_bytes)),
            totals
                .2
                .saturating_add(current.write_bytes.saturating_sub(previous.write_bytes)),
        )
    })
}

fn summarize(
    samples: &[Sample],
    elapsed: Duration,
    logical_cpu_count: usize,
    unique_process_count: usize,
    process_start_events: usize,
    initial_sampler: Option<Counters>,
    final_sampler: Option<Counters>,
) -> Summary {
    let matched = samples
        .iter()
        .filter(|sample| sample.process_count > 0)
        .collect::<Vec<_>>();
    let accumulated_cpu_ms = samples.iter().map(|sample| sample.cpu_delta_ms).sum();
    let elapsed_ms = duration_ms(elapsed).max(1);
    let average_cpu_one_core_percent = accumulated_cpu_ms as f64 / elapsed_ms as f64 * 100.0;
    let sampler_cpu_ms = match (initial_sampler, final_sampler) {
        (Some(initial), Some(final_value)) => final_value.cpu_ms.saturating_sub(initial.cpu_ms),
        _ => 0,
    };
    Summary {
        complete: matched.len() == samples.len(),
        elapsed_seconds: elapsed.as_secs_f64(),
        sample_count: samples.len(),
        matched_sample_count: matched.len(),
        missing_sample_count: samples.len().saturating_sub(matched.len()),
        unique_process_count,
        process_start_events,
        accumulated_cpu_ms,
        average_cpu_one_core_percent,
        average_cpu_system_percent: average_cpu_one_core_percent / logical_cpu_count as f64,
        cpu_one_core_percent: distribution(
            matched
                .iter()
                .map(|sample| sample.cpu_one_core_percent)
                .collect(),
        ),
        resident_memory_mib: distribution(
            matched
                .iter()
                .map(|sample| sample.resident_memory_bytes as f64 / 1_048_576.0)
                .collect(),
        ),
        virtual_memory_mib: distribution(
            matched
                .iter()
                .map(|sample| sample.virtual_memory_bytes as f64 / 1_048_576.0)
                .collect(),
        ),
        process_count: distribution(
            samples
                .iter()
                .map(|sample| sample.process_count as f64)
                .collect(),
        ),
        io_read_bytes: samples.iter().map(|sample| sample.io_read_bytes).sum(),
        io_write_bytes: samples.iter().map(|sample| sample.io_write_bytes).sum(),
        system_cpu_percent: distribution(
            samples
                .iter()
                .map(|sample| sample.system_cpu_percent)
                .collect(),
        ),
        sampler_cpu_ms,
    }
}

fn distribution(mut values: Vec<f64>) -> Distribution {
    if values.is_empty() {
        return Distribution::default();
    }
    values.sort_by(f64::total_cmp);
    Distribution {
        min: values[0],
        mean: values.iter().sum::<f64>() / values.len() as f64,
        p50: percentile(&values, 0.50),
        p95: percentile(&values, 0.95),
        max: values[values.len() - 1],
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[index]
}

fn normalize_process_name(name: &std::ffi::OsStr) -> String {
    let lowercase = name.to_string_lossy().trim().to_ascii_lowercase();
    lowercase
        .strip_suffix(".exe")
        .unwrap_or(&lowercase)
        .to_owned()
}

fn sleep_until(deadline: Instant) {
    if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        thread::sleep(remaining);
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn default_rediscovery_seconds() -> u64 {
    5
}

fn unix_time_ms() -> Result<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis())
}

fn default_output_path(label: &str, started_unix_ms: u128) -> PathBuf {
    let safe_label = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    PathBuf::from("benchmark-results").join(format!(
        "{}-{}.json",
        safe_label.trim_matches('-'),
        started_unix_ms / 1000
    ))
}

fn write_report(path: &Path, report: &Report) -> Result<()> {
    create_parent(path)?;
    let file = File::create(path)
        .with_context(|| format!("failed to create JSON report `{}`", path.display()))?;
    serde_json::to_writer_pretty(BufWriter::new(file), report)
        .with_context(|| format!("failed to write JSON report `{}`", path.display()))
}

fn write_csv(path: &Path, samples: &[Sample]) -> Result<()> {
    create_parent(path)?;
    let mut output = BufWriter::new(
        File::create(path)
            .with_context(|| format!("failed to create CSV report `{}`", path.display()))?,
    );
    writeln!(
        output,
        "elapsed_ms,interval_ms,process_count,pids,cpu_delta_ms,cpu_one_core_percent,cpu_system_percent,resident_memory_bytes,virtual_memory_bytes,io_read_bytes,io_write_bytes,system_cpu_percent"
    )?;
    for sample in samples {
        let pids = sample
            .pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(";");
        writeln!(
            output,
            "{},{},{},\"{}\",{},{:.6},{:.6},{},{},{},{},{:.6}",
            sample.elapsed_ms,
            sample.interval_ms,
            sample.process_count,
            pids,
            sample.cpu_delta_ms,
            sample.cpu_one_core_percent,
            sample.cpu_system_percent,
            sample.resident_memory_bytes,
            sample.virtual_memory_bytes,
            sample.io_read_bytes,
            sample.io_write_bytes,
            sample.system_cpu_percent
        )?;
    }
    output.flush()?;
    Ok(())
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    Ok(())
}

fn read_report(path: &Path) -> Result<Report> {
    let file = File::open(path)
        .with_context(|| format!("failed to open benchmark report `{}`", path.display()))?;
    let report: Report = serde_json::from_reader(file)
        .with_context(|| format!("failed to parse benchmark report `{}`", path.display()))?;
    if report.schema_version != REPORT_SCHEMA_VERSION {
        bail!(
            "unsupported report schema {} in `{}`",
            report.schema_version,
            path.display()
        );
    }
    Ok(report)
}

fn read_reports(paths: &[PathBuf]) -> Result<Vec<Report>> {
    paths.iter().map(|path| read_report(path)).collect()
}

fn report_set_label(reports: &[Report]) -> String {
    let labels = reports
        .iter()
        .map(|report| report.metadata.label.as_str())
        .collect::<BTreeSet<_>>();
    if labels.len() == 1 {
        labels.into_iter().next().unwrap_or_default().to_owned()
    } else {
        "mixed labels".to_owned()
    }
}

fn report_median(reports: &[Report], value: impl Fn(&Report) -> f64) -> f64 {
    let mut values = reports.iter().map(value).collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn io_rate(bytes: u64, report: &Report) -> f64 {
    bytes as f64 / report.summary.elapsed_seconds.max(f64::EPSILON)
}

fn sampler_cpu_percent(report: &Report) -> f64 {
    report.summary.sampler_cpu_ms as f64
        / (report.summary.elapsed_seconds.max(f64::EPSILON) * 1000.0)
        * 100.0
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn warn_if_incompatible(baseline: &[Report], candidate: &[Report]) {
    let reports = baseline.iter().chain(candidate);
    let platforms = reports
        .clone()
        .map(|report| {
            format!(
                "{} / {} / {} logical CPUs",
                report.metadata.os, report.metadata.architecture, report.metadata.logical_cpu_count
            )
        })
        .collect::<BTreeSet<_>>();
    if platforms.len() > 1 {
        eprintln!("Warning: reports were recorded on different platform configurations.");
    }
    let intervals = reports
        .map(|report| report.metadata.interval_ms)
        .collect::<BTreeSet<_>>();
    if intervals.len() > 1 {
        eprintln!("Warning: reports use different sample intervals.");
    }
    if baseline
        .iter()
        .chain(candidate)
        .any(|report| !report.summary.complete)
    {
        eprintln!("Warning: at least one report has missing process samples.");
    }
}

fn print_summary(report: &Report) {
    println!();
    println!("Result: {}", report.metadata.label);
    println!(
        "  CPU: {:.4}% of one core ({:.6}% of the system), {} ms accumulated",
        report.summary.average_cpu_one_core_percent,
        report.summary.average_cpu_system_percent,
        report.summary.accumulated_cpu_ms
    );
    println!(
        "  Resident memory: {:.2} MiB mean, {:.2} MiB p95",
        report.summary.resident_memory_mib.mean, report.summary.resident_memory_mib.p95
    );
    println!(
        "  I/O: {} bytes read, {} bytes written",
        report.summary.io_read_bytes, report.summary.io_write_bytes
    );
    println!(
        "  Process count: {:.2} mean; sampler overhead: {} CPU-ms",
        report.summary.process_count.mean, report.summary.sampler_cpu_ms
    );
}

fn print_comparison_row(label: &str, baseline: f64, candidate: f64) {
    let ratio = if baseline > 0.0 {
        format!("{:.3}x", candidate / baseline)
    } else if candidate == 0.0 {
        "1.000x".to_owned()
    } else {
        "n/a".to_owned()
    };
    println!("{label:<30} {baseline:>14.4} {candidate:>14.4} {ratio:>12}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn normalizes_windows_and_unix_process_names() {
        assert_eq!(
            normalize_process_name("Open-Hub-Agent.EXE".as_ref()),
            "open-hub-agent"
        );
        assert_eq!(
            normalize_process_name("open-hub-agent".as_ref()),
            "open-hub-agent"
        );
    }

    #[test]
    fn computes_only_continuing_process_deltas() {
        let continuing = ProcessKey {
            pid: 1,
            start_time: 10,
        };
        let new_process = ProcessKey {
            pid: 2,
            start_time: 20,
        };
        let previous = BTreeMap::from([(
            continuing,
            Counters {
                cpu_ms: 10,
                read_bytes: 100,
                write_bytes: 200,
            },
        )]);
        let current = BTreeMap::from([
            (
                continuing,
                Counters {
                    cpu_ms: 14,
                    read_bytes: 130,
                    write_bytes: 250,
                },
            ),
            (
                new_process,
                Counters {
                    cpu_ms: 8,
                    read_bytes: 80,
                    write_bytes: 90,
                },
            ),
        ]);
        assert_eq!(counter_deltas(&previous, &current), (4, 30, 50));
    }

    #[test]
    fn distribution_uses_nearest_rank_percentiles() {
        let stats = distribution(vec![5.0, 1.0, 4.0, 2.0, 3.0]);
        assert_eq!(stats.min, 1.0);
        assert_eq!(stats.mean, 3.0);
        assert_eq!(stats.p50, 3.0);
        assert_eq!(stats.p95, 5.0);
        assert_eq!(stats.max, 5.0);
    }
}
