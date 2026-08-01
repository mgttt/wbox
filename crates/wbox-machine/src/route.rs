use crate::{guest_contract, ExecutionProvider, GuestContract, GuestOs, Isa, IsolationModel};

pub const CONTRACT_REVISION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    pub isa: Isa,
    pub guest_contract: GuestContract,
    pub priority: Priority,
    pub provider: ExecutionProvider,
    pub isolation: IsolationModel,
    pub availability: Availability,
    pub reason: &'static str,
}

pub const fn route(host: HostOs, guest: GuestOs, isa: Isa) -> Route {
    use Availability::{Available, Legacy, Planned, Research};
    use ExecutionProvider::{
        CompatibilityRuntime, FullSystemVirtualizer, NativeKernel, UserModeEmulator,
    };
    use GuestOs::{Linux as LinuxGuest, Macos as MacosGuest, Windows as WindowsGuest};
    use HostOs::{Linux, Macos, Windows};
    use Isa::{Aarch64, X86_64};
    use IsolationModel::{AppContainerJob, LinuxNamespaces, ProviderBoundary};
    use Priority::{Core, Deferred};

    let priority = match guest {
        WindowsGuest | LinuxGuest => Core,
        MacosGuest => Deferred,
    };
    let isolation = match (host, guest) {
        (Windows, WindowsGuest | LinuxGuest) => AppContainerJob,
        (Linux, WindowsGuest | LinuxGuest) => LinuxNamespaces,
        _ => ProviderBoundary,
    };
    let (provider, availability, reason) = match (host, guest, isa) {
        (Windows, WindowsGuest, X86_64) => (
            NativeKernel,
            Available,
            "AppContainer token plus Job Object",
        ),
        (Windows, WindowsGuest, Aarch64) => (
            NativeKernel,
            Planned,
            "Windows AArch64 build and AppContainer product gate are not established",
        ),
        (Windows, LinuxGuest, X86_64) => (
            UserModeEmulator,
            Available,
            "x86-64 wbox-linux inside AppContainer plus Job Object",
        ),
        (Windows, LinuxGuest, Aarch64) => (
            UserModeEmulator,
            Planned,
            "first-party AArch64 CPU core and Linux personality are not implemented",
        ),
        (Windows, MacosGuest, _) => (
            FullSystemVirtualizer,
            Research,
            "first-party Rust Darwin runtime is not implemented",
        ),
        (Linux, LinuxGuest, X86_64) => (
            NativeKernel,
            Available,
            "x86-64 rootless namespaces plus cgroup or explicit limit fallback",
        ),
        (Linux, LinuxGuest, Aarch64) => (
            NativeKernel,
            Planned,
            "Linux AArch64 build and native product gate are not established",
        ),
        (Linux, WindowsGuest, X86_64) => (
            CompatibilityRuntime,
            Legacy,
            "system Wine path is legacy; the first-party Rust Win32 runtime is not implemented",
        ),
        (Linux, WindowsGuest, Aarch64) => (
            CompatibilityRuntime,
            Planned,
            "first-party AArch64 PE/Win32 runtime is not implemented",
        ),
        (Linux, MacosGuest, _) => (
            FullSystemVirtualizer,
            Research,
            "first-party Rust Darwin runtime is not implemented",
        ),
        (Macos, MacosGuest, _) => (
            NativeKernel,
            Planned,
            "native macOS sandbox adapter is not implemented",
        ),
        (Macos, LinuxGuest, X86_64) => (
            UserModeEmulator,
            Planned,
            "port x86-64 wbox-linux and qualify a macOS outer sandbox",
        ),
        (Macos, LinuxGuest, Aarch64) => (
            UserModeEmulator,
            Planned,
            "implement the AArch64 Linux personality and qualify a macOS outer sandbox",
        ),
        (Macos, WindowsGuest, X86_64) => (
            CompatibilityRuntime,
            Planned,
            "port the first-party x86-64 Win32 runtime and qualify a macOS outer sandbox",
        ),
        (Macos, WindowsGuest, Aarch64) => (
            CompatibilityRuntime,
            Planned,
            "implement the AArch64 Win32 runtime and qualify a macOS outer sandbox",
        ),
    };
    Route {
        host,
        guest,
        isa,
        guest_contract: guest_contract(guest, isa),
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
    fn matrix_is_complete_unique_and_abi_bound() {
        let mut tuples = Vec::new();
        for host in HostOs::ALL {
            for guest in GuestOs::ALL {
                for isa in Isa::ALL {
                    let item = route(host, guest, isa);
                    assert_eq!(item.host, host);
                    assert_eq!(item.guest_contract, guest_contract(guest, isa));
                    tuples.push((item.host.as_str(), item.guest.as_str(), item.isa.as_str()));
                }
            }
        }
        tuples.sort_unstable();
        tuples.dedup();
        assert_eq!(tuples.len(), 18);
    }

    #[test]
    fn core_routes_cover_two_primary_guests_on_every_host_and_isa() {
        for host in HostOs::ALL {
            for guest in [GuestOs::Windows, GuestOs::Linux] {
                for isa in Isa::ALL {
                    assert_eq!(route(host, guest, isa).priority, Priority::Core);
                }
            }
        }
    }

    #[test]
    fn only_qualified_first_party_routes_are_available() {
        let available = HostOs::ALL
            .into_iter()
            .flat_map(|host| {
                GuestOs::ALL
                    .into_iter()
                    .flat_map(move |guest| Isa::ALL.map(move |isa| route(host, guest, isa)))
            })
            .filter(|item| item.availability == Availability::Available)
            .map(|item| (item.host, item.guest, item.isa))
            .collect::<Vec<_>>();
        assert_eq!(
            available,
            vec![
                (HostOs::Windows, GuestOs::Windows, Isa::X86_64),
                (HostOs::Windows, GuestOs::Linux, Isa::X86_64),
                (HostOs::Linux, GuestOs::Linux, Isa::X86_64),
            ]
        );
    }

    #[test]
    fn legacy_wine_is_visible_but_not_available() {
        let item = route(HostOs::Linux, GuestOs::Windows, Isa::X86_64);
        assert_eq!(item.availability, Availability::Legacy);
        assert_eq!(item.provider, ExecutionProvider::CompatibilityRuntime);
    }

    #[test]
    fn macos_routes_do_not_claim_unimplemented_isolation() {
        for guest in GuestOs::ALL {
            for isa in Isa::ALL {
                assert_ne!(
                    route(HostOs::Macos, guest, isa).availability,
                    Availability::Available
                );
            }
        }
    }
}
