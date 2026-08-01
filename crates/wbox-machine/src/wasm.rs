#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WasmHostSurface {
    Browser,
    Wasi,
}

impl WasmHostSurface {
    pub const ALL: [Self; 2] = [Self::Browser, Self::Wasi];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Wasi => "wasi",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WasmMachineCapability {
    CpuInterpreter,
    HotRegionTranslation,
    LinearMemory,
    DeviceBus,
    BlockStorage,
    Networking,
    Snapshot,
    MultiInstance,
}

impl WasmMachineCapability {
    pub const ALL: [Self; 8] = [
        Self::CpuInterpreter,
        Self::HotRegionTranslation,
        Self::LinearMemory,
        Self::DeviceBus,
        Self::BlockStorage,
        Self::Networking,
        Self::Snapshot,
        Self::MultiInstance,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CpuInterpreter => "cpu-interpreter",
            Self::HotRegionTranslation => "hot-region-translation",
            Self::LinearMemory => "linear-memory",
            Self::DeviceBus => "device-bus",
            Self::BlockStorage => "block-storage",
            Self::Networking => "networking",
            Self::Snapshot => "snapshot",
            Self::MultiInstance => "multi-instance",
        }
    }

    const fn todo(self) -> &'static str {
        match self {
            // TODO(WM-WASM-INTERPRETER): freeze the portable CPU step/exception contract.
            Self::CpuInterpreter => "WM-WASM-INTERPRETER",
            // TODO(WM-WASM-JIT): define hotness, invalidation, and x-to-WASM translation.
            Self::HotRegionTranslation => "WM-WASM-JIT",
            // TODO(WM-WASM-MEMORY): define paging over bounded WASM linear memory.
            Self::LinearMemory => "WM-WASM-MEMORY",
            // TODO(WM-WASM-DEVICES): define interrupts, timers, buses, and device lifecycle.
            Self::DeviceBus => "WM-WASM-DEVICES",
            // TODO(WM-WASM-STORAGE): define browser and WASI block persistence adapters.
            Self::BlockStorage => "WM-WASM-STORAGE",
            // TODO(WM-WASM-NET): define capability-safe browser and WASI networking.
            Self::Networking => "WM-WASM-NET",
            // TODO(WM-WASM-SNAPSHOT): version CPU, memory, and device state together.
            Self::Snapshot => "WM-WASM-SNAPSHOT",
            // TODO(WM-WASM-MULTI): define isolation and communication between instances.
            Self::MultiInstance => "WM-WASM-MULTI",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmRouteStatus {
    Research,
}

impl WasmRouteStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Research => "research",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasmMachineRoute {
    pub surface: WasmHostSurface,
    pub capability: WasmMachineCapability,
    pub status: WasmRouteStatus,
    pub todo: &'static str,
}

pub fn wasm_machine_routes() -> Vec<WasmMachineRoute> {
    WasmHostSurface::ALL
        .into_iter()
        .flat_map(|surface| {
            WasmMachineCapability::ALL
                .into_iter()
                .map(move |capability| WasmMachineRoute {
                    surface,
                    capability,
                    status: WasmRouteStatus::Research,
                    todo: capability.todo(),
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_and_wasi_capability_matrix_is_prefilled() {
        let routes = wasm_machine_routes();
        assert_eq!(routes.len(), 16);
        let mut tuples = routes
            .into_iter()
            .map(|route| {
                assert!(!route.todo.is_empty());
                (route.surface.as_str(), route.capability.as_str())
            })
            .collect::<Vec<_>>();
        tuples.sort_unstable();
        tuples.dedup();
        assert_eq!(tuples.len(), 16);
    }
}
