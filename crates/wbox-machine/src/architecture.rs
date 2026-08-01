use crate::route::HostOs;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CpuFeature {
    X86Sse2,
    X86Avx,
    X86Avx2,
    ArmNeon,
}

impl CpuFeature {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86Sse2 => "x86.sse2",
            Self::X86Avx => "x86.avx",
            Self::X86Avx2 => "x86.avx2",
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
}

impl ProbeState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unprobed => "unprobed",
            Self::Available => "available",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareCapabilities {
    pub native_isa: Option<Isa>,
    pub logical_processors: Option<usize>,
    pub cpu_features: Vec<CpuFeature>,
    pub acceleration_api: Option<AccelerationApi>,
    pub acceleration_state: ProbeState,
}

impl HardwareCapabilities {
    pub fn supports_cpu_feature(&self, feature: CpuFeature) -> bool {
        self.cpu_features.contains(&feature)
    }
}

pub const fn current_isa() -> Option<Isa> {
    if cfg!(target_arch = "x86_64") {
        Some(Isa::X86_64)
    } else if cfg!(target_arch = "aarch64") {
        Some(Isa::Aarch64)
    } else {
        None
    }
}

pub fn detect_hardware(host: Option<HostOs>) -> HardwareCapabilities {
    HardwareCapabilities {
        native_isa: current_isa(),
        logical_processors: std::thread::available_parallelism()
            .ok()
            .map(std::num::NonZeroUsize::get),
        cpu_features: detected_cpu_features(),
        acceleration_api: host.map(host_acceleration_api),
        // Selecting the host API is not a permission/device/firmware probe.
        acceleration_state: ProbeState::Unprobed,
    }
}

const fn host_acceleration_api(host: HostOs) -> AccelerationApi {
    match host {
        HostOs::Windows => AccelerationApi::Whpx,
        HostOs::Linux => AccelerationApi::Kvm,
        HostOs::Macos => AccelerationApi::Hvf,
    }
}

fn detected_cpu_features() -> Vec<CpuFeature> {
    let mut features = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("sse2") {
            features.push(CpuFeature::X86Sse2);
        }
        if std::is_x86_feature_detected!("avx") {
            features.push(CpuFeature::X86Avx);
        }
        if std::is_x86_feature_detected!("avx2") {
            features.push(CpuFeature::X86Avx2);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            features.push(CpuFeature::ArmNeon);
        }
    }
    features
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
    fn hardware_detection_does_not_claim_unprobed_acceleration() {
        for host in HostOs::ALL {
            let hardware = detect_hardware(Some(host));
            assert!(hardware.acceleration_api.is_some());
            assert_eq!(hardware.acceleration_state, ProbeState::Unprobed);
        }
    }

    #[test]
    fn current_target_isa_matches_rust_cfg() {
        #[cfg(target_arch = "x86_64")]
        assert_eq!(current_isa(), Some(Isa::X86_64));
        #[cfg(target_arch = "aarch64")]
        assert_eq!(current_isa(), Some(Isa::Aarch64));
    }
}
