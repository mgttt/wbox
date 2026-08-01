use std::env;
use std::hint::black_box;
#[cfg(windows)]
use std::process::Command;
use std::time::{Duration, Instant};

use wbox_machine::{current_host, detect_hardware, CpuFeature, HardwareCapabilities, HostOs, Isa};

#[cfg(windows)]
mod shared_windows;

const DEFAULT_ITEMS: usize = 2_000_000;
const DEFAULT_ROUNDS: u32 = 32;
const DEFAULT_REPEAT: usize = 3;
const DEFAULT_FLOP_ITERATIONS: u64 = 200_000_000;
const DEFAULT_FLOP_REPEAT: usize = 5;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const FP64_FLOPS_PER_ITERATION: u64 = 64;
#[cfg(windows)]
const CACHE_LINE: usize = 64;

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("wbox-hpc-lab: {error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    if args.first().is_some_and(|arg| arg == "worker") {
        return worker(&args[1..]);
    }
    if args.first().is_some_and(|arg| arg == "flops") {
        return flops_benchmark(parse_flops_args(&args[1..])?);
    }
    let config = parse_bench_args(&args)?;
    benchmark(config)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FlopsConfig {
    iterations: u64,
    repeat: usize,
}

fn parse_flops_args(args: &[String]) -> Result<FlopsConfig, String> {
    let mut config = FlopsConfig {
        iterations: DEFAULT_FLOP_ITERATIONS,
        repeat: DEFAULT_FLOP_REPEAT,
    };
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--iterations" => {
                config.iterations = value
                    .parse()
                    .map_err(|_| format!("invalid iteration count: {value}"))?;
            }
            "--repeat" => {
                config.repeat = value
                    .parse()
                    .map_err(|_| format!("invalid repeat count: {value}"))?;
            }
            _ => return Err(format!("unknown argument: {flag}")),
        }
        index += 2;
    }
    if config.iterations == 0 || config.repeat == 0 {
        return Err("iterations and repeat must be positive".to_owned());
    }
    Ok(config)
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn flops_benchmark(config: FlopsConfig) -> Result<(), String> {
    let hardware = detect_hardware(current_host());
    let kernel_support = KernelSupport::from_hardware(&hardware);
    let kernel = kernel_support.fp64_kernel.ok_or_else(|| {
        "FP64 throughput lab requires hardware-reported AVX2+FMA or AArch64 NEON".to_owned()
    })?;
    let logical = hardware.logical_processors.unwrap_or(1);
    println!(
        "metric=fp64-flops kernel={} logical_processors={} iterations_per_worker={} flops_per_iteration={} repeat={} statistic=median",
        kernel,
        logical,
        config.iterations,
        FP64_FLOPS_PER_ITERATION,
        config.repeat,
    );
    for workers in worker_scan(logical) {
        let measurement =
            measure_float_repeated(config.repeat, || threaded_fma(config.iterations, workers));
        let operations = fma_operation_count(config.iterations, workers);
        let gflops = operations as f64 / measurement.elapsed.as_secs_f64() / 1e9;
        println!(
            "mode=simd-threads workers={} elapsed_ms={:.3} fp64_gflops={:.3} result={:.9}",
            workers,
            millis(measurement.elapsed),
            gflops,
            measurement.value,
        );
    }
    Ok(())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn flops_benchmark(_config: FlopsConfig) -> Result<(), String> {
    Err("FP64 throughput lab is not implemented for this ISA".to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KernelSupport {
    avx2_checksum: bool,
    fp64_kernel: Option<&'static str>,
}

impl KernelSupport {
    fn from_hardware(hardware: &HardwareCapabilities) -> Self {
        let avx2 = hardware.supports_cpu_feature(CpuFeature::X86Avx2);
        let fma = hardware.supports_cpu_feature(CpuFeature::X86Fma);
        let neon = hardware.supports_cpu_feature(CpuFeature::ArmNeon);
        match hardware.native_isa {
            Some(Isa::X86_64) => Self {
                avx2_checksum: avx2,
                fp64_kernel: (avx2 && fma).then_some("avx2-fma"),
            },
            Some(Isa::Aarch64) => Self {
                avx2_checksum: false,
                fp64_kernel: neon.then_some("neon-fma"),
            },
            None => Self {
                avx2_checksum: false,
                fp64_kernel: None,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BenchConfig {
    items: usize,
    rounds: u32,
    repeat: usize,
}

fn parse_bench_args(args: &[String]) -> Result<BenchConfig, String> {
    let mut config = BenchConfig {
        items: DEFAULT_ITEMS,
        rounds: DEFAULT_ROUNDS,
        repeat: DEFAULT_REPEAT,
    };
    let mut index = usize::from(args.first().is_some_and(|arg| arg == "bench"));
    while index < args.len() {
        let flag = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--items" => {
                config.items = value
                    .parse()
                    .map_err(|_| format!("invalid item count: {value}"))?;
            }
            "--rounds" => {
                config.rounds = value
                    .parse()
                    .map_err(|_| format!("invalid round count: {value}"))?;
            }
            "--repeat" => {
                config.repeat = value
                    .parse()
                    .map_err(|_| format!("invalid repeat count: {value}"))?;
            }
            _ => return Err(format!("unknown argument: {flag}")),
        }
        index += 2;
    }
    if config.items == 0 || config.rounds == 0 || config.repeat == 0 {
        return Err("items, rounds, and repeat must be positive".to_owned());
    }
    Ok(config)
}

fn benchmark(config: BenchConfig) -> Result<(), String> {
    let BenchConfig {
        items,
        rounds,
        repeat,
    } = config;
    let host = current_host();
    let hardware = detect_hardware(host);
    let kernel_support = KernelSupport::from_hardware(&hardware);
    let logical = hardware.logical_processors.unwrap_or(1);
    let workers = worker_scan(logical);
    println!(
        "host={} isa={} logical_processors={} items={} rounds={} repeat={} statistic=median",
        host.map_or("unknown", HostOs::as_str),
        hardware
            .native_isa
            .map_or("unknown", wbox_machine::Isa::as_str),
        logical,
        items,
        rounds,
        repeat,
    );

    #[cfg(windows)]
    let mut dataset = WindowsDataset::new(items, *workers.last().unwrap())?;
    #[cfg(not(windows))]
    let mut dataset = PortableDataset::new(items);
    dataset.fill();

    let serial = measure_repeated(repeat, || checksum(dataset.data(), rounds));
    println!(
        "mode=serial workers=1 memory=shared-mapping elapsed_ms={:.3} speedup=1.000 checksum={:#018x} logical_copies=0",
        millis(serial.elapsed),
        serial.checksum,
    );

    if kernel_support.avx2_checksum {
        let simd = measure_repeated(repeat, || {
            simd_checksum(dataset.data(), rounds, kernel_support.avx2_checksum).unwrap()
        });
        ensure_checksum(serial.checksum, simd.checksum, "simd")?;
        println!(
            "mode=simd workers=1 isa=avx2 memory=borrowed-shared elapsed_ms={:.3} speedup={:.3} checksum={:#018x} logical_copies=0",
            millis(simd.elapsed),
            speedup(serial.elapsed, simd.elapsed),
            simd.checksum,
        );
    } else {
        println!("mode=simd status=unsupported reason=avx2-not-detected");
    }

    for count in workers {
        let threaded =
            measure_repeated(repeat, || threaded_checksum(dataset.data(), rounds, count));
        ensure_checksum(serial.checksum, threaded.checksum, "threads")?;
        println!(
            "mode=threads workers={} memory=borrowed-shared elapsed_ms={:.3} speedup={:.3} checksum={:#018x} logical_copies=0",
            count,
            millis(threaded.elapsed),
            speedup(serial.elapsed, threaded.elapsed),
            threaded.checksum,
        );

        if kernel_support.avx2_checksum {
            let simd_threads = measure_repeated(repeat, || {
                threaded_simd_checksum(dataset.data(), rounds, count, kernel_support.avx2_checksum)
                    .unwrap()
            });
            ensure_checksum(serial.checksum, simd_threads.checksum, "simd-threads")?;
            println!(
                "mode=simd-threads workers={} isa=avx2 memory=borrowed-shared elapsed_ms={:.3} speedup={:.3} checksum={:#018x} logical_copies=0",
                count,
                millis(simd_threads.elapsed),
                speedup(serial.elapsed, simd_threads.elapsed),
                simd_threads.checksum,
            );
        }

        #[cfg(windows)]
        {
            let processes = measure_result_repeated(repeat, || {
                dataset.clear_results(count);
                process_checksum(&dataset, rounds, count)
            })?;
            ensure_checksum(serial.checksum, processes.checksum, "processes")?;
            println!(
                "mode=processes workers={} memory=shared-mapping elapsed_ms={:.3} speedup={:.3} checksum={:#018x} logical_copies=0 cache_line_slots={}",
                count,
                millis(processes.elapsed),
                speedup(serial.elapsed, processes.elapsed),
                processes.checksum,
                CACHE_LINE,
            );
        }
        #[cfg(not(windows))]
        println!(
            "mode=processes workers={} status=unsupported reason=native-shared-mapping-adapter-not-implemented",
            count
        );
    }
    Ok(())
}

struct Measurement {
    elapsed: Duration,
    checksum: u64,
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
struct FloatMeasurement {
    elapsed: Duration,
    value: f64,
}

fn measure(action: impl FnOnce() -> u64) -> Measurement {
    let start = Instant::now();
    let checksum = black_box(action());
    Measurement {
        elapsed: start.elapsed(),
        checksum,
    }
}

fn measure_repeated(repeat: usize, mut action: impl FnMut() -> u64) -> Measurement {
    let mut results = (0..repeat)
        .map(|_| measure(&mut action))
        .collect::<Vec<_>>();
    results.sort_unstable_by_key(|result| result.elapsed);
    let checksum = results[0].checksum;
    assert!(results.iter().all(|result| result.checksum == checksum));
    results.swap_remove(results.len() / 2)
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn measure_float_repeated(repeat: usize, mut action: impl FnMut() -> f64) -> FloatMeasurement {
    let mut results = (0..repeat)
        .map(|_| {
            let start = Instant::now();
            let value = black_box(action());
            FloatMeasurement {
                elapsed: start.elapsed(),
                value,
            }
        })
        .collect::<Vec<_>>();
    assert!(results.iter().all(|result| result.value.is_finite()));
    results.sort_unstable_by_key(|result| result.elapsed);
    results.swap_remove(results.len() / 2)
}

#[cfg(windows)]
fn measure_result(action: impl FnOnce() -> Result<u64, String>) -> Result<Measurement, String> {
    let start = Instant::now();
    let checksum = black_box(action()?);
    Ok(Measurement {
        elapsed: start.elapsed(),
        checksum,
    })
}

#[cfg(windows)]
fn measure_result_repeated(
    repeat: usize,
    mut action: impl FnMut() -> Result<u64, String>,
) -> Result<Measurement, String> {
    let mut results = (0..repeat)
        .map(|_| measure_result(&mut action))
        .collect::<Result<Vec<_>, _>>()?;
    results.sort_unstable_by_key(|result| result.elapsed);
    let checksum = results[0].checksum;
    if !results.iter().all(|result| result.checksum == checksum) {
        return Err("repeated benchmark checksums differ".to_owned());
    }
    Ok(results.swap_remove(results.len() / 2))
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn speedup(serial: Duration, parallel: Duration) -> f64 {
    serial.as_secs_f64() / parallel.as_secs_f64()
}

fn worker_scan(logical: usize) -> Vec<usize> {
    let mut workers = vec![1];
    let mut count = 2;
    while count < logical {
        workers.push(count);
        count *= 2;
    }
    if logical > 1 && workers.last().copied() != Some(logical) {
        workers.push(logical);
    }
    workers
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn fma_operation_count(iterations: u64, workers: usize) -> u128 {
    u128::from(iterations) * u128::from(FP64_FLOPS_PER_ITERATION) * workers as u128
}

fn input_value(index: usize) -> u32 {
    (index as u32)
        .wrapping_mul(0x9e37_79b9)
        .rotate_left((index & 63) as u32)
        ^ 0xd1b5_4a32
}

fn mix(mut value: u32, rounds: u32) -> u32 {
    for round in 0..rounds {
        value ^= value >> 16;
        value = value.wrapping_mul(0x7feb_352d);
        value ^= value >> 15;
        value = value.wrapping_mul(0x846c_a68b);
        value ^= value >> 16;
        value = value.rotate_left((round & 31) + 1);
    }
    value
}

fn checksum(data: &[u32], rounds: u32) -> u64 {
    data.iter().fold(0, |sum, &value| {
        sum.wrapping_add(u64::from(mix(value, rounds)))
    })
}

fn threaded_checksum(data: &[u32], rounds: u32, workers: usize) -> u64 {
    std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|worker| {
                let (start, end) = partition(data.len(), worker, workers);
                scope.spawn(move || checksum(&data[start..end], rounds))
            })
            .collect::<Vec<_>>();
        handles.into_iter().fold(0_u64, |sum, handle| {
            sum.wrapping_add(handle.join().expect("HPC worker panicked"))
        })
    })
}

#[cfg(target_arch = "x86_64")]
fn threaded_simd_checksum(
    data: &[u32],
    rounds: u32,
    workers: usize,
    avx2_available: bool,
) -> Option<u64> {
    if !avx2_available {
        return None;
    }
    Some(std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|worker| {
                let (start, end) = partition(data.len(), worker, workers);
                // SAFETY: AVX2 was checked before any worker was started.
                scope.spawn(move || unsafe { checksum_avx2(&data[start..end], rounds) })
            })
            .collect::<Vec<_>>();
        handles.into_iter().fold(0_u64, |sum, handle| {
            sum.wrapping_add(handle.join().expect("HPC SIMD worker panicked"))
        })
    }))
}

#[cfg(not(target_arch = "x86_64"))]
fn threaded_simd_checksum(
    _data: &[u32],
    _rounds: u32,
    _workers: usize,
    _avx2_available: bool,
) -> Option<u64> {
    None
}

#[cfg(target_arch = "x86_64")]
fn threaded_fma(iterations: u64, workers: usize) -> f64 {
    std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|worker| {
                // SAFETY: the command checks AVX2 and FMA before starting workers.
                scope.spawn(move || unsafe { fp64_fma_kernel(iterations, worker as u64) })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("FMA worker panicked"))
            .sum()
    })
}

#[cfg(target_arch = "aarch64")]
fn threaded_fma(iterations: u64, workers: usize) -> f64 {
    std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|worker| {
                // SAFETY: Advanced SIMD and FP64 FMA are part of the AArch64
                // execution environment represented by Rust's aarch64 target.
                scope.spawn(move || unsafe { fp64_fma_kernel(iterations, worker as u64) })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("FMA worker panicked"))
            .sum()
    })
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn fp64_fma_kernel(iterations: u64, seed: u64) -> f64 {
    use std::arch::x86_64::*;

    let multiplier = _mm256_set1_pd(1.0 + f64::EPSILON);
    let addend = _mm256_set1_pd(1.0e-12);
    let base = 1.0 + seed as f64 * 1.0e-6;
    let mut a0 = _mm256_set1_pd(base + 0.01);
    let mut a1 = _mm256_set1_pd(base + 0.02);
    let mut a2 = _mm256_set1_pd(base + 0.03);
    let mut a3 = _mm256_set1_pd(base + 0.04);
    let mut a4 = _mm256_set1_pd(base + 0.05);
    let mut a5 = _mm256_set1_pd(base + 0.06);
    let mut a6 = _mm256_set1_pd(base + 0.07);
    let mut a7 = _mm256_set1_pd(base + 0.08);
    for _ in 0..black_box(iterations) {
        a0 = _mm256_fmadd_pd(a0, multiplier, addend);
        a1 = _mm256_fmadd_pd(a1, multiplier, addend);
        a2 = _mm256_fmadd_pd(a2, multiplier, addend);
        a3 = _mm256_fmadd_pd(a3, multiplier, addend);
        a4 = _mm256_fmadd_pd(a4, multiplier, addend);
        a5 = _mm256_fmadd_pd(a5, multiplier, addend);
        a6 = _mm256_fmadd_pd(a6, multiplier, addend);
        a7 = _mm256_fmadd_pd(a7, multiplier, addend);
    }
    let mut lanes = [0.0_f64; 4];
    let total = [a0, a1, a2, a3, a4, a5, a6, a7]
        .into_iter()
        .fold(0.0, |total, accumulator| {
            _mm256_storeu_pd(lanes.as_mut_ptr(), accumulator);
            total + lanes.iter().sum::<f64>()
        });
    black_box(total)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn fp64_fma_kernel(iterations: u64, seed: u64) -> f64 {
    use std::arch::aarch64::*;

    let multiplier = vdupq_n_f64(1.0 + f64::EPSILON);
    let addend = vdupq_n_f64(1.0e-12);
    let base = 1.0 + seed as f64 * 1.0e-6;
    let mut a0 = vdupq_n_f64(base + 0.01);
    let mut a1 = vdupq_n_f64(base + 0.02);
    let mut a2 = vdupq_n_f64(base + 0.03);
    let mut a3 = vdupq_n_f64(base + 0.04);
    let mut a4 = vdupq_n_f64(base + 0.05);
    let mut a5 = vdupq_n_f64(base + 0.06);
    let mut a6 = vdupq_n_f64(base + 0.07);
    let mut a7 = vdupq_n_f64(base + 0.08);
    let mut a8 = vdupq_n_f64(base + 0.09);
    let mut a9 = vdupq_n_f64(base + 0.10);
    let mut a10 = vdupq_n_f64(base + 0.11);
    let mut a11 = vdupq_n_f64(base + 0.12);
    let mut a12 = vdupq_n_f64(base + 0.13);
    let mut a13 = vdupq_n_f64(base + 0.14);
    let mut a14 = vdupq_n_f64(base + 0.15);
    let mut a15 = vdupq_n_f64(base + 0.16);
    for _ in 0..black_box(iterations) {
        a0 = vfmaq_f64(addend, a0, multiplier);
        a1 = vfmaq_f64(addend, a1, multiplier);
        a2 = vfmaq_f64(addend, a2, multiplier);
        a3 = vfmaq_f64(addend, a3, multiplier);
        a4 = vfmaq_f64(addend, a4, multiplier);
        a5 = vfmaq_f64(addend, a5, multiplier);
        a6 = vfmaq_f64(addend, a6, multiplier);
        a7 = vfmaq_f64(addend, a7, multiplier);
        a8 = vfmaq_f64(addend, a8, multiplier);
        a9 = vfmaq_f64(addend, a9, multiplier);
        a10 = vfmaq_f64(addend, a10, multiplier);
        a11 = vfmaq_f64(addend, a11, multiplier);
        a12 = vfmaq_f64(addend, a12, multiplier);
        a13 = vfmaq_f64(addend, a13, multiplier);
        a14 = vfmaq_f64(addend, a14, multiplier);
        a15 = vfmaq_f64(addend, a15, multiplier);
    }
    let mut lanes = [0.0_f64; 2];
    let total = [
        a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15,
    ]
    .into_iter()
    .fold(0.0, |total, accumulator| {
        vst1q_f64(lanes.as_mut_ptr(), accumulator);
        total + lanes.iter().sum::<f64>()
    });
    black_box(total)
}

#[cfg(target_arch = "x86_64")]
fn simd_checksum(data: &[u32], rounds: u32, avx2_available: bool) -> Option<u64> {
    if avx2_available {
        // SAFETY: the shared hardware snapshot reported AVX2 before entering the kernel.
        Some(unsafe { checksum_avx2(data, rounds) })
    } else {
        None
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn simd_checksum(_data: &[u32], _rounds: u32, _avx2_available: bool) -> Option<u64> {
    None
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn checksum_avx2(data: &[u32], rounds: u32) -> u64 {
    use std::arch::x86_64::*;

    let mut sum = 0_u64;
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let mut values = _mm256_loadu_si256(chunk.as_ptr().cast());
        for round in 0..rounds {
            values = _mm256_xor_si256(values, _mm256_srli_epi32::<16>(values));
            values = _mm256_mullo_epi32(values, _mm256_set1_epi32(0x7feb_352d));
            values = _mm256_xor_si256(values, _mm256_srli_epi32::<15>(values));
            values = _mm256_mullo_epi32(values, _mm256_set1_epi32(0x846c_a68b_u32 as i32));
            values = _mm256_xor_si256(values, _mm256_srli_epi32::<16>(values));
            let left = ((round & 31) + 1) as i32;
            let left_count = _mm256_set1_epi32(left);
            let right_count = _mm256_set1_epi32(32 - left);
            values = _mm256_or_si256(
                _mm256_sllv_epi32(values, left_count),
                _mm256_srlv_epi32(values, right_count),
            );
        }
        let mut lanes = [0_u32; 8];
        _mm256_storeu_si256(lanes.as_mut_ptr().cast(), values);
        sum = lanes
            .into_iter()
            .fold(sum, |sum, value| sum.wrapping_add(u64::from(value)));
    }
    chunks.remainder().iter().fold(sum, |sum, &value| {
        sum.wrapping_add(u64::from(mix(value, rounds)))
    })
}

fn partition(items: usize, worker: usize, workers: usize) -> (usize, usize) {
    (items * worker / workers, items * (worker + 1) / workers)
}

fn ensure_checksum(expected: u64, actual: u64, mode: &str) -> Result<(), String> {
    if expected == actual {
        Ok(())
    } else {
        Err(format!(
            "{mode} checksum mismatch: expected {expected:#x}, got {actual:#x}"
        ))
    }
}

#[cfg(windows)]
struct WindowsDataset {
    mapping: shared_windows::SharedMapping,
    items: usize,
    max_workers: usize,
}

#[cfg(windows)]
impl WindowsDataset {
    fn new(items: usize, max_workers: usize) -> Result<Self, String> {
        let name = format!(
            "Local\\wbox-hpc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let mapping =
            shared_windows::SharedMapping::create(&name, mapping_size(items, max_workers))?;
        Ok(Self {
            mapping,
            items,
            max_workers,
        })
    }

    fn fill(&mut self) {
        for (index, value) in self.data_mut().iter_mut().enumerate() {
            *value = input_value(index);
        }
    }

    fn data(&self) -> &[u32] {
        // SAFETY: the mapping is alive, aligned to allocation granularity, and
        // the first `items * size_of::<u32>()` bytes are initialized before use.
        unsafe { std::slice::from_raw_parts(self.mapping.as_ptr().cast(), self.items) }
    }

    fn data_mut(&mut self) -> &mut [u32] {
        // SAFETY: `&mut self` guarantees no other slice is active during setup.
        unsafe { std::slice::from_raw_parts_mut(self.mapping.as_mut_ptr().cast(), self.items) }
    }

    fn clear_results(&mut self, workers: usize) {
        assert!(workers <= self.max_workers);
        // SAFETY: each result slot is within the mapped result region.
        unsafe {
            std::ptr::write_bytes(self.result_ptr(0), 0, workers * CACHE_LINE);
        }
    }

    unsafe fn result_ptr(&self, worker: usize) -> *mut u8 {
        self.mapping
            .as_mut_ptr()
            .add(result_offset(self.items) + worker * CACHE_LINE)
    }
}

#[cfg(not(windows))]
struct PortableDataset {
    data: Vec<u32>,
}

#[cfg(not(windows))]
impl PortableDataset {
    fn new(items: usize) -> Self {
        Self {
            data: vec![0; items],
        }
    }

    fn fill(&mut self) {
        for (index, value) in self.data.iter_mut().enumerate() {
            *value = input_value(index);
        }
    }

    fn data(&self) -> &[u32] {
        &self.data
    }
}

#[cfg(windows)]
fn process_checksum(dataset: &WindowsDataset, rounds: u32, workers: usize) -> Result<u64, String> {
    let executable = env::current_exe().map_err(|error| error.to_string())?;
    let mut children = Vec::with_capacity(workers);
    for worker in 0..workers {
        let child = Command::new(&executable)
            .args([
                "worker",
                dataset.mapping.name(),
                &dataset.items.to_string(),
                &dataset.max_workers.to_string(),
                &rounds.to_string(),
                &worker.to_string(),
                &workers.to_string(),
            ])
            .spawn()
            .map_err(|error| format!("spawn worker {worker}: {error}"))?;
        children.push(child);
    }
    for (worker, child) in children.iter_mut().enumerate() {
        let status = child
            .wait()
            .map_err(|error| format!("wait worker {worker}: {error}"))?;
        if !status.success() {
            return Err(format!("worker {worker} exited with {status}"));
        }
    }
    let mut sum = 0_u64;
    for worker in 0..workers {
        // SAFETY: workers have exited; each initialized its own aligned slot.
        let value = unsafe { dataset.result_ptr(worker).cast::<u64>().read() };
        sum = sum.wrapping_add(value);
    }
    Ok(sum)
}

#[cfg(windows)]
fn worker(args: &[String]) -> Result<(), String> {
    if args.len() != 6 {
        return Err(
            "worker requires mapping, items, max-workers, rounds, index, workers".to_owned(),
        );
    }
    let name = &args[0];
    let items = parse::<usize>(&args[1], "items")?;
    let max_workers = parse::<usize>(&args[2], "max-workers")?;
    let rounds = parse::<u32>(&args[3], "rounds")?;
    let worker = parse::<usize>(&args[4], "worker")?;
    let workers = parse::<usize>(&args[5], "workers")?;
    if worker >= workers || workers > max_workers {
        return Err("invalid worker partition".to_owned());
    }
    let mapping = shared_windows::SharedMapping::open(name, mapping_size(items, max_workers))?;
    // SAFETY: parent initialized the data region before spawning this worker.
    let data = unsafe { std::slice::from_raw_parts(mapping.as_ptr().cast::<u32>(), items) };
    let (start, end) = partition(items, worker, workers);
    let partial = checksum(&data[start..end], rounds);
    // SAFETY: this worker owns one cache-line-sized result slot.
    unsafe {
        mapping
            .as_mut_ptr()
            .add(result_offset(items) + worker * CACHE_LINE)
            .cast::<u64>()
            .write(partial);
    }
    Ok(())
}

#[cfg(not(windows))]
fn worker(_args: &[String]) -> Result<(), String> {
    Err("shared-mapping workers are currently implemented only on Windows".to_owned())
}

#[cfg(windows)]
fn parse<T: std::str::FromStr>(value: &str, name: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {name}: {value}"))
}

#[cfg(windows)]
fn result_offset(items: usize) -> usize {
    (items * std::mem::size_of::<u32>() + CACHE_LINE - 1) & !(CACHE_LINE - 1)
}

#[cfg(windows)]
fn mapping_size(items: usize, workers: usize) -> usize {
    result_offset(items) + workers * CACHE_LINE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_cover_input_exactly() {
        for workers in 1..=9 {
            let ranges = (0..workers)
                .map(|worker| partition(101, worker, workers))
                .collect::<Vec<_>>();
            assert_eq!(ranges.first().unwrap().0, 0);
            assert_eq!(ranges.last().unwrap().1, 101);
            for pair in ranges.windows(2) {
                assert_eq!(pair[0].1, pair[1].0);
            }
        }
    }

    #[test]
    fn threaded_checksum_matches_serial() {
        let data = (0..257).map(input_value).collect::<Vec<_>>();
        let expected = checksum(&data, 5);
        for workers in [1, 2, 4, 8] {
            assert_eq!(threaded_checksum(&data, 5, workers), expected);
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn avx2_checksum_matches_scalar_when_available() {
        let data = (0..259).map(input_value).collect::<Vec<_>>();
        let support = KernelSupport::from_hardware(&detect_hardware(current_host()));
        if support.avx2_checksum {
            let expected = checksum(&data, 7);
            // SAFETY: guarded by the shared hardware capability snapshot.
            assert_eq!(unsafe { checksum_avx2(&data, 7) }, expected);
            for workers in [1, 2, 4, 8] {
                assert_eq!(
                    threaded_simd_checksum(&data, 7, workers, support.avx2_checksum),
                    Some(expected)
                );
            }
        }
    }

    #[test]
    fn worker_scan_includes_hardware_limit() {
        assert_eq!(worker_scan(1), vec![1]);
        assert_eq!(worker_scan(6), vec![1, 2, 4, 6]);
        assert_eq!(worker_scan(8), vec![1, 2, 4, 8]);
    }

    #[test]
    fn kernel_selection_requires_matching_machine_facts() {
        let mut hardware = detect_hardware(None);

        hardware.native_isa = Some(Isa::X86_64);
        hardware.cpu_features = vec![CpuFeature::X86Avx2];
        assert_eq!(
            KernelSupport::from_hardware(&hardware),
            KernelSupport {
                avx2_checksum: true,
                fp64_kernel: None,
            }
        );
        hardware.cpu_features.push(CpuFeature::X86Fma);
        assert_eq!(
            KernelSupport::from_hardware(&hardware).fp64_kernel,
            Some("avx2-fma")
        );

        hardware.native_isa = Some(Isa::Aarch64);
        assert_eq!(
            KernelSupport::from_hardware(&hardware),
            KernelSupport {
                avx2_checksum: false,
                fp64_kernel: None,
            }
        );
        hardware.cpu_features.push(CpuFeature::ArmNeon);
        assert_eq!(
            KernelSupport::from_hardware(&hardware).fp64_kernel,
            Some("neon-fma")
        );
    }

    #[test]
    #[cfg(windows)]
    fn result_region_is_cache_line_aligned() {
        for items in [1, 7, 8, 9, 1000] {
            assert_eq!(result_offset(items) % CACHE_LINE, 0);
        }
    }

    #[test]
    fn arguments_include_repeat_count() {
        let args = [
            "bench".to_owned(),
            "--items".to_owned(),
            "10".to_owned(),
            "--rounds".to_owned(),
            "2".to_owned(),
            "--repeat".to_owned(),
            "5".to_owned(),
        ];
        assert_eq!(
            parse_bench_args(&args).unwrap(),
            BenchConfig {
                items: 10,
                rounds: 2,
                repeat: 5,
            }
        );
    }

    #[test]
    fn flops_arguments_are_parsed() {
        let args = [
            "--iterations".to_owned(),
            "123".to_owned(),
            "--repeat".to_owned(),
            "7".to_owned(),
        ];
        assert_eq!(
            parse_flops_args(&args).unwrap(),
            FlopsConfig {
                iterations: 123,
                repeat: 7,
            }
        );
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn fma_kernel_returns_finite_result_when_available() {
        let support = KernelSupport::from_hardware(&detect_hardware(current_host()));
        if support.fp64_kernel == Some("avx2-fma") {
            // SAFETY: guarded by the shared AVX2 and FMA capability snapshot.
            assert!(unsafe { fp64_fma_kernel(10, 0) }.is_finite());
        }
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn neon_fma_kernel_returns_finite_result() {
        let support = KernelSupport::from_hardware(&detect_hardware(current_host()));
        if support.fp64_kernel == Some("neon-fma") {
            // SAFETY: guarded by the shared AArch64 NEON capability snapshot.
            assert!(unsafe { fp64_fma_kernel(10, 0) }.is_finite());
        }
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn fma_operation_accounting_counts_lanes_and_mul_add() {
        assert_eq!(fma_operation_count(10, 4), 2_560);
    }
}
