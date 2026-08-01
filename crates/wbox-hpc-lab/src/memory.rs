use std::time::Duration;

use agenterm_platform::{
    process_metrics::{self, PageFaultCounters},
    shared_memory::SharedMemory,
};
use wbox_machine::{current_host, detect_hardware, detect_host_memory};

use crate::{
    cache_line_bytes, input_value, measure, measure_repeated, millis, partition, worker_scan,
    Measurement,
};

const DEFAULT_MIB: usize = 128;
const DEFAULT_PASSES: usize = 3;
const DEFAULT_REPEAT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Config {
    bytes: usize,
    passes: usize,
    repeat: usize,
}

pub(crate) fn run(args: &[String]) -> Result<(), String> {
    benchmark(parse_args(args)?)
}

fn parse_args(args: &[String]) -> Result<Config, String> {
    let mut mib = DEFAULT_MIB;
    let mut config = Config {
        bytes: 0,
        passes: DEFAULT_PASSES,
        repeat: DEFAULT_REPEAT,
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

fn benchmark(config: Config) -> Result<(), String> {
    let memory = detect_host_memory().map_err(|error| error.to_string())?;
    let page_size = memory.page_size.get();
    let hardware = detect_hardware(current_host());
    let cache_line = cache_line_bytes(&hardware)?;
    let logical = hardware.logical_processors.unwrap_or(1);
    let worker_counts = worker_scan(logical);
    let single_direction = TrafficAccounting::new(config.bytes, config.passes, 1);
    let copy_traffic = TrafficAccounting::new(config.bytes, config.passes, 2);
    let mut dataset = Dataset::new(config.bytes)?;
    println!(
        "metric=memory-bandwidth memory=shared-mapping dataset_bytes={} mapping_bytes={} page_size={} cache_line_bytes={} physical_memory_bytes={} logical_processors={} passes={} repeat={} statistic=median",
        config.bytes,
        dataset.mapping_bytes(),
        page_size,
        cache_line,
        memory.physical_bytes,
        logical,
        config.passes,
        config.repeat,
    );

    let (cold_touch, cold_faults) =
        sample_page_faults(|| measure(|| dataset.touch_pages(page_size, 0x5a)))?;
    let (warm_touch, warm_faults) =
        sample_page_faults(|| measure(|| dataset.touch_pages(page_size, 0xa5)))?;
    let pages = pages_spanned(dataset.mapping_bytes(), page_size)?;
    println!(
        "mode=page-touch phase=cold pages={} elapsed_ms={:.3} ns_per_page={:.3} faults_total={} faults_soft={} faults_hard={} checksum={:#018x}",
        pages,
        millis(cold_touch.elapsed),
        nanos_per_unit(cold_touch.elapsed, pages),
        cold_faults.total,
        optional_faults(cold_faults.soft),
        optional_faults(cold_faults.hard),
        cold_touch.checksum,
    );
    println!(
        "mode=page-touch phase=warm pages={} elapsed_ms={:.3} ns_per_page={:.3} faults_total={} faults_soft={} faults_hard={} checksum={:#018x}",
        pages,
        millis(warm_touch.elapsed),
        nanos_per_unit(warm_touch.elapsed, pages),
        warm_faults.total,
        optional_faults(warm_faults.soft),
        optional_faults(warm_faults.hard),
        warm_touch.checksum,
    );

    dataset.initialize_source();
    let (read, read_faults) =
        sample_page_faults(|| measure_repeated(config.repeat, || dataset.read(config.passes)))?;
    print_bandwidth(
        "read",
        1,
        &read,
        read.elapsed,
        single_direction,
        read_faults,
    )?;

    let (write, write_faults) =
        sample_page_faults(|| measure_repeated(config.repeat, || dataset.write(config.passes)))?;
    print_bandwidth(
        "write",
        1,
        &write,
        write.elapsed,
        single_direction,
        write_faults,
    )?;
    dataset.verify_write(config.passes)?;

    let (copy, copy_faults) =
        sample_page_faults(|| measure_repeated(config.repeat, || dataset.copy(config.passes)))?;
    print_bandwidth("copy", 1, &copy, copy.elapsed, copy_traffic, copy_faults)?;
    dataset.verify_copy()?;

    for workers in worker_counts {
        let (threaded_read, threaded_read_faults) = sample_page_faults(|| {
            measure_repeated(config.repeat, || {
                dataset.threaded_read(config.passes, workers)
            })
        })?;
        if threaded_read.checksum != read.checksum {
            return Err(format!(
                "threaded read checksum mismatch with {workers} workers"
            ));
        }
        print_bandwidth(
            "read-threads",
            workers,
            &threaded_read,
            read.elapsed,
            single_direction,
            threaded_read_faults,
        )?;

        let (threaded_write, threaded_write_faults) = sample_page_faults(|| {
            measure_repeated(config.repeat, || {
                dataset.threaded_write(config.passes, workers)
            })
        })?;
        dataset.verify_write(config.passes)?;
        print_bandwidth(
            "write-threads",
            workers,
            &threaded_write,
            write.elapsed,
            single_direction,
            threaded_write_faults,
        )?;

        let (threaded_copy, threaded_copy_faults) = sample_page_faults(|| {
            measure_repeated(config.repeat, || {
                dataset.threaded_copy(config.passes, workers)
            })
        })?;
        dataset.verify_copy()?;
        print_bandwidth(
            "copy-threads",
            workers,
            &threaded_copy,
            copy.elapsed,
            copy_traffic,
            threaded_copy_faults,
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct TrafficAccounting {
    bytes_per_pass: usize,
    passes: usize,
    factor: usize,
}

impl TrafficAccounting {
    const fn new(bytes_per_pass: usize, passes: usize, factor: usize) -> Self {
        Self {
            bytes_per_pass,
            passes,
            factor,
        }
    }
}

fn print_bandwidth(
    mode: &str,
    workers: usize,
    measurement: &Measurement,
    baseline: Duration,
    traffic: TrafficAccounting,
    page_faults: PageFaultCounters,
) -> Result<(), String> {
    let payload_bytes = traffic
        .bytes_per_pass
        .checked_mul(traffic.passes)
        .ok_or_else(|| "memory payload byte count overflows usize".to_owned())?;
    let traffic_bytes = payload_bytes
        .checked_mul(traffic.factor)
        .ok_or_else(|| "memory traffic byte count overflows usize".to_owned())?;
    println!(
        "mode={} workers={} elapsed_ms={:.3} min_ms={:.3} max_ms={:.3} speedup={:.3} payload_gib_s={:.3} traffic_gib_s={:.3} payload_bytes={} traffic_bytes={} faults_total={} faults_soft={} faults_hard={} checksum={:#018x}",
        mode,
        workers,
        millis(measurement.elapsed),
        millis(measurement.min_elapsed),
        millis(measurement.max_elapsed),
        baseline.as_secs_f64() / measurement.elapsed.as_secs_f64(),
        gib_per_second(payload_bytes, measurement.elapsed),
        gib_per_second(traffic_bytes, measurement.elapsed),
        payload_bytes,
        traffic_bytes,
        page_faults.total,
        optional_faults(page_faults.soft),
        optional_faults(page_faults.hard),
        measurement.checksum,
    );
    Ok(())
}

fn sample_page_faults<T>(action: impl FnOnce() -> T) -> Result<(T, PageFaultCounters), String> {
    let before = process_metrics::metrics(std::process::id()).map_err(|error| error.to_string())?;
    let result = action();
    let after = process_metrics::metrics(std::process::id()).map_err(|error| error.to_string())?;
    let delta = after
        .page_faults
        .checked_delta_since(before.page_faults)
        .ok_or_else(|| "process page-fault counter wrapped or changed classification".to_owned())?;
    Ok((result, delta))
}

fn optional_faults(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
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

struct Dataset {
    mapping: SharedMemory,
    bytes: usize,
}

impl Dataset {
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
        read_values(self.source(), passes)
    }

    fn write(&mut self, passes: usize) -> u64 {
        write_bytes(self.destination_mut(), passes)
    }

    fn copy(&mut self, passes: usize) -> u64 {
        let split = self.bytes;
        let (source, destination) = self.mapping_mut().split_at_mut(split);
        copy_bytes(source, destination, passes)
    }

    fn threaded_read(&self, passes: usize, workers: usize) -> u64 {
        let source = self.source();
        assert!(workers > 0 && workers <= source.len());
        std::thread::scope(|scope| {
            let handles = (0..workers)
                .map(|worker| {
                    let (start, end) = partition(source.len(), worker, workers);
                    scope.spawn(move || read_values(&source[start..end], passes))
                })
                .collect::<Vec<_>>();
            handles.into_iter().fold(0_u64, |sum, handle| {
                sum.wrapping_add(handle.join().expect("memory read worker panicked"))
            })
        })
    }

    fn threaded_write(&mut self, passes: usize, workers: usize) -> u64 {
        let bytes = self.bytes;
        let destination = self.destination_mut();
        assert!(workers > 0 && workers <= destination.len());
        std::thread::scope(|scope| {
            let mut remaining = &mut *destination;
            let mut handles = Vec::with_capacity(workers);
            for worker in 0..workers {
                let (start, end) = partition(bytes, worker, workers);
                let (chunk, rest) = remaining.split_at_mut(end - start);
                remaining = rest;
                handles.push(scope.spawn(move || write_kernel(chunk, passes)));
            }
            for handle in handles {
                handle.join().expect("memory write worker panicked");
            }
        });
        memory_sample(destination)
    }

    fn threaded_copy(&mut self, passes: usize, workers: usize) -> u64 {
        let bytes = self.bytes;
        let (source, destination) = self.mapping_mut().split_at_mut(bytes);
        assert!(workers > 0 && workers <= bytes);
        std::thread::scope(|scope| {
            let mut remaining = &mut *destination;
            let mut handles = Vec::with_capacity(workers);
            for worker in 0..workers {
                let (start, end) = partition(bytes, worker, workers);
                let (chunk, rest) = remaining.split_at_mut(end - start);
                remaining = rest;
                let source = &source[start..end];
                handles.push(scope.spawn(move || copy_kernel(source, chunk, passes)));
            }
            for handle in handles {
                handle.join().expect("memory copy worker panicked");
            }
        });
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

fn read_values(source: &[u64], passes: usize) -> u64 {
    let source = std::hint::black_box(source);
    let mut checksum = 0_u64;
    for _ in 0..passes {
        checksum = source.iter().copied().fold(checksum, u64::wrapping_add);
    }
    std::hint::black_box(checksum)
}

fn write_bytes(destination: &mut [u8], passes: usize) -> u64 {
    write_kernel(destination, passes);
    memory_sample(destination)
}

fn write_kernel(destination: &mut [u8], passes: usize) {
    for pass in 0..passes {
        let byte = 0x31_u8.wrapping_add(pass as u8);
        // The black-box barrier makes each complete write pass observable.
        std::hint::black_box(&mut *destination).fill(byte);
    }
}

fn copy_bytes(source: &[u8], destination: &mut [u8], passes: usize) -> u64 {
    copy_kernel(source, destination, passes);
    memory_sample(destination)
}

fn copy_kernel(source: &[u8], destination: &mut [u8], passes: usize) {
    for _ in 0..passes {
        destination.copy_from_slice(source);
        std::hint::black_box(&mut *destination);
    }
}

fn memory_sample(bytes: &[u8]) -> u64 {
    let stride = (bytes.len() / 64).max(1);
    bytes
        .iter()
        .step_by(stride)
        .fold(0_u64, |sum, value| sum.wrapping_add(u64::from(*value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_parsed_with_checked_mib_conversion() {
        let args = [
            "--mib".to_owned(),
            "2".to_owned(),
            "--passes".to_owned(),
            "4".to_owned(),
            "--repeat".to_owned(),
            "5".to_owned(),
        ];
        assert_eq!(
            parse_args(&args).unwrap(),
            Config {
                bytes: 2 * 1024 * 1024,
                passes: 4,
                repeat: 5,
            }
        );
        assert!(parse_args(&["--mib".to_owned(), "0".to_owned()]).is_err());
        assert!(parse_args(&["--mib".to_owned(), usize::MAX.to_string()]).is_err());
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
    fn kernels_touch_read_write_and_copy_the_shared_mapping() {
        let mut dataset = Dataset::new(4096).unwrap();
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
    fn threaded_kernels_partition_and_verify_the_entire_mapping() {
        let mut dataset = Dataset::new(8192).unwrap();
        dataset.initialize_source();
        let expected = dataset.read(3);
        let expected_write = dataset.write(3);
        let expected_copy = dataset.copy(3);
        for workers in [1, 2, 4, 8] {
            assert_eq!(dataset.threaded_read(3, workers), expected);
            assert_eq!(dataset.threaded_write(3, workers), expected_write);
            dataset.verify_write(3).unwrap();
            assert_eq!(dataset.threaded_copy(3, workers), expected_copy);
            dataset.verify_copy().unwrap();
        }
    }

    #[test]
    fn first_touch_reports_process_page_fault_delta() {
        let mut dataset = Dataset::new(4 * 1024 * 1024).unwrap();
        let (_, faults) = sample_page_faults(|| dataset.touch_pages(4096, 1)).unwrap();
        assert!(faults.total > 0);
        #[cfg(windows)]
        assert_eq!((faults.soft, faults.hard), (None, None));
    }
}
