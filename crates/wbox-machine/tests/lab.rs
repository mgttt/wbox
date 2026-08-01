use std::process::Command;

fn lab() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wbox-machine-lab"))
}

#[test]
fn runtime_contract_check_passes() {
    let output = lab().arg("check").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("contract=ok routes=18"));
}

#[test]
fn host_probe_reports_a_real_acceleration_state() {
    let output = lab().arg("host").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("host="));
    assert!(stdout.contains("native_isa="));
    assert!(stdout.contains("page_size="));
    assert!(stdout.contains("allocation_granularity="));
    assert!(stdout.contains("physical_memory_bytes="));
    assert!(stdout.contains("acceleration="));
    assert!(!stdout.contains("/unprobed"));
}

#[test]
fn devices_prefills_esp32_without_claiming_availability() {
    let output = lab().arg("devices").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("esp32/xtensa32/bare-metal"));
    assert!(stdout.contains("esp32/riscv32/freertos"));
    assert!(stdout.contains("device_routes=4 available=0"));
}

#[test]
fn accelerators_prefills_gpu_npu_lpu_on_every_host() {
    let output = lab().arg("accelerators").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("windows/gpu workload=parallel-compute"));
    assert!(stdout.contains("linux/npu workload=tensor-compute"));
    assert!(stdout.contains("macos/lpu workload=language-compute"));
    assert!(stdout.contains("accelerator_routes=9 available=0"));
}

#[test]
fn topology_prints_and_validates_point_line_plane_fabric() {
    let output = lab().arg("topology").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("point/cpu kind=cpu"));
    assert!(stdout.contains("line/cpu-esp32"));
    assert!(stdout.contains("plane/local-compute"));
    assert!(stdout.contains("fabric/wbox-fabric"));
    assert!(stdout.contains("validation=ok"));
}

#[test]
fn parallel_prefills_execution_and_data_path_combinations() {
    let output = lab().arg("parallel").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("simd-threads/borrowed-shared copies=0 status=declared"));
    assert!(stdout.contains("simd-processes/shared-mapping copies=0 status=planned"));
    assert!(stdout.contains("parallel_routes=30 declared=5"));
}

#[test]
fn wasm_prefills_browser_and_wasi_machine_capabilities() {
    let output = lab().arg("wasm").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("browser/cpu-interpreter"));
    assert!(stdout.contains("browser/hot-region-translation"));
    assert!(stdout.contains("wasi/device-bus"));
    assert!(stdout.contains("wasm_routes=16 available=0"));
}

#[test]
fn inspect_maps_a_pe_header_to_the_current_route() {
    let mut bytes = vec![0; 0x80];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&0x40_u32.to_le_bytes());
    bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
    bytes[0x44..0x46].copy_from_slice(&0x8664_u16.to_le_bytes());
    bytes[0x58..0x5a].copy_from_slice(&0x20b_u16.to_le_bytes());
    let path = std::env::temp_dir().join(format!(
        "wbox-machine-lab-{}-{}.exe",
        std::process::id(),
        std::thread::current().name().unwrap_or("inspect")
    ));
    std::fs::write(&path, bytes).unwrap();
    let output = lab().arg("inspect").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(path);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("format=pe32+"));
    assert!(stdout.contains("guest=windows"));
    assert!(stdout.contains("isa=x86-64"));
    assert!(stdout.contains("route_status="));
}

#[test]
fn invalid_command_fails_with_usage() {
    let output = lab().arg("unknown").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected host"));
}
