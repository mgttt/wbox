use crate::{
    Availability, ExecutionProvider, GuestAbi, GuestContract, GuestOs, HostOs, Isa, IsolationModel,
    Priority,
};

/// Native ABI personality exposed by a host operating system.
///
/// This is a product-level contract, not a claim that a guest runtime exists
/// for the ABI. Host detection remains delegated to `agenterm-platform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostAbi {
    WindowsNt,
    LinuxSyscall,
    Darwin,
}

impl HostAbi {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WindowsNt => "windows-nt",
            Self::LinuxSyscall => "linux-syscall",
            Self::Darwin => "darwin",
        }
    }
}

pub const fn host_abi(host: HostOs) -> HostAbi {
    match host {
        HostOs::Windows => HostAbi::WindowsNt,
        HostOs::Linux => HostAbi::LinuxSyscall,
        HostOs::Macos => HostAbi::Darwin,
    }
}

/// The minimum identity needed to hand a machine route to another layer.
///
/// The value is deliberately descriptive: availability and provider facts are
/// carried through unchanged, while actual host probing stays outside this
/// crate's matrix contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineCore {
    pub host: HostOs,
    pub host_abi: HostAbi,
    pub guest: GuestOs,
    pub guest_contract: GuestContract,
    pub isa: Isa,
    pub provider: ExecutionProvider,
    pub isolation: IsolationModel,
    pub priority: Priority,
    pub availability: Availability,
}

impl MachineCore {
    pub const fn from_route(route: crate::Route) -> Self {
        Self {
            host: route.host,
            host_abi: host_abi(route.host),
            guest: route.guest,
            guest_contract: route.guest_contract,
            isa: route.isa,
            provider: route.provider,
            isolation: route.isolation,
            priority: route.priority,
            availability: route.availability,
        }
    }

    pub const fn guest_abi(self) -> GuestAbi {
        self.guest_contract.abi
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route;

    #[test]
    fn host_abi_is_stable_for_all_matrix_hosts() {
        assert_eq!(host_abi(HostOs::Windows).as_str(), "windows-nt");
        assert_eq!(host_abi(HostOs::Linux).as_str(), "linux-syscall");
        assert_eq!(host_abi(HostOs::Macos).as_str(), "darwin");
    }

    #[test]
    fn machine_core_preserves_route_identity_and_personality() {
        let core = MachineCore::from_route(route(HostOs::Windows, GuestOs::Linux, Isa::X86_64));
        assert_eq!(core.host_abi, HostAbi::WindowsNt);
        assert_eq!(core.guest_abi(), GuestAbi::LinuxSyscall);
        assert_eq!(
            core.guest_contract,
            crate::guest_contract(GuestOs::Linux, Isa::X86_64)
        );
        assert_eq!(core.provider, ExecutionProvider::UserModeEmulator);
        assert_eq!(core.isolation, IsolationModel::AppContainerJob);
    }

    #[test]
    fn every_route_can_be_represented_by_the_minimum_core() {
        for host in HostOs::ALL {
            for guest in GuestOs::ALL {
                for isa in Isa::ALL {
                    let route = route(host, guest, isa);
                    let core = MachineCore::from_route(route);
                    assert_eq!(core.host, host);
                    assert_eq!(core.guest, guest);
                    assert_eq!(core.isa, isa);
                    assert_eq!(core.host_abi, host_abi(host));
                    assert_eq!(
                        core.guest_contract.abi,
                        crate::guest_contract(guest, isa).abi
                    );
                }
            }
        }
    }
}
