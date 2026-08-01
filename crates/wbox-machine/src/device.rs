use crate::ProcessorIsa;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceFamily {
    Esp32,
}

impl DeviceFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Esp32 => "esp32",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FirmwareEnvironment {
    BareMetal,
    FreeRtos,
}

impl FirmwareEnvironment {
    pub const ALL: [Self; 2] = [Self::BareMetal, Self::FreeRtos];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BareMetal => "bare-metal",
            Self::FreeRtos => "freertos",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRouteStatus {
    Planned,
    Research,
}

impl DeviceRouteStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Research => "research",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceRoute {
    pub family: DeviceFamily,
    pub isa: ProcessorIsa,
    pub environment: FirmwareEnvironment,
    pub status: DeviceRouteStatus,
    pub todo: &'static str,
}

pub const fn esp32_routes() -> [DeviceRoute; 4] {
    use DeviceRouteStatus::{Planned, Research};
    use FirmwareEnvironment::{BareMetal, FreeRtos};
    use ProcessorIsa::{RiscV32, Xtensa32};

    [
        DeviceRoute {
            family: DeviceFamily::Esp32,
            isa: Xtensa32,
            environment: BareMetal,
            status: Research,
            // TODO(WM-ESP32-XTENSA): inspect ESP images and model the Xtensa core.
            todo: "WM-ESP32-XTENSA",
        },
        DeviceRoute {
            family: DeviceFamily::Esp32,
            isa: Xtensa32,
            environment: FreeRtos,
            status: Research,
            // TODO(WM-ESP32-FREERTOS): define task, interrupt, and peripheral ABI contracts.
            todo: "WM-ESP32-FREERTOS",
        },
        DeviceRoute {
            family: DeviceFamily::Esp32,
            isa: RiscV32,
            environment: BareMetal,
            status: Planned,
            // TODO(WM-ESP32-RISCV): inspect RV32 firmware and define the core profile.
            todo: "WM-ESP32-RISCV",
        },
        DeviceRoute {
            family: DeviceFamily::Esp32,
            isa: RiscV32,
            environment: FreeRtos,
            status: Planned,
            // TODO(WM-ESP32-TRANSPORT): probe USB/JTAG/UART through host adapters.
            todo: "WM-ESP32-TRANSPORT",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esp32_matrix_is_prefilled_without_claiming_availability() {
        let routes = esp32_routes();
        assert_eq!(routes.len(), 4);
        let mut tuples = routes
            .into_iter()
            .map(|route| {
                assert_eq!(route.isa.pointer_width(), 32);
                assert!(!route.todo.is_empty());
                (route.isa.as_str(), route.environment.as_str())
            })
            .collect::<Vec<_>>();
        tuples.sort_unstable();
        tuples.dedup();
        assert_eq!(tuples.len(), 4);
    }
}
