use crate::route::HostOs;

pub use agenterm_platform::cache_hierarchy::{
    CacheGeometryFacts, CacheHierarchyError, CacheHierarchyErrorKind, CacheHierarchyFacts,
    CacheKind,
};
pub use agenterm_platform::processor_topology::{
    ProcessorTopologyError, ProcessorTopologyErrorKind, ProcessorTopologyFacts,
};

/// Processor ISA taxonomy across application processors and device cores.
///
/// `Isa` below remains the two-ISA desktop guest matrix contract. Keeping the
/// sets distinct prevents ESP32 device targets from being mistaken for an OS
/// guest route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessorIsa {
    X86_64,
    Aarch64,
    X86_32,
    Arm32,
    RiscV32,
    Xtensa32,
}

impl ProcessorIsa {
    pub const ALL: [Self; 6] = [
        Self::X86_64,
        Self::Aarch64,
        Self::X86_32,
        Self::Arm32,
        Self::RiscV32,
        Self::Xtensa32,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86-64",
            Self::Aarch64 => "aarch64",
            Self::X86_32 => "x86-32",
            Self::Arm32 => "arm32",
            Self::RiscV32 => "riscv32",
            Self::Xtensa32 => "xtensa32",
        }
    }

    pub const fn pointer_width(self) -> u8 {
        match self {
            Self::X86_64 | Self::Aarch64 => 64,
            Self::X86_32 | Self::Arm32 | Self::RiscV32 | Self::Xtensa32 => 32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Isa {
    X86_64,
    Aarch64,
}

impl Isa {
    pub const ALL: [Self; 2] = [Self::X86_64, Self::Aarch64];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86-64",
            Self::Aarch64 => "aarch64",
        }
    }

    pub const fn pointer_width(self) -> u8 {
        64
    }

    pub const fn processor_isa(self) -> ProcessorIsa {
        match self {
            Self::X86_64 => ProcessorIsa::X86_64,
            Self::Aarch64 => ProcessorIsa::Aarch64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CpuFeature {
    X86Sse2,
    X86Avx,
    X86Avx2,
    X86Fma,
    ArmNeon,
}

impl CpuFeature {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86Sse2 => "x86.sse2",
            Self::X86Avx => "x86.avx",
            Self::X86Avx2 => "x86.avx2",
            Self::X86Fma => "x86.fma",
            Self::ArmNeon => "arm.neon",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccelerationApi {
    Whpx,
    Kvm,
    Hvf,
}

impl AccelerationApi {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Whpx => "whpx",
            Self::Kvm => "kvm",
            Self::Hvf => "hvf",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeState {
    Unprobed,
    Available,
    Unavailable,
    AccessDenied,
    Incompatible,
    Failed,
}

impl ProbeState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unprobed => "unprobed",
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::AccessDenied => "access-denied",
            Self::Incompatible => "incompatible",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccelerationCapabilities {
    api: AccelerationApi,
    state: ProbeState,
    api_version: Option<u32>,
    native_code: Option<i64>,
}

impl AccelerationCapabilities {
    const fn unprobed(api: AccelerationApi) -> Self {
        Self {
            api,
            state: ProbeState::Unprobed,
            api_version: None,
            native_code: None,
        }
    }

    fn from_native_facts(
        facts: agenterm_platform::contract::native_virtualization::NativeVirtualizationFacts,
    ) -> Option<Self> {
        use agenterm_platform::contract::native_virtualization::{
            NativeVirtualizationBackend, VirtualizationProbeState,
        };

        let api = match facts.backend() {
            NativeVirtualizationBackend::WindowsHypervisorPlatform => AccelerationApi::Whpx,
            NativeVirtualizationBackend::Kvm => AccelerationApi::Kvm,
            NativeVirtualizationBackend::HypervisorFramework => AccelerationApi::Hvf,
            _ => return None,
        };
        let state = match facts.state() {
            VirtualizationProbeState::Available => ProbeState::Available,
            VirtualizationProbeState::Unavailable => ProbeState::Unavailable,
            VirtualizationProbeState::AccessDenied => ProbeState::AccessDenied,
            VirtualizationProbeState::Incompatible => ProbeState::Incompatible,
            VirtualizationProbeState::Failed => ProbeState::Failed,
            _ => return None,
        };
        Some(Self {
            api,
            state,
            api_version: facts.api_version(),
            native_code: facts.native_code(),
        })
    }

    pub const fn api(self) -> AccelerationApi {
        self.api
    }

    pub const fn state(self) -> ProbeState {
        self.state
    }

    pub const fn api_version(self) -> Option<u32> {
        self.api_version
    }

    pub const fn native_code(self) -> Option<i64> {
        self.native_code
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareCapabilities {
    pub native_isa: Option<Isa>,
    /// Parallelism available to this process after affinity and scheduler limits.
    pub logical_processors: Option<usize>,
    /// System-wide topology for the real current host. Hypothetical hosts are unprobed.
    pub processor_topology: Option<Result<ProcessorTopologyFacts, ProcessorTopologyError>>,
    /// System-wide CPU cache hierarchy for the real current host.
    pub cache_hierarchy: Option<Result<CacheHierarchyFacts, CacheHierarchyError>>,
    pub cpu_features: Vec<CpuFeature>,
    pub acceleration: Option<AccelerationCapabilities>,
}

impl HardwareCapabilities {
    pub fn supports_cpu_feature(&self, feature: CpuFeature) -> bool {
        self.cpu_features.contains(&feature)
    }

    pub fn apply_native_virtualization(
        &mut self,
        facts: agenterm_platform::contract::native_virtualization::NativeVirtualizationFacts,
    ) -> Result<(), agenterm_platform::contract::native_virtualization::NativeVirtualizationFacts>
    {
        let Some(mapped) = AccelerationCapabilities::from_native_facts(facts) else {
            return Err(facts);
        };
        if self.acceleration.map(AccelerationCapabilities::api) != Some(mapped.api) {
            return Err(facts);
        }
        self.acceleration = Some(mapped);
        Ok(())
    }
}

pub const fn current_isa() -> Option<Isa> {
    use agenterm_platform::hardware::ProcessorArchitecture;
    match agenterm_platform::hardware::current_architecture() {
        ProcessorArchitecture::X86_64 => Some(Isa::X86_64),
        ProcessorArchitecture::Aarch64 => Some(Isa::Aarch64),
        _ => None,
    }
}

pub fn detect_hardware(host: Option<HostOs>) -> HardwareCapabilities {
    let processor = agenterm_platform::hardware::processor_facts();
    let mut capabilities = HardwareCapabilities {
        native_isa: match processor.architecture {
            agenterm_platform::hardware::ProcessorArchitecture::X86_64 => Some(Isa::X86_64),
            agenterm_platform::hardware::ProcessorArchitecture::Aarch64 => Some(Isa::Aarch64),
            _ => None,
        },
        logical_processors: processor
            .logical_processors
            .map(std::num::NonZeroUsize::get),
        processor_topology: (host.is_some() && host == crate::route::current_host())
            .then(agenterm_platform::processor_topology::facts),
        cache_hierarchy: (host.is_some() && host == crate::route::current_host())
            .then(agenterm_platform::cache_hierarchy::facts),
        cpu_features: processor
            .features
            .into_iter()
            .filter_map(map_cpu_feature)
            .collect(),
        // Selecting the host API is not a permission/device/firmware probe.
        acceleration: host
            .map(host_acceleration_api)
            .map(AccelerationCapabilities::unprobed),
    };
    let probe_failed = host.is_some()
        && host == crate::route::current_host()
        && capabilities
            .apply_native_virtualization(agenterm_platform::native_virtualization::probe())
            .is_err();
    if probe_failed {
        capabilities.acceleration = capabilities.acceleration.map(|mut acceleration| {
            acceleration.state = ProbeState::Failed;
            acceleration
        });
    }
    capabilities
}

fn map_cpu_feature(feature: agenterm_platform::hardware::ProcessorFeature) -> Option<CpuFeature> {
    use agenterm_platform::hardware::ProcessorFeature;
    match feature {
        ProcessorFeature::X86Sse2 => Some(CpuFeature::X86Sse2),
        ProcessorFeature::X86Avx => Some(CpuFeature::X86Avx),
        ProcessorFeature::X86Avx2 => Some(CpuFeature::X86Avx2),
        ProcessorFeature::X86Fma => Some(CpuFeature::X86Fma),
        ProcessorFeature::ArmNeon => Some(CpuFeature::ArmNeon),
        _ => None,
    }
}

const fn host_acceleration_api(host: HostOs) -> AccelerationApi {
    match host {
        HostOs::Windows => AccelerationApi::Whpx,
        HostOs::Linux => AccelerationApi::Kvm,
        HostOs::Macos => AccelerationApi::Hvf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isa_contract_is_explicitly_64_bit() {
        for isa in Isa::ALL {
            assert_eq!(isa.pointer_width(), 64);
        }
    }

    #[test]
    fn processor_taxonomy_prefills_32_and_64_bit_domains() {
        assert_eq!(ProcessorIsa::ALL.len(), 6);
        let width32 = ProcessorIsa::ALL
            .into_iter()
            .filter(|isa| isa.pointer_width() == 32)
            .count();
        assert_eq!(width32, 4);
        for isa in Isa::ALL {
            assert_eq!(isa.processor_isa().pointer_width(), 64);
        }
    }

    #[test]
    fn hardware_detection_probes_only_the_current_host() {
        for host in HostOs::ALL {
            let hardware = detect_hardware(Some(host));
            let acceleration = hardware.acceleration.expect("host acceleration candidate");
            assert_eq!(acceleration.api(), host_acceleration_api(host));
            if Some(host) == crate::route::current_host() {
                assert_ne!(acceleration.state(), ProbeState::Unprobed);
                assert!(hardware.processor_topology.is_some());
                assert!(hardware.cache_hierarchy.is_some());
            } else {
                assert_eq!(acceleration.state(), ProbeState::Unprobed);
                assert!(hardware.processor_topology.is_none());
                assert!(hardware.cache_hierarchy.is_none());
            }
            assert_eq!(acceleration.api_version(), None);
        }
    }

    #[test]
    fn native_virtualization_facts_preserve_evidence_and_reject_wrong_host() {
        use agenterm_platform::contract::native_virtualization::{
            NativeVirtualizationBackend, NativeVirtualizationFacts,
        };

        let mut linux = detect_hardware(Some(HostOs::Linux));
        linux
            .apply_native_virtualization(NativeVirtualizationFacts::available(
                NativeVirtualizationBackend::Kvm,
                Some(12),
            ))
            .unwrap();
        assert_eq!(
            linux.acceleration,
            Some(AccelerationCapabilities {
                api: AccelerationApi::Kvm,
                state: ProbeState::Available,
                api_version: Some(12),
                native_code: None,
            })
        );

        let whpx_denied = NativeVirtualizationFacts::access_denied(
            NativeVirtualizationBackend::WindowsHypervisorPlatform,
            5,
        );
        assert_eq!(
            linux.apply_native_virtualization(whpx_denied),
            Err(whpx_denied)
        );
        assert_eq!(linux.acceleration.unwrap().api(), AccelerationApi::Kvm);

        let mut unknown = detect_hardware(None);
        let kvm = NativeVirtualizationFacts::unavailable(NativeVirtualizationBackend::Kvm);
        assert_eq!(unknown.apply_native_virtualization(kvm), Err(kvm));
        assert_eq!(unknown.acceleration, None);
    }

    #[test]
    fn current_target_isa_matches_rust_cfg() {
        #[cfg(target_arch = "x86_64")]
        assert_eq!(current_isa(), Some(Isa::X86_64));
        #[cfg(target_arch = "aarch64")]
        assert_eq!(current_isa(), Some(Isa::Aarch64));
    }

    #[test]
    fn hardware_facts_come_from_the_lightweight_platform_feature() {
        assert_eq!(
            agenterm_platform::capability_status(agenterm_platform::Capability::Hardware),
            agenterm_platform::CapabilityStatus::Available
        );
        let platform = agenterm_platform::hardware::processor_facts();
        let mapped = detect_hardware(None);
        assert!(mapped.processor_topology.is_none());
        assert!(mapped.cache_hierarchy.is_none());
        assert_eq!(
            mapped.logical_processors,
            platform.logical_processors.map(std::num::NonZeroUsize::get)
        );
        for feature in platform.features {
            if let Some(feature) = map_cpu_feature(feature) {
                assert!(mapped.supports_cpu_feature(feature));
            }
        }
    }

    #[test]
    fn current_host_preserves_system_topology_evidence() {
        let hardware = detect_hardware(crate::route::current_host());
        let topology = hardware
            .processor_topology
            .expect("current host topology was probed")
            .expect("current host topology query succeeded");
        let system_logical = topology.system_logical_processors.get();
        assert!(hardware
            .logical_processors
            .is_none_or(|count| count <= system_logical));
        if let Some(physical) = topology.physical_cores {
            assert!(physical.get() <= system_logical);
            if let Some(threads) = topology.uniform_threads_per_core() {
                assert_eq!(physical.get() * threads.get(), system_logical);
            }
        }
    }

    #[test]
    fn current_host_preserves_cache_hierarchy_evidence() {
        let hardware = detect_hardware(crate::route::current_host());
        let caches = hardware
            .cache_hierarchy
            .expect("current host cache hierarchy was probed")
            .expect("current host cache hierarchy query succeeded");
        assert!(!caches.geometries.is_empty());
        assert!(caches.max_data_line_bytes().is_some());
        for cache in caches.geometries {
            assert!(u64::from(cache.line_bytes.get()) <= cache.size_bytes.get());
        }
    }
}
