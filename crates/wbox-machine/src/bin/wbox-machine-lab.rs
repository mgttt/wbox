use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use wbox_machine::{
    accelerator_routes, current_host, detect_hardware, detect_host_memory, esp32_routes,
    inspect_artifact, parallel_routes, prefilled_topology, route, wasm_machine_routes,
    Availability, GuestOs, HostOs, Isa, ParallelRouteStatus, Priority,
};

const HEADER_READ_LIMIT: u64 = 1024 * 1024;

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1).collect()) {
        eprintln!("wbox-machine-lab: {error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<OsString>) -> Result<(), String> {
    match args.as_slice() {
        [command] if command == "host" => print_host(),
        [command] if command == "matrix" => print_matrix(),
        [command] if command == "devices" => print_devices(),
        [command] if command == "accelerators" => print_accelerators(),
        [command] if command == "topology" => print_topology(),
        [command] if command == "parallel" => print_parallel(),
        [command] if command == "wasm" => print_wasm(),
        [command, path] if command == "inspect" => inspect_path(Path::new(path)),
        [command] if command == "check" => check_contract(),
        [command] if command == "help" || command == "-h" || command == "--help" => {
            print_help();
            Ok(())
        }
        _ => {
            print_help();
            Err(
                "expected host, matrix, devices, accelerators, topology, parallel, wasm, inspect <file>, or check"
                    .to_owned(),
            )
        }
    }
}

fn print_help() {
    println!("wbox-machine-lab host");
    println!("wbox-machine-lab matrix");
    println!("wbox-machine-lab devices");
    println!("wbox-machine-lab accelerators");
    println!("wbox-machine-lab topology");
    println!("wbox-machine-lab parallel");
    println!("wbox-machine-lab wasm");
    println!("wbox-machine-lab inspect <executable>");
    println!("wbox-machine-lab check");
}

fn print_parallel() -> Result<(), String> {
    let routes = parallel_routes();
    let declared = routes
        .iter()
        .filter(|route| route.status == ParallelRouteStatus::Declared)
        .count();
    for item in &routes {
        println!(
            "{}/{} copies={} status={} todo={}",
            item.execution.as_str(),
            item.data_path.as_str(),
            item.data_path.logical_data_copies(),
            item.status.as_str(),
            item.todo.unwrap_or("none"),
        );
    }
    println!(
        "summary parallel_routes={} declared={declared}",
        routes.len()
    );
    Ok(())
}

fn print_wasm() -> Result<(), String> {
    let routes = wasm_machine_routes();
    for item in &routes {
        println!(
            "{}/{} status={} todo={}",
            item.surface.as_str(),
            item.capability.as_str(),
            item.status.as_str(),
            item.todo,
        );
    }
    println!("summary wasm_routes={} available=0", routes.len());
    Ok(())
}

fn print_topology() -> Result<(), String> {
    let topology = prefilled_topology();
    topology.validate().map_err(|error| error.to_string())?;
    for node in &topology.nodes {
        println!(
            "point/{} kind={} state={} todo={}",
            node.id,
            node.kind.as_str(),
            node.state.as_str(),
            node.todo.as_deref().unwrap_or("none"),
        );
    }
    for link in &topology.links {
        println!(
            "line/{} {}->{} transport={} direction={} state={} todo={}",
            link.id,
            link.from,
            link.to,
            link.transport.as_str(),
            link.direction.as_str(),
            link.state.as_str(),
            link.todo.as_deref().unwrap_or("none"),
        );
    }
    for domain in &topology.domains {
        println!(
            "plane/{} members={} distribution={} state={} todo={}",
            domain.id,
            domain.members.join(","),
            domain.distribution.as_str(),
            domain.state.as_str(),
            domain.todo.as_deref().unwrap_or("none"),
        );
    }
    for fabric in &topology.fabrics {
        println!(
            "fabric/{} domains={} coordination={} state={} todo={}",
            fabric.id,
            fabric.domains.join(","),
            fabric.coordination.as_str(),
            fabric.state.as_str(),
            fabric.todo.as_deref().unwrap_or("none"),
        );
    }
    println!(
        "summary points={} lines={} planes={} fabrics={} validation=ok",
        topology.nodes.len(),
        topology.links.len(),
        topology.domains.len(),
        topology.fabrics.len(),
    );
    Ok(())
}

fn print_accelerators() -> Result<(), String> {
    let routes = accelerator_routes();
    for item in &routes {
        println!(
            "{}/{} workload={} status={} todo={}",
            item.host.as_str(),
            item.class.as_str(),
            item.workload.as_str(),
            item.status.as_str(),
            item.todo,
        );
    }
    println!("summary accelerator_routes={} available=0", routes.len());
    Ok(())
}

fn print_devices() -> Result<(), String> {
    let routes = esp32_routes();
    for item in routes {
        println!(
            "{}/{}/{} status={} todo={}",
            item.family.as_str(),
            item.isa.as_str(),
            item.environment.as_str(),
            item.status.as_str(),
            item.todo,
        );
    }
    println!("summary device_routes=4 available=0");
    Ok(())
}

fn print_host() -> Result<(), String> {
    let host = current_host();
    let hardware = detect_hardware(host);
    let memory = detect_host_memory().map_err(|error| error.to_string())?;
    println!("host={}", host.map_or("unknown", HostOs::as_str));
    println!(
        "native_isa={}",
        hardware.native_isa.map_or("unknown", Isa::as_str)
    );
    println!(
        "logical_processors={}",
        hardware
            .logical_processors
            .map_or_else(|| "unknown".to_owned(), |count| count.to_string())
    );
    println!(
        "cpu_features={}",
        hardware
            .cpu_features
            .iter()
            .map(|feature| feature.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    println!(
        "acceleration={}/{}",
        hardware.acceleration_api.map_or("none", |api| api.as_str()),
        hardware.acceleration_state.as_str()
    );
    println!("page_size={}", memory.page_size);
    println!("allocation_granularity={}", memory.allocation_granularity);
    println!("physical_memory_bytes={}", memory.physical_bytes);
    Ok(())
}

fn print_matrix() -> Result<(), String> {
    let mut available = 0;
    let mut legacy = 0;
    let mut planned = 0;
    let mut research = 0;
    for host in HostOs::ALL {
        for guest in GuestOs::ALL {
            for isa in Isa::ALL {
                let item = route(host, guest, isa);
                match item.availability {
                    Availability::Available => available += 1,
                    Availability::Legacy => legacy += 1,
                    Availability::Planned => planned += 1,
                    Availability::Research => research += 1,
                }
                println!(
                    "{}/{}/{} abi={} format={} priority={} status={} provider={} isolation={}",
                    host.as_str(),
                    guest.as_str(),
                    isa.as_str(),
                    item.guest_contract.abi.as_str(),
                    item.guest_contract.binary_format.as_str(),
                    item.priority.as_str(),
                    item.availability.as_str(),
                    item.provider.as_str(),
                    item.isolation.as_str(),
                );
            }
        }
    }
    println!(
        "summary total=18 available={available} legacy={legacy} planned={planned} research={research}"
    );
    Ok(())
}

fn inspect_path(path: &Path) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(HEADER_READ_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let identity = inspect_artifact(&bytes).map_err(|error| error.to_string())?;
    println!("path={}", path.display());
    println!("format={}", identity.format.as_str());
    println!("guest={}", identity.guest_os.as_str());
    println!("guest_abi={}", identity.guest_abi.as_str());
    println!("isa={}", identity.isa.as_str());
    if let Some(host) = current_host() {
        let item = route(host, identity.guest_os, identity.isa);
        println!("current_host={}", host.as_str());
        println!("route_status={}", item.availability.as_str());
        println!("provider={}", item.provider.as_str());
        println!("isolation={}", item.isolation.as_str());
        if item.availability != Availability::Available {
            println!("reason={}", item.reason);
        }
    } else {
        println!("current_host=unknown");
        println!("route_status=unsupported-host");
    }
    Ok(())
}

fn check_contract() -> Result<(), String> {
    let mut tuples = Vec::new();
    let mut core = 0;
    for host in HostOs::ALL {
        for guest in GuestOs::ALL {
            for isa in Isa::ALL {
                let item = route(host, guest, isa);
                if item.guest_contract.os != guest || item.guest_contract.isa != isa {
                    return Err(format!(
                        "guest contract mismatch for {}/{}/{}",
                        host.as_str(),
                        guest.as_str(),
                        isa.as_str()
                    ));
                }
                if item.priority == Priority::Core {
                    core += 1;
                }
                tuples.push((host.as_str(), guest.as_str(), isa.as_str()));
            }
        }
    }
    tuples.sort_unstable();
    tuples.dedup();
    if tuples.len() != 18 {
        return Err(format!("expected 18 unique routes, found {}", tuples.len()));
    }
    if core != 12 {
        return Err(format!("expected 12 core routes, found {core}"));
    }
    println!("contract=ok routes=18 core=12 deferred=6");
    Ok(())
}
