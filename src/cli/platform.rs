use wbox_codec::json::{Map, Number, Value};

use crate::{
    error::{Result, WboxError},
    platform::{self, GuestOs, HostOs, Isa},
};

pub fn cmd_platform(args: &[String]) -> Result<u32> {
    match args {
        [] => print_human(),
        [format] if format == "--json" => print_json(),
        _ => {
            return Err(WboxError::args("platform 用法：wbox platform [--json]"));
        }
    }
    Ok(0)
}

fn current_host_name() -> &'static str {
    platform::current_host().map_or("unknown", HostOs::as_str)
}

fn current_isa_name() -> &'static str {
    platform::current_isa().map_or("unknown", Isa::as_str)
}

fn print_human() {
    let hardware = platform::detect_hardware(platform::current_host());
    println!("wbox platform contract {}", platform::CONTRACT_REVISION);
    println!(
        "current host: {}/{}",
        current_host_name(),
        current_isa_name()
    );
    let features = hardware
        .cpu_features
        .iter()
        .map(|feature| feature.as_str())
        .collect::<Vec<_>>()
        .join(",");
    println!(
        "hardware: process-available-cpus={} features={} acceleration={}/{}",
        hardware
            .logical_processors
            .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
        if features.is_empty() {
            "none"
        } else {
            &features
        },
        hardware
            .acceleration
            .map_or("none", |item| item.api().as_str()),
        hardware
            .acceleration
            .map_or("unprobed", |item| item.state().as_str()),
    );
    match hardware.processor_topology.as_ref() {
        Some(Ok(topology)) => println!(
            "system-topology: logical-cpus={} physical-cores={} packages={} numa-nodes={} processor-groups={} threads-per-core={}",
            topology.system_logical_processors,
            optional_count(topology.physical_cores),
            optional_count(topology.packages),
            optional_count(topology.numa_nodes),
            optional_count(topology.processor_groups),
            optional_count(topology.uniform_threads_per_core()),
        ),
        Some(Err(error)) => println!("system-topology: failed ({error})"),
        None => println!("system-topology: unprobed"),
    }
    match hardware.processor_affinity.as_ref() {
        Some(Ok(affinity)) => println!(
            "process-affinity: semantics={} count={} processors={}",
            affinity.semantics().as_str(),
            affinity.count(),
            affinity
                .processors()
                .iter()
                .map(|processor| format!("{}:{}", processor.group, processor.index))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Some(Err(error)) => println!(
            "process-affinity: {} kind={} detail={}",
            affinity_error_state(error.kind()),
            error.kind().as_str(),
            error.detail()
        ),
        None => println!("process-affinity: unprobed"),
    }
    match hardware.cache_hierarchy.as_ref() {
        Some(Ok(hierarchy)) => {
            println!(
                "cache-hierarchy: geometries={} max-data-line-bytes={}",
                hierarchy.geometries.len(),
                hierarchy
                    .max_data_line_bytes()
                    .map_or_else(|| "unknown".to_owned(), |line| line.to_string())
            );
            for cache in &hierarchy.geometries {
                println!(
                    "  cache: L{} {} size={} line={} instances={} shared-logical-cpus={}",
                    cache.level,
                    cache.kind.as_str(),
                    cache.size_bytes,
                    cache.line_bytes,
                    optional_count(cache.instances),
                    optional_count(cache.shared_logical_processors),
                );
            }
        }
        Some(Err(error)) => println!("cache-hierarchy: failed ({error})"),
        None => println!("cache-hierarchy: unprobed"),
    }
    println!(
        "HOST     GUEST    ISA      ABI            FORMAT    PRIORITY  STATUS     EXECUTION                 ISOLATION"
    );
    for host in HostOs::ALL {
        for guest in GuestOs::ALL {
            for isa in Isa::ALL {
                let item = platform::route(host, guest, isa);
                println!(
                    "{:<8} {:<8} {:<8} {:<14} {:<9} {:<9} {:<10} {:<25} {}",
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
                if item.availability != platform::Availability::Available {
                    println!("  reason: {}", item.reason);
                }
            }
        }
    }
}

fn string(value: &str) -> Value {
    Value::String(value.to_owned())
}

fn optional_count(value: Option<std::num::NonZeroUsize>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |count| count.to_string())
}

fn optional_count_json(value: Option<std::num::NonZeroUsize>) -> Value {
    value.map_or(Value::Null, |count| {
        Value::Number(Number::PosInt(count.get() as u64))
    })
}

fn processor_topology_json(hardware: &platform::HardwareCapabilities) -> Value {
    let mut object = Map::new();
    object.insert("state".to_owned(), string("unprobed"));
    for key in [
        "system_logical_processors",
        "physical_cores",
        "packages",
        "numa_nodes",
        "processor_groups",
        "uniform_threads_per_core",
        "error_kind",
        "error_detail",
    ] {
        object.insert(key.to_owned(), Value::Null);
    }
    match hardware.processor_topology.as_ref() {
        Some(Ok(topology)) => {
            object.insert("state".to_owned(), string("available"));
            object.insert(
                "system_logical_processors".to_owned(),
                Value::Number(Number::PosInt(
                    topology.system_logical_processors.get() as u64
                )),
            );
            object.insert(
                "physical_cores".to_owned(),
                optional_count_json(topology.physical_cores),
            );
            object.insert(
                "packages".to_owned(),
                optional_count_json(topology.packages),
            );
            object.insert(
                "numa_nodes".to_owned(),
                optional_count_json(topology.numa_nodes),
            );
            object.insert(
                "processor_groups".to_owned(),
                optional_count_json(topology.processor_groups),
            );
            object.insert(
                "uniform_threads_per_core".to_owned(),
                optional_count_json(topology.uniform_threads_per_core()),
            );
        }
        Some(Err(error)) => {
            object.insert("state".to_owned(), string("failed"));
            object.insert(
                "error_kind".to_owned(),
                string(match error.kind() {
                    platform::ProcessorTopologyErrorKind::Query => "query",
                    platform::ProcessorTopologyErrorKind::InvalidValue => "invalid-value",
                    platform::ProcessorTopologyErrorKind::MalformedNativeData => {
                        "malformed-native-data"
                    }
                    _ => "unknown",
                }),
            );
            object.insert("error_detail".to_owned(), string(error.detail()));
        }
        None => {}
    }
    Value::Object(object)
}

fn affinity_error_state(kind: platform::ProcessorAffinityErrorKind) -> &'static str {
    if kind == platform::ProcessorAffinityErrorKind::Unsupported {
        "unsupported"
    } else {
        "failed"
    }
}

fn processor_affinity_json(hardware: &platform::HardwareCapabilities) -> Value {
    let mut object = Map::new();
    object.insert("state".to_owned(), string("unprobed"));
    object.insert("semantics".to_owned(), Value::Null);
    object.insert("count".to_owned(), Value::Null);
    object.insert("processors".to_owned(), Value::Array(Vec::new()));
    object.insert("error_kind".to_owned(), Value::Null);
    object.insert("error_detail".to_owned(), Value::Null);
    match hardware.processor_affinity.as_ref() {
        Some(Ok(affinity)) => {
            object.insert("state".to_owned(), string("available"));
            object.insert(
                "semantics".to_owned(),
                string(affinity.semantics().as_str()),
            );
            object.insert(
                "count".to_owned(),
                Value::Number(Number::PosInt(affinity.count().get() as u64)),
            );
            object.insert(
                "processors".to_owned(),
                Value::Array(
                    affinity
                        .processors()
                        .iter()
                        .map(|processor| {
                            let mut item = Map::new();
                            item.insert(
                                "group".to_owned(),
                                Value::Number(Number::PosInt(u64::from(processor.group))),
                            );
                            item.insert(
                                "index".to_owned(),
                                Value::Number(Number::PosInt(u64::from(processor.index))),
                            );
                            Value::Object(item)
                        })
                        .collect(),
                ),
            );
        }
        Some(Err(error)) => {
            object.insert(
                "state".to_owned(),
                string(affinity_error_state(error.kind())),
            );
            object.insert("error_kind".to_owned(), string(error.kind().as_str()));
            object.insert("error_detail".to_owned(), string(error.detail()));
        }
        None => {}
    }
    Value::Object(object)
}

fn cache_hierarchy_json(hardware: &platform::HardwareCapabilities) -> Value {
    let mut object = Map::new();
    object.insert("state".to_owned(), string("unprobed"));
    object.insert("max_data_line_bytes".to_owned(), Value::Null);
    object.insert("geometries".to_owned(), Value::Array(Vec::new()));
    object.insert("error_kind".to_owned(), Value::Null);
    object.insert("error_detail".to_owned(), Value::Null);
    match hardware.cache_hierarchy.as_ref() {
        Some(Ok(hierarchy)) => {
            object.insert("state".to_owned(), string("available"));
            object.insert(
                "max_data_line_bytes".to_owned(),
                hierarchy.max_data_line_bytes().map_or(Value::Null, |line| {
                    Value::Number(Number::PosInt(u64::from(line.get())))
                }),
            );
            let geometries = hierarchy
                .geometries
                .iter()
                .map(|cache| {
                    let mut item = Map::new();
                    item.insert(
                        "level".to_owned(),
                        Value::Number(Number::PosInt(u64::from(cache.level.get()))),
                    );
                    item.insert("kind".to_owned(), string(cache.kind.as_str()));
                    item.insert(
                        "size_bytes".to_owned(),
                        Value::Number(Number::PosInt(cache.size_bytes.get())),
                    );
                    item.insert(
                        "line_bytes".to_owned(),
                        Value::Number(Number::PosInt(u64::from(cache.line_bytes.get()))),
                    );
                    item.insert("instances".to_owned(), optional_count_json(cache.instances));
                    item.insert(
                        "shared_logical_processors".to_owned(),
                        optional_count_json(cache.shared_logical_processors),
                    );
                    Value::Object(item)
                })
                .collect();
            object.insert("geometries".to_owned(), Value::Array(geometries));
        }
        Some(Err(error)) => {
            object.insert("state".to_owned(), string("failed"));
            object.insert("error_kind".to_owned(), string(error.kind().as_str()));
            object.insert("error_detail".to_owned(), string(error.detail()));
        }
        None => {}
    }
    Value::Object(object)
}

fn print_json() {
    let hardware = platform::detect_hardware(platform::current_host());
    let routes = HostOs::ALL
        .into_iter()
        .flat_map(|host| {
            GuestOs::ALL
                .into_iter()
                .flat_map(move |guest| Isa::ALL.map(move |isa| platform::route(host, guest, isa)))
        })
        .map(|item| {
            let mut object = Map::new();
            object.insert(
                "availability".to_owned(),
                string(item.availability.as_str()),
            );
            object.insert("guest".to_owned(), string(item.guest.as_str()));
            object.insert(
                "guest_abi".to_owned(),
                string(item.guest_contract.abi.as_str()),
            );
            object.insert("host".to_owned(), string(item.host.as_str()));
            object.insert("isa".to_owned(), string(item.isa.as_str()));
            object.insert("isolation".to_owned(), string(item.isolation.as_str()));
            object.insert(
                "binary_format".to_owned(),
                string(item.guest_contract.binary_format.as_str()),
            );
            object.insert("priority".to_owned(), string(item.priority.as_str()));
            object.insert("provider".to_owned(), string(item.provider.as_str()));
            object.insert("reason".to_owned(), string(item.reason));
            Value::Object(object)
        })
        .collect();
    let mut root = Map::new();
    root.insert(
        "contract_revision".to_owned(),
        Value::Number(Number::PosInt(u64::from(platform::CONTRACT_REVISION))),
    );
    root.insert("current_host".to_owned(), string(current_host_name()));
    root.insert("current_isa".to_owned(), string(current_isa_name()));
    let mut hardware_json = Map::new();
    hardware_json.insert(
        "acceleration_api".to_owned(),
        string(
            hardware
                .acceleration
                .map_or("none", |item| item.api().as_str()),
        ),
    );
    hardware_json.insert(
        "acceleration_state".to_owned(),
        string(
            hardware
                .acceleration
                .map_or("unprobed", |item| item.state().as_str()),
        ),
    );
    hardware_json.insert(
        "acceleration_api_version".to_owned(),
        hardware
            .acceleration
            .and_then(|item| item.api_version())
            .map_or(Value::Null, |version| {
                Value::Number(Number::PosInt(u64::from(version)))
            }),
    );
    hardware_json.insert(
        "acceleration_native_code".to_owned(),
        hardware
            .acceleration
            .and_then(|item| item.native_code())
            .map_or(Value::Null, |code| {
                if code >= 0 {
                    Value::Number(Number::PosInt(code as u64))
                } else {
                    Value::Number(Number::NegInt(code))
                }
            }),
    );
    hardware_json.insert(
        "cpu_features".to_owned(),
        Value::Array(
            hardware
                .cpu_features
                .iter()
                .map(|feature| string(feature.as_str()))
                .collect(),
        ),
    );
    hardware_json.insert(
        "logical_processors".to_owned(),
        hardware.logical_processors.map_or(Value::Null, |count| {
            Value::Number(Number::PosInt(count as u64))
        }),
    );
    hardware_json.insert(
        "processor_topology".to_owned(),
        processor_topology_json(&hardware),
    );
    hardware_json.insert(
        "processor_affinity".to_owned(),
        processor_affinity_json(&hardware),
    );
    hardware_json.insert(
        "cache_hierarchy".to_owned(),
        cache_hierarchy_json(&hardware),
    );
    root.insert("hardware".to_owned(), Value::Object(hardware_json));
    root.insert("routes".to_owned(), Value::Array(routes));
    println!("{}", Value::Object(root).to_string_pretty());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_arguments_are_strict() {
        assert_eq!(cmd_platform(&[]).unwrap(), 0);
        assert_eq!(cmd_platform(&["--json".to_owned()]).unwrap(), 0);
        assert!(cmd_platform(&["--yaml".to_owned()]).is_err());
        assert!(cmd_platform(&["--json".to_owned(), "extra".to_owned()]).is_err());
    }

    #[test]
    fn topology_json_distinguishes_current_host_from_unprobed_hosts() {
        let current = platform::detect_hardware(platform::current_host());
        let Value::Object(current) = processor_topology_json(&current) else {
            panic!("processor topology must be an object");
        };
        assert_eq!(current.get("state"), Some(&string("available")));
        assert!(current.contains_key("system_logical_processors"));

        let hypothetical = platform::detect_hardware(None);
        let Value::Object(hypothetical) = processor_topology_json(&hypothetical) else {
            panic!("processor topology must be an object");
        };
        assert_eq!(hypothetical.get("state"), Some(&string("unprobed")));
        assert_eq!(
            hypothetical.get("system_logical_processors"),
            Some(&Value::Null)
        );
        assert_eq!(hypothetical.get("error_kind"), Some(&Value::Null));
        assert_eq!(hypothetical.get("error_detail"), Some(&Value::Null));
    }

    #[test]
    fn cache_json_distinguishes_current_host_from_unprobed_hosts() {
        let current = platform::detect_hardware(platform::current_host());
        let Value::Object(current) = cache_hierarchy_json(&current) else {
            panic!("cache hierarchy must be an object");
        };
        assert_eq!(current.get("state"), Some(&string("available")));
        assert_ne!(current.get("max_data_line_bytes"), Some(&Value::Null));
        assert!(matches!(
            current.get("geometries"),
            Some(Value::Array(items)) if !items.is_empty()
        ));

        let hypothetical = platform::detect_hardware(None);
        let Value::Object(hypothetical) = cache_hierarchy_json(&hypothetical) else {
            panic!("cache hierarchy must be an object");
        };
        assert_eq!(hypothetical.get("state"), Some(&string("unprobed")));
        assert_eq!(hypothetical.get("max_data_line_bytes"), Some(&Value::Null));
        assert_eq!(
            hypothetical.get("geometries"),
            Some(&Value::Array(Vec::new()))
        );
    }

    #[test]
    fn affinity_json_distinguishes_current_host_from_unprobed_hosts() {
        let current = platform::detect_hardware(platform::current_host());
        let Value::Object(current) = processor_affinity_json(&current) else {
            panic!("processor affinity must be an object");
        };
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(current.get("state"), Some(&string("available")));
            assert_ne!(current.get("semantics"), Some(&Value::Null));
            assert!(matches!(
                current.get("processors"),
                Some(Value::Array(items)) if !items.is_empty()
            ));
        }
        #[cfg(target_os = "macos")]
        assert_eq!(current.get("state"), Some(&string("unsupported")));

        let hypothetical = platform::detect_hardware(None);
        let Value::Object(hypothetical) = processor_affinity_json(&hypothetical) else {
            panic!("processor affinity must be an object");
        };
        assert_eq!(hypothetical.get("state"), Some(&string("unprobed")));
        assert_eq!(hypothetical.get("semantics"), Some(&Value::Null));
        assert_eq!(
            hypothetical.get("processors"),
            Some(&Value::Array(Vec::new()))
        );
    }
}
