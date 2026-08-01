use std::env;
use std::hint::black_box;
use std::process::Command;
use std::time::{Duration, Instant};

use agenterm_platform::shared_memory::SharedMemory;
use wbox_machine::{
    current_host, detect_hardware, detect_host_memory, CpuFeature, HardwareCapabilities, HostOs,
    Isa,
};

const DEFAULT_ITEMS: usize = 2_000_000;
const DEFAULT_ROUNDS: u32 = 32;
const DEFAULT_REPEAT: usize = 3;
const DEFAULT_FLOP_ITERATIONS: u64 = 200_000_000;
const DEFAULT_FLOP_REPEAT: usize = 5;
const DEFAULT_MEMORY_MIB: usize = 128;
const DEFAULT_MEMORY_PASSES: usize = 3;
const DEFAULT_MEMORY_REPEAT: usize = 3;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const FP64_FLOPS_PER_ITERATION: u64 = 64;

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
    if args.first().is_some_and(|arg| arg == "memory") {
        return memory_benchmark(parse_memory_args(&args[1..])?);
    }
    let config = parse_bench_args(&args)?;
    benchmark(config)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryConfig {
    bytes: usize,
    passes: usize,
    repeat: usize,
}

fn parse_memory_args(args: &[String]) -> Result<MemoryConfig, String> {
    let mut mib = DEFAULT_MEMORY_MIB;
    let mut config = MemoryConfig {
        bytes: 0,
        passes: DEFAULT_MEMORY_PASSES,
        repeat: DEFAULT_MEMORY_REPEAT,
    };
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--mib" => {
                mib = value
                    .parse()
                    .map_err(|_| format!("invalid MiB count: {value}"))?;
            }
            "--passes" => {
                config.passes = value
                    .parse()
                    .map_err(|_| format!("invalid pass count: {value}"))?;
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
    if mib == 0 || config.passes == 0 || config.repeat == 0 {
        return Err("MiB, passes, and repeat must be positive".to_owned());
    }
    config.bytes = mib
        .checked_mul(1024 * 1024)
        .ok_or_else(|| "memory dataset size overflows usize".to_owned())?;
    Ok(config)
}

fn memory_benchmark(config: MemoryConfig) -> Result<(), String> {
    let memory = detect_host_memory().map_err(|error| error.to_string())?;
    let page_size = memory.page_size.get();
    let cache_line = cache_line_bytes(&detect_hardware(current_host()))?;
    let mut dataset = MemoryDataset::new(config.bytes)?;
    println!(
        "metric=memory-bandwidth memory=shared-mapping dataset_bytes={} mapping_bytes={} page_size={} cache_line_bytes={} physical_memory_bytes={} passes={} repeat={} statistic=median",
        config.bytes,
        dataset.mapping_bytes(),
        page_size,
        cache_line,
        memory.physical_bytes,
        config.passes,
        config.repeat,
    );

    let cold_touch = measure(|| dataset.touch_pages(page_size, 0x5a));
    let warm_touch = measure(|| dataset.touch_pages(page_size, 0xa5));
    let pages = pages_spanned(dataset.mapping_bytes(), page_size)?;
    println!(
        "mode=page-touch phase=cold pages={} elapsed_ms={:.3} ns_per_page={:.3} checksum={:#018x}",
        pages,
        millis(cold_touch.elapsed),
        nanos_per_unit(cold_touch.elapsed, pages),
        cold_touch.checksum,
    );
    println!(
        "mode=page-touch phase=warm pages={} elapsed_ms={:.3} ns_per_page={:.3} checksum={:#018x}",
        pages,
        millis(warm_touch.elapsed),
        nanos_per_unit(warm_touch.elapsed, pages),
        warm_touch.checksum,
    );

    dataset.initialize_source();
    let read = measure_repeated(config.repeat, || dataset.read(config.passes));
    print_bandwidth("read", read, config.bytes, config.passes, 1)?;

    let write = measure_repeated(config.repeat, || dataset.write(config.passes));
    print_bandwidth("write", write, config.bytes, config.passes, 1)?;
    dataset.verify_write(config.passes)?;

    let copy = measure_repeated(config.repeat, || dataset.copy(config.passes));
    print_bandwidth("copy", copy, config.bytes, config.passes, 2)?;
    dataset.verify_copy()?;
    Ok(())
}

fn print_bandwidth(
    mode: &str,
    measurement: Measurement,
    bytes: usize,
    passes: usize,
    traffic_factor: usize,
) -> Result<(), String> {
    let payload_bytes = bytes
        .checked_mul(passes)
        .ok_or_else(|| "memory payload byte count overflows usize".to_owned())?;
    let traffic_bytes = payload_bytes
        .checked_mul(traffic_factor)
        .ok_or_else(|| "memory traffic byte count overflows usize".to_owned())?;
    println!(
        "mode={} elapsed_ms={:.3} payload_gib_s={:.3} traffic_gib_s={:.3} payload_bytes={} traffic_bytes={} checksum={:#018x}",
        mode,
        millis(measurement.elapsed),
        gib_per_second(payload_bytes, measurement.elapsed),
        gib_per_second(traffic_bytes, measurement.elapsed),
        payload_bytes,
        traffic_bytes,
        measurement.checksum,
    );
    Ok(())
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
    let cache_line = cache_line_bytes(&hardware)?;
    let logical = hardware.logical_processors.unwrap_or(1);
    let workers = worker_scan(logical);
    println!(
        "host={} isa={} logical_processors={} cache_line_bytes={} items={} rounds={} repeat={} statistic=median",
        host.map_or("unknown", HostOs::as_str),
        hardware
            .native_isa
            .map_or("unknown", wbox_machine::Isa::as_str),
        logical,
        cache_line,
        items,
        rounds,
        repeat,
    );

    let mut dataset = SharedDataset::new(items, *workers.last().unwrap(), cache_line)?;
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
            dataset.cache_line,
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

fn measure_result(action: impl FnOnce() -> Result<u64, String>) -> Result<Measurement, String> {
    let start = Instant::now();
    let checksum = black_box(action()?);
    Ok(Measurement {
        elapsed: start.elapsed(),
        checksum,
    })
}

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

fn pages_spanned(bytes: usize, page_size: usize) -> Result<usize, String> {
    if page_size == 0 {
        return Err("page size must be positive".to_owned());
    }
    bytes
        .checked_add(page_size - 1)
        .map(|rounded| rounded / page_size)
        .ok_or_else(|| "page count overflows usize".to_owned())
}

fn nanos_per_unit(duration: Duration, units: usize) -> f64 {
    duration.as_secs_f64() * 1e9 / units as f64
}

fn gib_per_second(bytes: usize, duration: Duration) -> f64 {
    bytes as f64 / 1024_f64.powi(3) / duration.as_secs_f64()
}

struct MemoryDataset {
    mapping: SharedMemory,
    bytes: usize,
}

impl MemoryDataset {
    fn new(bytes: usize) -> Result<Self, String> {
        if bytes == 0 || !bytes.is_multiple_of(std::mem::size_of::<u64>()) {
            return Err(
                "memory dataset must contain a positive whole number of u64 values".to_owned(),
            );
        }
        let mapping_bytes = bytes
            .checked_mul(2)
            .ok_or_else(|| "memory mapping size overflows usize".to_owned())?;
        let name = format!(
            "wbox-memory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let mapping =
            SharedMemory::create(&name, mapping_bytes).map_err(|error| error.to_string())?;
        Ok(Self { mapping, bytes })
    }

    fn mapping_bytes(&self) -> usize {
        self.bytes * 2
    }

    fn touch_pages(&mut self, page_size: usize, pattern: u8) -> u64 {
        let mut checksum = 0_u64;
        for offset in (0..self.mapping_bytes()).step_by(page_size) {
            // SAFETY: every offset is below mapping_bytes and the mapping is writable.
            unsafe {
                let pointer = self.mapping.as_mut_ptr().add(offset);
                pointer.write_volatile(pattern);
                checksum = checksum.wrapping_add(u64::from(pointer.read_volatile()));
            }
        }
        checksum
    }

    fn initialize_source(&mut self) {
        let source_values = self.bytes / 8;
        let (source, destination) = self.values_mut().split_at_mut(source_values);
        for (index, value) in source.iter_mut().enumerate() {
            *value = u64::from(input_value(index)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
        destination.fill(0);
    }

    fn read(&self, passes: usize) -> u64 {
        let source = std::hint::black_box(self.source());
        let mut checksum = 0_u64;
        for _ in 0..passes {
            checksum = source.iter().copied().fold(checksum, u64::wrapping_add);
        }
        std::hint::black_box(checksum)
    }

    fn write(&mut self, passes: usize) -> u64 {
        let destination = self.destination_mut();
        for pass in 0..passes {
            let byte = 0x31_u8.wrapping_add(pass as u8);
            // The black-box barrier makes each complete write pass observable.
            std::hint::black_box(&mut *destination).fill(byte);
        }
        memory_sample(destination)
    }

    fn copy(&mut self, passes: usize) -> u64 {
        let split = self.bytes;
        let (source, destination) = self.mapping_mut().split_at_mut(split);
        for _ in 0..passes {
            destination.copy_from_slice(source);
            std::hint::black_box(&mut *destination);
        }
        memory_sample(destination)
    }

    fn verify_write(&self, passes: usize) -> Result<(), String> {
        let expected = 0x31_u8.wrapping_add((passes - 1) as u8);
        if self.destination().iter().all(|value| *value == expected) {
            Ok(())
        } else {
            Err("sequential write verification failed".to_owned())
        }
    }

    fn verify_copy(&self) -> Result<(), String> {
        if self.source_bytes() == self.destination() {
            Ok(())
        } else {
            Err("memory copy verification failed".to_owned())
        }
    }

    fn source(&self) -> &[u64] {
        // SAFETY: mappings are page-aligned and `bytes` is a multiple of u64.
        unsafe { std::slice::from_raw_parts(self.mapping.as_ptr().cast(), self.bytes / 8) }
    }

    fn destination_mut(&mut self) -> &mut [u8] {
        let bytes = self.bytes;
        &mut self.mapping_mut()[bytes..]
    }

    fn source_bytes(&self) -> &[u8] {
        &self.mapping()[..self.bytes]
    }

    fn destination(&self) -> &[u8] {
        &self.mapping()[self.bytes..]
    }

    fn values_mut(&mut self) -> &mut [u64] {
        let values = self.mapping_bytes() / 8;
        // SAFETY: mappings are page-aligned and mapping_bytes is a multiple of u64.
        unsafe { std::slice::from_raw_parts_mut(self.mapping.as_mut_ptr().cast(), values) }
    }

    fn mapping_mut(&mut self) -> &mut [u8] {
        let bytes = self.mapping_bytes();
        // SAFETY: the mapping remains alive and `&mut self` makes the view exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.mapping.as_mut_ptr(), bytes) }
    }

    fn mapping(&self) -> &[u8] {
        // SAFETY: the mapping remains alive for the returned shared view.
        unsafe { std::slice::from_raw_parts(self.mapping.as_ptr(), self.mapping_bytes()) }
    }
}

fn memory_sample(bytes: &[u8]) -> u64 {
    let stride = (bytes.len() / 64).max(1);
    bytes
        .iter()
        .step_by(stride)
        .fold(0_u64, |sum, value| sum.wrapping_add(u64::from(*value)))
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

struct SharedDataset {
    mapping: SharedMemory,
    items: usize,
    max_workers: usize,
    result_offset: usize,
    cache_line: usize,
}

impl SharedDataset {
    fn new(items: usize, max_workers: usize, cache_line: usize) -> Result<Self, String> {
        let (result_offset, mapping_size) = mapping_layout(items, max_workers, cache_line)?;
        let name = format!(
            "wbox-hpc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let mapping =
            SharedMemory::create(&name, mapping_size).map_err(|error| error.to_string())?;
        Ok(Self {
            mapping,
            items,
            max_workers,
            result_offset,
            cache_line,
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
            std::ptr::write_bytes(self.result_mut_ptr(0), 0, workers * self.cache_line);
        }
    }

    unsafe fn result_mut_ptr(&mut self, worker: usize) -> *mut u8 {
        self.mapping
            .as_mut_ptr()
            .add(self.result_offset + worker * self.cache_line)
    }

    unsafe fn result_ptr(&self, worker: usize) -> *const u8 {
        self.mapping
            .as_ptr()
            .add(self.result_offset + worker * self.cache_line)
    }
}

fn process_checksum(dataset: &SharedDataset, rounds: u32, workers: usize) -> Result<u64, String> {
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
                &dataset.cache_line.to_string(),
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

fn worker(args: &[String]) -> Result<(), String> {
    if args.len() != 7 {
        return Err(
            "worker requires mapping, items, max-workers, rounds, index, workers, cache-line"
                .to_owned(),
        );
    }
    let name = &args[0];
    let items = parse::<usize>(&args[1], "items")?;
    let max_workers = parse::<usize>(&args[2], "max-workers")?;
    let rounds = parse::<u32>(&args[3], "rounds")?;
    let worker = parse::<usize>(&args[4], "worker")?;
    let workers = parse::<usize>(&args[5], "workers")?;
    let cache_line = parse::<usize>(&args[6], "cache-line")?;
    if worker >= workers || workers > max_workers {
        return Err("invalid worker partition".to_owned());
    }
    let (result_offset, mapping_size) = mapping_layout(items, max_workers, cache_line)?;
    let mut mapping = SharedMemory::open(name, mapping_size).map_err(|error| error.to_string())?;
    // SAFETY: parent initialized the data region before spawning this worker.
    let data = unsafe { std::slice::from_raw_parts(mapping.as_ptr().cast::<u32>(), items) };
    let (start, end) = partition(items, worker, workers);
    let partial = checksum(&data[start..end], rounds);
    // SAFETY: this worker owns one cache-line-sized result slot.
    unsafe {
        mapping
            .as_mut_ptr()
            .add(result_offset + worker * cache_line)
            .cast::<u64>()
            .write(partial);
    }
    Ok(())
}

fn parse<T: std::str::FromStr>(value: &str, name: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {name}: {value}"))
}

fn mapping_layout(
    items: usize,
    workers: usize,
    cache_line: usize,
) -> Result<(usize, usize), String> {
    if cache_line == 0 || !cache_line.is_multiple_of(std::mem::align_of::<u64>()) {
        return Err("cache line must preserve u64 alignment".to_owned());
    }
    let data_bytes = items
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| "shared mapping data size overflows usize".to_owned())?;
    let result_offset = data_bytes
        .checked_add(cache_line - 1)
        .and_then(|size| size.checked_div(cache_line))
        .and_then(|units| units.checked_mul(cache_line))
        .ok_or_else(|| "shared mapping alignment overflows usize".to_owned())?;
    let result_bytes = workers
        .checked_mul(cache_line)
        .ok_or_else(|| "shared mapping result size overflows usize".to_owned())?;
    let mapping_size = result_offset
        .checked_add(result_bytes)
        .ok_or_else(|| "shared mapping total size overflows usize".to_owned())?;
    Ok((result_offset, mapping_size))
}

fn cache_line_bytes(hardware: &HardwareCapabilities) -> Result<usize, String> {
    let hierarchy = hardware
        .cache_hierarchy
        .as_ref()
        .ok_or_else(|| "current host cache hierarchy was not probed".to_owned())?
        .as_ref()
        .map_err(ToString::to_string)?;
    let line = hierarchy
        .max_data_line_bytes()
        .ok_or_else(|| "host reported no data-bearing cache line".to_owned())?;
    usize::try_from(line.get()).map_err(|_| "cache line does not fit usize".to_owned())
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
    fn result_region_is_cache_line_aligned() {
        for cache_line in [64, 128, 192] {
            for items in [1, 7, 8, 9, 1000] {
                assert_eq!(
                    mapping_layout(items, 8, cache_line).unwrap().0 % cache_line,
                    0
                );
            }
        }
        assert!(mapping_layout(1, 1, 0).is_err());
        assert!(mapping_layout(1, 1, 10).is_err());
    }

    #[test]
    fn detected_cache_line_drives_the_shared_mapping_layout() {
        let hardware = detect_hardware(current_host());
        let cache_line = cache_line_bytes(&hardware).unwrap();
        let (result_offset, _) = mapping_layout(257, 8, cache_line).unwrap();
        assert_eq!(result_offset % cache_line, 0);
        assert!(cache_line.is_multiple_of(std::mem::align_of::<u64>()));
    }

    #[test]
    fn shared_mapping_layout_rejects_overflow() {
        assert!(mapping_layout(usize::MAX, 1, 64).is_err());
        assert!(mapping_layout(1, usize::MAX, 64).is_err());
    }

    #[test]
    fn shared_mapping_rejects_a_view_larger_than_the_object() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("wbox-hpc-size-test-{}-{nonce}", std::process::id());
        let _creator = SharedMemory::create(&name, 4096).unwrap();
        let error = SharedMemory::open(&name, 8192).unwrap_err();
        #[cfg(windows)]
        assert_eq!(
            error.kind(),
            agenterm_platform::shared_memory::SharedMemoryErrorKind::Map
        );
        #[cfg(unix)]
        assert_eq!(
            error.kind(),
            agenterm_platform::shared_memory::SharedMemoryErrorKind::SizeMismatch
        );
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
    fn memory_arguments_are_parsed_with_checked_mib_conversion() {
        let args = [
            "--mib".to_owned(),
            "2".to_owned(),
            "--passes".to_owned(),
            "4".to_owned(),
            "--repeat".to_owned(),
            "5".to_owned(),
        ];
        assert_eq!(
            parse_memory_args(&args).unwrap(),
            MemoryConfig {
                bytes: 2 * 1024 * 1024,
                passes: 4,
                repeat: 5,
            }
        );
        assert!(parse_memory_args(&["--mib".to_owned(), "0".to_owned()]).is_err());
        assert!(parse_memory_args(&["--mib".to_owned(), usize::MAX.to_string()]).is_err());
    }

    #[test]
    fn page_count_rounds_up_and_rejects_overflow() {
        assert_eq!(pages_spanned(1, 4096).unwrap(), 1);
        assert_eq!(pages_spanned(4096, 4096).unwrap(), 1);
        assert_eq!(pages_spanned(4097, 4096).unwrap(), 2);
        assert!(pages_spanned(1, 0).is_err());
        assert!(pages_spanned(usize::MAX, 4096).is_err());
    }

    #[test]
    fn memory_kernels_touch_read_write_and_copy_the_shared_mapping() {
        let mut dataset = MemoryDataset::new(4096).unwrap();
        assert_eq!(dataset.mapping_bytes(), 8192);
        assert_eq!(dataset.touch_pages(4096, 7), 14);
        dataset.initialize_source();
        let read = dataset.read(2);
        assert_eq!(read, dataset.read(2));
        assert_ne!(read, 0);
        let write = dataset.write(2);
        assert_ne!(write, 0);
        dataset.verify_write(2).unwrap();
        let copied = dataset.copy(2);
        assert_ne!(copied, 0);
        assert_ne!(copied, write);
        dataset.verify_copy().unwrap();
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
