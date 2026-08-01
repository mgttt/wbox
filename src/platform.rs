//! wbox product-level host/guest execution contract.
//!
//! This layer answers which execution and isolation model wbox promises for a
//! host/guest pair. Native mechanics (process trees, durable files, locks) may
//! later come from `agenterm-platform`; those mechanics must not own this
//! product routing policy.

pub const CONTRACT_REVISION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    Windows,
    Linux,
    Macos,
}

impl HostOs {
    pub const ALL: [Self; 3] = [Self::Windows, Self::Linux, Self::Macos];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Macos => "macos",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestOs {
    Windows,
    Linux,
    Macos,
}

impl GuestOs {
    pub const ALL: [Self; 3] = [Self::Windows, Self::Linux, Self::Macos];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Macos => "macos",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionProvider {
    NativeKernel,
    UserModeEmulator,
    CompatibilityLayer,
    ExternalVirtualMachine,
}

impl ExecutionProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeKernel => "native-kernel",
            Self::UserModeEmulator => "user-mode-emulator",
            Self::CompatibilityLayer => "compatibility-layer",
            Self::ExternalVirtualMachine => "external-virtual-machine",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    Planned,
    Research,
}

impl Availability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Planned => "planned",
            Self::Research => "research",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub host: HostOs,
    pub guest: GuestOs,
    pub provider: ExecutionProvider,
    pub isolation: IsolationModel,
    pub availability: Availability,
    pub reason: &'static str,
}

pub const fn route(host: HostOs, guest: GuestOs) -> Route {
    use Availability::{Available, Planned, Research};
    use ExecutionProvider::{
        CompatibilityLayer, ExternalVirtualMachine, NativeKernel, UserModeEmulator,
    };
    use GuestOs::{Linux as LinuxGuest, Macos as MacosGuest, Windows as WindowsGuest};
    use HostOs::{Linux, Macos, Windows};
    use IsolationModel::{AppContainerJob, LinuxNamespaces, ProviderBoundary};

    let (provider, isolation, availability, reason) = match (host, guest) {
        (Windows, WindowsGuest) => (
            NativeKernel,
            AppContainerJob,
            Available,
            "AppContainer token plus Job Object",
        ),
        (Windows, LinuxGuest) => (
            UserModeEmulator,
            AppContainerJob,
            Available,
            "wbox-linux inside AppContainer plus Job Object",
        ),
        (Windows, MacosGuest) => (
            ExternalVirtualMachine,
            ProviderBoundary,
            Research,
            "no qualified Darwin execution provider",
        ),
        (Linux, LinuxGuest) => (
            NativeKernel,
            LinuxNamespaces,
            Available,
            "rootless namespaces plus cgroup or explicit limit fallback",
        ),
        (Linux, WindowsGuest) => (
            CompatibilityLayer,
            LinuxNamespaces,
            Available,
            "Wine inside the Linux isolation boundary",
        ),
        (Linux, MacosGuest) => (
            ExternalVirtualMachine,
            ProviderBoundary,
            Research,
            "no qualified Darwin execution provider",
        ),
        (Macos, MacosGuest) => (
            NativeKernel,
            ProviderBoundary,
            Planned,
            "native macOS sandbox adapter is not implemented",
        ),
        (Macos, LinuxGuest) => (
            UserModeEmulator,
            ProviderBoundary,
            Planned,
            "port and qualify wbox-linux plus a macOS outer sandbox",
        ),
        (Macos, WindowsGuest) => (
            CompatibilityLayer,
            ProviderBoundary,
            Planned,
            "qualify Wine plus a macOS outer sandbox",
        ),
    };
    Route {
        host,
        guest,
        provider,
        isolation,
        availability,
        reason,
    }
}

pub const fn current_host() -> Option<HostOs> {
    if cfg!(windows) {
        Some(HostOs::Windows)
    } else if cfg!(target_os = "linux") {
        Some(HostOs::Linux)
    } else if cfg!(target_os = "macos") {
        Some(HostOs::Macos)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_is_complete_and_unique() {
        let mut pairs = Vec::new();
        for host in HostOs::ALL {
            for guest in GuestOs::ALL {
                let item = route(host, guest);
                assert_eq!(item.host, host);
                assert_eq!(item.guest, guest);
                pairs.push((item.host.as_str(), item.guest.as_str()));
            }
        }
        pairs.sort_unstable();
        pairs.dedup();
        assert_eq!(pairs.len(), 9);
    }

    #[test]
    fn only_qualified_routes_are_available() {
        let available = HostOs::ALL
            .into_iter()
            .flat_map(|host| GuestOs::ALL.map(|guest| route(host, guest)))
            .filter(|item| item.availability == Availability::Available)
            .map(|item| (item.host, item.guest))
            .collect::<Vec<_>>();
        assert_eq!(
            available,
            vec![
                (HostOs::Windows, GuestOs::Windows),
                (HostOs::Windows, GuestOs::Linux),
                (HostOs::Linux, GuestOs::Windows),
                (HostOs::Linux, GuestOs::Linux),
            ]
        );
    }

    #[test]
    fn macos_routes_do_not_claim_unimplemented_isolation() {
        for guest in GuestOs::ALL {
            assert_ne!(
                route(HostOs::Macos, guest).availability,
                Availability::Available
            );
        }
    }
}
