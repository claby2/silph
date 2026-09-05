//! Storage footprint benchmark: how many bytes on disk does a stored point
//! actually cost?
//!
//! This is not a correctness test and asserts nothing about sizes — it drives
//! the real write path (insert -> compact -> retention) with a deterministic
//! synthetic workload and reports what landed on disk, so a storage change can
//! be measured before and after instead of argued about.
//!
//! ```text
//! cargo test -p silph-server --lib --release -- --ignored --nocapture footprint
//! ```
//!
//! Set `SILPH_BENCH_DAYS` to scale the simulated span (default 3), and
//! `SILPH_BENCH_OUT` to also write the report to a file, which is the
//! convenient thing to commit and diff across a change.
//!
//! What the columns mean, and which one to look at:
//!
//! - **payload** — the tsz blob bytes the encoder produced. Improves only when
//!   compression improves.
//! - **db** — the database file once the WAL is checkpointed back into it.
//!   Payload plus SQLite page and row overhead; the gap between the two is
//!   B-tree fill and blob overflow pages.
//! - **live** / **peak** — everything in the data directory (db + `-wal` +
//!   `-shm`) while the server is running. This is what actually occupies the
//!   disk of a running deployment, and it is the number that matters most.
//! - **B/pt** — `db` divided by stored points; the density figure to compare
//!   across runs, since it is independent of how long the scenario ran.
//!
//! The per-series table attributes payload to individual series, which is how
//! you find *which* metric a regression came from.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use silph_core::{METRICS, Point};

use super::{MAINTENANCE_INTERVAL, Store, decode_chunk};

/// The scrape grid scenarios use unless they say otherwise, matching the
/// server default. Points per chunk window scale inversely with this, so it
/// drives both the total volume and how close blobs get to `max_local`.
const DEFAULT_SCRAPE_INTERVAL_S: i64 = 15;
/// A recent, epoch-aligned start. Real timestamps matter: tsz stores the first
/// one in full and delta-of-deltas the rest, so starting at 0 would flatter it.
const START_S: i64 = 1_763_000_000 - (1_763_000_000 % 3600);

// ---------------------------------------------------------------------------
// Deterministic value generation
// ---------------------------------------------------------------------------

/// xorshift64*, so a run is reproducible and two runs are comparable.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.unit() * (hi - lo)
    }
}

struct DiskSim {
    mount: String,
    total: f64,
    used: f64,
}

struct SensorSim {
    name: String,
    base_c: f64,
}

/// One simulated host. Emits exactly the point set the server's `process()`
/// half would emit for that host, with values shaped the way the real ones are
/// shaped — which is the part that decides how well they compress. Ratios stay
/// ratios and byte counts stay block-granular, because Gorilla's XOR encoding
/// cares about the bit pattern, not the magnitude.
struct HostSim {
    rng: Rng,
    tick: i64,
    mem_total: f64,
    mem_available: f64,
    swap_total: f64,
    swap_used: f64,
    disks: Vec<DiskSim>,
    sensors: Vec<SensorSim>,
}

impl HostSim {
    fn new(seed: u64, mounts: &[&str], sensors: &[&str]) -> HostSim {
        let mut rng = Rng::new(seed);
        let mem_total = 8_192.0 * 1024.0 * 1024.0;
        HostSim {
            tick: 0,
            mem_total,
            mem_available: mem_total * 0.5,
            swap_total: 2.0 * 1024.0 * 1024.0 * 1024.0,
            swap_used: 0.0,
            disks: mounts
                .iter()
                .map(|m| {
                    let total = (rng.range(200.0, 1000.0) * 1e9 / 4096.0).round() * 4096.0;
                    DiskSim {
                        mount: (*m).to_string(),
                        total,
                        used: total * rng.range(0.2, 0.6),
                    }
                })
                .collect(),
            sensors: sensors
                .iter()
                .map(|s| SensorSim {
                    name: (*s).to_string(),
                    base_c: rng.range(30.0, 45.0),
                })
                .collect(),
            rng,
        }
    }

    fn step(&mut self) -> Vec<Point> {
        self.tick += 1;
        let t = self.tick as f64;
        let mut out = Vec::new();

        // cpu_usage_percent: a ratio of two jiffy deltas, as Cpu::process
        // computes it. 100 Hz * 15 s * 4 cores, jittered like real counters.
        let total = (6000.0 + self.rng.range(-4.0, 4.0)).round();
        let busy =
            (total * (0.05 + 0.30 * (t / 240.0).sin().abs() + self.rng.range(0.0, 0.06))).round();
        out.push(Point::new("cpu_usage_percent", busy / total * 100.0));

        // memory: /proc/meminfo is kB-granular, and procfs scales it to bytes.
        self.mem_available = (self.mem_available + self.rng.range(-3.0e7, 3.0e7))
            .clamp(self.mem_total * 0.15, self.mem_total * 0.85);
        let available = (self.mem_available / 1024.0).round() * 1024.0;
        let used = (self.mem_total - available).max(0.0);
        out.push(Point::new("memory_total", self.mem_total));
        out.push(Point::new("memory_used", used));
        out.push(Point::new(
            "memory_used_percent",
            used / self.mem_total * 100.0,
        ));
        out.push(Point::new("memory_swap_total", self.swap_total));
        // Swap moves rarely, and in whole pages when it does.
        if self.rng.unit() < 0.01 {
            let pages = self.rng.range(-4.0, 8.0).round();
            self.swap_used = (self.swap_used + pages * 4096.0).clamp(0.0, self.swap_total);
        }
        out.push(Point::new("memory_swap_used", self.swap_used));

        // disk: statvfs reports blocks, and f_frsize is 4096 on ext4/xfs, so
        // used space only ever moves in block multiples.
        for i in 0..self.disks.len() {
            let drift = self.rng.range(-2.0e5, 6.0e5);
            let disk = &mut self.disks[i];
            disk.used = (disk.used + drift).clamp(0.0, disk.total);
            let used = (disk.used / 4096.0).round() * 4096.0;
            let mount = disk.mount.as_str();
            out.push(Point::with_instance("disk_total", mount, disk.total));
            out.push(Point::with_instance("disk_used", mount, used));
            out.push(Point::with_instance(
                "disk_used_percent",
                mount,
                used / disk.total * 100.0,
            ));
        }

        // temperature: hwmon reports millidegrees, which the collector divides
        // by 1000 — so the stored values are thousandths, not binary fractions.
        for i in 0..self.sensors.len() {
            let jitter = self.rng.range(0.0, 1500.0);
            let sensor = &self.sensors[i];
            let milli = (sensor.base_c * 1000.0 + 8000.0 * (t / 40.0).sin() + jitter).round();
            out.push(Point::with_instance(
                "temperature_celsius",
                sensor.name.as_str(),
                milli / 1000.0,
            ));
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

struct Scenario {
    name: &'static str,
    hosts: usize,
    mounts: &'static [&'static str],
    sensors: &'static [&'static str],
    /// Simulated span. Scaled by `SILPH_BENCH_DAYS`.
    days: f64,
    retention: Duration,
    scrape_interval_s: i64,
}

const MOUNTS: &[&str] = &["/", "/home"];
const SENSORS: &[&str] = &["k10temp/Tctl", "nvme/Composite", "coretemp/Package id 0"];

fn scenarios() -> Vec<Scenario> {
    let scale: f64 = std::env::var("SILPH_BENCH_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3.0)
        / 3.0;
    vec![
        Scenario {
            name: "single-host",
            hosts: 1,
            mounts: &MOUNTS[..1],
            sensors: &SENSORS[..2],
            days: 3.0 * scale,
            retention: Duration::from_secs(30 * 86400),
            scrape_interval_s: DEFAULT_SCRAPE_INTERVAL_S,
        },
        Scenario {
            name: "fleet",
            hosts: 5,
            mounts: MOUNTS,
            sensors: SENSORS,
            days: 3.0 * scale,
            retention: Duration::from_secs(30 * 86400),
            scrape_interval_s: DEFAULT_SCRAPE_INTERVAL_S,
        },
        // Retention shorter than the run, so the file reaches the steady state
        // a long-lived deployment actually sits at: compaction and pruning have
        // both been running, and freed pages are being reused rather than
        // returned. Watch this one for freelist growth.
        Scenario {
            name: "past-retention",
            hosts: 1,
            mounts: &MOUNTS[..1],
            sensors: &SENSORS[..2],
            days: 3.0 * scale,
            retention: Duration::from_secs(86400),
            scrape_interval_s: DEFAULT_SCRAPE_INTERVAL_S,
        },
        // A faster grid is a supported config, and it triples the points packed
        // into each hourly chunk -- which is what pushes blobs over `max_local`
        // into overflow pages. Watch the overflow count here, not just the
        // default-grid scenarios.
        Scenario {
            name: "dense-scrape",
            hosts: 1,
            mounts: &MOUNTS[..1],
            sensors: &SENSORS[..2],
            days: 1.0 * scale,
            retention: Duration::from_secs(30 * 86400),
            scrape_interval_s: 5,
        },
    ]
}

// ---------------------------------------------------------------------------
// Running and reporting
// ---------------------------------------------------------------------------

/// Largest cell payload SQLite keeps inside an index or `WITHOUT ROWID` b-tree
/// page. Anything longer keeps `max_local` bytes there and spills the rest into
/// a chain of overflow pages that hold nothing else — so a blob a single byte
/// over the limit still costs an entire extra page, and a 1.7 KB blob on a 4 KB
/// page occupies about 5 KB. Chunk blobs are the only payloads in this schema
/// large enough to be at risk, and they grow with both the compression ratio
/// and the number of points per window, which is set by the scrape interval.
fn max_local(page_size: i64) -> i64 {
    (page_size - 12) * 64 / 255 - 23
}

/// Everything read out of the database at the end of a run.
struct Stats {
    series: i64,
    payload: i64,
    chunk_rows: i64,
    raw_rows: i64,
    free_bytes: i64,
    page_size: i64,
    max_local: i64,
    max_blob: i64,
    /// Chunks whose blob spills into overflow pages.
    overflowing: i64,
    per_series: Vec<(String, i64, i64)>,
}

struct Report {
    name: &'static str,
    hosts: usize,
    series: i64,
    /// Points handed to the store over the whole run.
    ingested: i64,
    /// Points the store still holds: what the measured bytes actually pay for.
    /// Differs from `ingested` once retention has pruned anything.
    stored: i64,
    payload: i64,
    chunk_rows: i64,
    raw_rows: i64,
    free_bytes: i64,
    page_size: i64,
    max_local: i64,
    max_blob: i64,
    overflowing: i64,
    live: u64,
    peak_live: u64,
    peak_wal: u64,
    closed: u64,
    retention: Duration,
    scrape_interval_s: i64,
    per_series: Vec<(String, i64, i64)>,
}

impl Report {
    /// Database bytes per stored point: the density figure to compare runs by.
    fn bytes_per_point(&self) -> f64 {
        self.closed as f64 / self.stored.max(1) as f64
    }

    /// Extrapolated database size once the scenario's retention is full.
    /// An estimate: it assumes the observed density holds, which it does as
    /// long as the series count and scrape grid do not change. The
    /// past-retention scenario is the check on it -- there the store has
    /// already reached that steady state, so the projection should land on the
    /// measured `db` column.
    fn projected(&self) -> f64 {
        let points =
            self.series as f64 * (self.retention.as_secs() as f64 / self.scrape_interval_s as f64);
        self.bytes_per_point() * points
    }
}

fn mb(bytes: impl Into<f64>) -> String {
    format!("{:.2}MB", bytes.into() / 1e6)
}

/// (total bytes in the data dir, bytes in the `-wal`).
fn dir_size(dir: &Path) -> (u64, u64) {
    let mut total = 0;
    let mut wal = 0;
    for entry in std::fs::read_dir(dir).expect("read data dir") {
        let entry = entry.expect("dir entry");
        let len = entry.metadata().expect("stat").len();
        total += len;
        if entry.file_name().to_string_lossy().ends_with("-wal") {
            wal = len;
        }
    }
    (total, wal)
}

async fn run(scenario: &Scenario) -> Report {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path(), scenario.retention).expect("open store");
    let mut sims: Vec<(String, HostSim)> = (0..scenario.hosts)
        .map(|i| {
            (
                format!("host-{i}"),
                HostSim::new(i as u64 + 1, scenario.mounts, scenario.sensors),
            )
        })
        .collect();

    let interval = scenario.scrape_interval_s;
    let ticks = (scenario.days * 86400.0 / interval as f64) as i64;
    let maintenance_every = MAINTENANCE_INTERVAL.as_secs() as i64 / interval;
    let mut points = 0i64;
    let (mut peak_live, mut peak_wal) = (0u64, 0u64);

    for tick in 0..ticks {
        let now_s = START_S + tick * interval;
        for (host, sim) in &mut sims {
            let pts = sim.step();
            points += pts.len() as i64;
            store.insert(host, now_s * 1000, pts).await.expect("insert");
        }
        // Maintenance on the same cadence the server runs it, so the raw
        // `samples` backlog — and the file's high-water mark — are realistic.
        if tick % maintenance_every == maintenance_every - 1 {
            store.compact_at(now_s).expect("compact");
        }
        let (total, wal) = dir_size(dir.path());
        peak_live = peak_live.max(total);
        peak_wal = peak_wal.max(wal);
    }

    let (live, _) = dir_size(dir.path());
    let stats = {
        let inner = store.inner.lock().expect("store mutex");
        let conn = &inner.conn;
        let scalar = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).expect("stat") };
        let page_size = scalar("PRAGMA page_size");
        let blobs = conn
            .prepare("SELECT s.metric, c.data FROM chunks c JOIN series s ON s.id = c.series_id")
            .and_then(|mut stmt| {
                stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            })
            .expect("chunk scan");

        let max_local = max_local(page_size);
        let mut overflowing = 0;
        let mut max_blob = 0;
        // Fold instances together: a per-mount breakdown would just repeat, and
        // the useful attribution is per metric.
        let mut by_metric: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for (metric, blob) in blobs {
            let len = blob.len() as i64;
            max_blob = max_blob.max(len);
            if len > max_local {
                overflowing += 1;
            }
            let n = decode_chunk(&blob).expect("decode chunk").len() as i64;
            let entry = by_metric.entry(metric).or_insert((0, 0));
            entry.0 += len;
            entry.1 += n;
        }
        Stats {
            series: scalar("SELECT COUNT(*) FROM series"),
            payload: scalar("SELECT COALESCE(SUM(LENGTH(data)), 0) FROM chunks"),
            chunk_rows: scalar("SELECT COUNT(*) FROM chunks"),
            raw_rows: scalar("SELECT COUNT(*) FROM samples"),
            free_bytes: scalar("PRAGMA freelist_count") * page_size,
            page_size,
            max_local,
            max_blob,
            overflowing,
            per_series: by_metric
                .into_iter()
                .map(|(m, (bytes, n))| (m, bytes, n))
                .collect(),
        }
    };
    // Checkpoint the WAL back into the database, so `closed` measures the
    // durable bytes rather than however much WAL happened to be outstanding.
    store.close().expect("close");
    let (closed, _) = dir_size(dir.path());

    Report {
        name: scenario.name,
        hosts: scenario.hosts,
        series: stats.series,
        ingested: points,
        // Chunk points plus rows not yet compacted.
        stored: stats.per_series.iter().map(|(_, _, n)| *n).sum::<i64>() + stats.raw_rows,
        payload: stats.payload,
        chunk_rows: stats.chunk_rows,
        raw_rows: stats.raw_rows,
        free_bytes: stats.free_bytes,
        page_size: stats.page_size,
        max_local: stats.max_local,
        max_blob: stats.max_blob,
        overflowing: stats.overflowing,
        live,
        peak_live,
        peak_wal,
        closed,
        retention: scenario.retention,
        scrape_interval_s: scenario.scrape_interval_s,
        per_series: stats.per_series,
    }
}

fn format_reports(reports: &[Report]) -> String {
    let mut out = String::new();
    writeln!(out, "silph storage footprint\n").expect("write");
    writeln!(
        out,
        "{:<16} {:>5} {:>6} {:>9} {:>9} {:>9} {:>9} {:>9} {:>7} {:>10}",
        "scenario",
        "hosts",
        "series",
        "stored",
        "payload",
        "db",
        "live",
        "peak",
        "B/pt",
        "proj@ret"
    )
    .expect("write");
    for r in reports {
        writeln!(
            out,
            "{:<16} {:>5} {:>6} {:>9} {:>9} {:>9} {:>9} {:>9} {:>7.2} {:>10}",
            r.name,
            r.hosts,
            r.series,
            r.stored,
            mb(r.payload as f64),
            mb(r.closed as f64),
            mb(r.live as f64),
            mb(r.peak_live as f64),
            r.bytes_per_point(),
            mb(r.projected()),
        )
        .expect("write");
    }

    writeln!(out, "\ndetail").expect("write");
    for r in reports {
        writeln!(
            out,
            "  {:<14} amplification {:>4.1}x (db/payload)   peak wal {:>9}   \
             freelist {:>9}   chunks {:>6}   uncompacted rows {:>6}   ingested {:>9}\n  {:<14} largest chunk {:>5} B vs {} B max_local ({} B pages) -> {} overflowing",
            r.name,
            r.closed as f64 / r.payload.max(1) as f64,
            mb(r.peak_wal as f64),
            mb(r.free_bytes as f64),
            r.chunk_rows,
            r.raw_rows,
            r.ingested,
            "",
            r.max_blob,
            r.max_local,
            r.page_size,
            r.overflowing,
        )
        .expect("write");
    }

    // Per-series attribution, from the widest scenario: this is where a
    // compression regression shows up as a single bad row.
    if let Some(r) = reports.iter().max_by_key(|r| r.series) {
        writeln!(out, "\nper-series payload ({})", r.name).expect("write");
        writeln!(
            out,
            "  {:<24} {:>10} {:>10} {:>8}",
            "metric", "points", "bytes", "B/pt"
        )
        .expect("write");
        for (metric, bytes, n) in &r.per_series {
            writeln!(
                out,
                "  {:<24} {:>10} {:>10} {:>8.2}",
                metric,
                n,
                bytes,
                *bytes as f64 / (*n).max(1) as f64
            )
            .expect("write");
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "storage size benchmark; run explicitly with --ignored"]
async fn storage_footprint() {
    let scenarios = scenarios();
    let mut reports = Vec::new();
    for scenario in &scenarios {
        reports.push(run(scenario).await);
    }
    let report = format_reports(&reports);
    println!("\n{report}");
    // Cargo runs test binaries with the package directory as the working
    // directory, so a relative path here is rarely what the caller meant.
    if let Ok(path) = std::env::var("SILPH_BENCH_OUT") {
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create report directory");
        }
        std::fs::write(&path, &report).expect("write report");
        println!("wrote {}", path.display());
    }
}

/// The benchmark is only meaningful if it exercises every series the server
/// actually stores, so adding a metric without teaching [`HostSim`] to emit it
/// should fail loudly rather than quietly shrink the workload.
#[test]
fn sim_emits_every_metric_output() {
    let mut sim = HostSim::new(1, MOUNTS, SENSORS);
    let emitted: std::collections::BTreeSet<&str> = sim.step().iter().map(|p| p.name).collect();
    let expected: std::collections::BTreeSet<&str> = METRICS
        .iter()
        .flat_map(|m| m.outputs())
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        emitted, expected,
        "HostSim must emit exactly the stored series; update it when metrics change"
    );
}
