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
    println!("wbox platform contract {}", platform::CONTRACT_REVISION);
    println!(
        "current host: {}/{}",
        current_host_name(),
        current_isa_name()
    );
    println!("HOST     GUEST    ISA      PRIORITY  STATUS     EXECUTION                 ISOLATION");
    for host in HostOs::ALL {
        for guest in GuestOs::ALL {
            for isa in Isa::ALL {
                let item = platform::route(host, guest, isa);
                println!(
                    "{:<8} {:<8} {:<8} {:<9} {:<10} {:<25} {}",
                    host.as_str(),
                    guest.as_str(),
                    isa.as_str(),
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
            object.insert("host".to_owned(), string(item.host.as_str()));
            object.insert("isa".to_owned(), string(item.isa.as_str()));
            object.insert("isolation".to_owned(), string(item.isolation.as_str()));
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
