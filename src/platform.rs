//! wbox product-level host/guest execution contract.
//!
//! This layer answers which execution and isolation model wbox promises for a
//! host/guest pair. Native mechanics (process trees, durable files, locks) may
//! later come from `agenterm-platform`; those mechanics must not own this
//! product routing policy.

pub const CONTRACT_REVISION: u32 = 2;

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
    CompatibilityRuntime,
    FullSystemVirtualizer,
}

impl ExecutionProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeKernel => "native-kernel",
            Self::UserModeEmulator => "user-mode-emulator",
            Self::CompatibilityRuntime => "compatibility-runtime",
            Self::FullSystemVirtualizer => "full-system-virtualizer",
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
    Legacy,
    Planned,
    Research,
}

impl Availability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Legacy => "legacy",
            Self::Planned => "planned",
            Self::Research => "research",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Core,
    Deferred,
}

impl Priority {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Deferred => "deferred",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub host: HostOs,
    pub guest: GuestOs,
    pub priority: Priority,
    pub provider: ExecutionProvider,
    pub isolation: IsolationModel,
    pub availability: Availability,
    pub reason: &'static str,
}

pub const fn route(host: HostOs, guest: GuestOs) -> Route {
    use Availability::{Available, Legacy, Planned, Research};
    use ExecutionProvider::{
        CompatibilityRuntime, FullSystemVirtualizer, NativeKernel, UserModeEmulator,
    };
    use GuestOs::{Linux as LinuxGuest, Macos as MacosGuest, Windows as WindowsGuest};
    use HostOs::{Linux, Macos, Windows};
    use IsolationModel::{AppContainerJob, LinuxNamespaces, ProviderBoundary};
    use Priority::{Core, Deferred};

    let priority = match guest {
        WindowsGuest | LinuxGuest => Core,
        MacosGuest => Deferred,
    };

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
            FullSystemVirtualizer,
            ProviderBoundary,
            Research,
            "first-party Rust Darwin runtime is not implemented",
        ),
        (Linux, LinuxGuest) => (
            NativeKernel,
            LinuxNamespaces,
            Available,
            "rootless namespaces plus cgroup or explicit limit fallback",
        ),
        (Linux, WindowsGuest) => (
            CompatibilityRuntime,
            LinuxNamespaces,
            Legacy,
            "system Wine path is legacy; the first-party Rust Win32 runtime is not implemented",
        ),
        (Linux, MacosGuest) => (
            FullSystemVirtualizer,
            ProviderBoundary,
            Research,
            "first-party Rust Darwin runtime is not implemented",
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
            CompatibilityRuntime,
            ProviderBoundary,
            Planned,
            "implement a first-party Rust Win32 runtime plus a macOS outer sandbox",
        ),
    };
    Route {
        host,
        guest,
        priority,
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
    fn three_hosts_by_two_primary_guests_are_core() {
        let core = HostOs::ALL
            .into_iter()
            .flat_map(|host| GuestOs::ALL.map(|guest| route(host, guest)))
            .filter(|item| item.priority == Priority::Core)
            .count();
        assert_eq!(core, 6);
        for host in HostOs::ALL {
            assert_eq!(route(host, GuestOs::Macos).priority, Priority::Deferred);
        }
    }

    #[test]
    fn only_rust_only_qualified_routes_are_available() {
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
                (HostOs::Linux, GuestOs::Linux),
            ]
        );
    }

    #[test]
    fn external_wine_route_is_visible_but_not_rust_only_available() {
        let item = route(HostOs::Linux, GuestOs::Windows);
        assert_eq!(item.availability, Availability::Legacy);
        assert_eq!(item.provider, ExecutionProvider::CompatibilityRuntime);
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
