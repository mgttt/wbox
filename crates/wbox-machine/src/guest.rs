use crate::Isa;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuestAbi {
    WindowsNt,
    LinuxSyscall,
    Darwin,
}

impl GuestAbi {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WindowsNt => "windows-nt",
            Self::LinuxSyscall => "linux-syscall",
            Self::Darwin => "darwin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryFormat {
    Pe32Plus,
    Elf64,
    MachO64,
}

impl BinaryFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pe32Plus => "pe32+",
            Self::Elf64 => "elf64",
            Self::MachO64 => "mach-o64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestContract {
    pub os: GuestOs,
    pub isa: Isa,
    pub abi: GuestAbi,
    pub binary_format: BinaryFormat,
}

pub const fn guest_contract(os: GuestOs, isa: Isa) -> GuestContract {
    let (abi, binary_format) = match os {
        GuestOs::Windows => (GuestAbi::WindowsNt, BinaryFormat::Pe32Plus),
        GuestOs::Linux => (GuestAbi::LinuxSyscall, BinaryFormat::Elf64),
        GuestOs::Macos => (GuestAbi::Darwin, BinaryFormat::MachO64),
    };
    GuestContract {
        os,
        isa,
        abi,
        binary_format,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_guest_isa_pair_has_an_abi_and_format() {
        let mut count = 0;
        for os in GuestOs::ALL {
            for isa in Isa::ALL {
                let contract = guest_contract(os, isa);
                assert_eq!(contract.os, os);
                assert_eq!(contract.isa, isa);
                count += 1;
            }
        }
        assert_eq!(count, 6);
    }
}
