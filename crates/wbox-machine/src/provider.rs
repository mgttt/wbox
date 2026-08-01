#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionProvider {
    NativeKernel,
    UserModeEmulator,
    CompatibilityRuntime,
    FullSystemVirtualizer,
}

impl ExecutionProvider {
    pub const ALL: [Self; 4] = [
        Self::NativeKernel,
        Self::UserModeEmulator,
        Self::CompatibilityRuntime,
        Self::FullSystemVirtualizer,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeKernel => "native-kernel",
            Self::UserModeEmulator => "user-mode-emulator",
            Self::CompatibilityRuntime => "compatibility-runtime",
            Self::FullSystemVirtualizer => "full-system-virtualizer",
        }
    }

    /// Returns the architectural envelope of this provider class.
    ///
    /// This does not report an implementation as available. Route availability
    /// and runtime hardware probes remain separate gates.
    pub const fn capabilities(self) -> ProviderCapabilities {
        match self {
            Self::NativeKernel => ProviderCapabilities::new(false, false, false, false),
            Self::UserModeEmulator => ProviderCapabilities::new(true, false, false, false),
            Self::CompatibilityRuntime => ProviderCapabilities::new(true, false, false, false),
            Self::FullSystemVirtualizer => ProviderCapabilities::new(true, true, true, true),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub cross_isa: bool,
    pub guest_kernel: bool,
    pub device_model: bool,
    pub snapshots: bool,
}

impl ProviderCapabilities {
    const fn new(cross_isa: bool, guest_kernel: bool, device_model: bool, snapshots: bool) -> Self {
        Self {
            cross_isa,
            guest_kernel,
            device_model,
            snapshots,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IsolationModel {
    AppContainerJob,
    LinuxNamespaces,
    ProviderBoundary,
}

impl IsolationModel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AppContainerJob => "appcontainer+job",
            Self::LinuxNamespaces => "linux-namespaces",
            Self::ProviderBoundary => "provider-boundary",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_system_provider_is_the_only_guest_kernel_model() {
        for provider in ExecutionProvider::ALL {
            assert_eq!(
                provider.capabilities().guest_kernel,
                provider == ExecutionProvider::FullSystemVirtualizer
            );
        }
    }
}
