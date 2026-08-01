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
        "hardware: logical-cpus={} features={} acceleration={}/{}",
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
}
