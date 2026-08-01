use std::fmt;

use crate::{BinaryFormat, GuestAbi, GuestOs, Isa};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactIdentity {
    pub format: BinaryFormat,
    pub guest_os: GuestOs,
    pub guest_abi: GuestAbi,
    pub isa: Isa,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    Truncated(&'static str),
    UnknownFormat,
    UnsupportedClass(&'static str),
    UnsupportedEndian(&'static str),
    UnsupportedMachine { format: BinaryFormat, machine: u32 },
    InvalidHeader(&'static str),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(format) => write!(f, "truncated {format} header"),
            Self::UnknownFormat => write!(f, "unrecognized executable format"),
            Self::UnsupportedClass(class) => write!(f, "unsupported executable class: {class}"),
            Self::UnsupportedEndian(endian) => {
                write!(f, "unsupported executable byte order: {endian}")
            }
            Self::UnsupportedMachine { format, machine } => write!(
                f,
                "unsupported {} machine identifier {machine:#x}",
                format.as_str()
            ),
            Self::InvalidHeader(message) => write!(f, "invalid executable header: {message}"),
        }
    }
}

impl std::error::Error for ArtifactError {}

pub fn inspect_artifact(bytes: &[u8]) -> Result<ArtifactIdentity, ArtifactError> {
    if bytes.starts_with(b"\x7fELF") {
        inspect_elf(bytes)
    } else if bytes.starts_with(b"MZ") {
        inspect_pe(bytes)
    } else if bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]) {
        inspect_macho(bytes)
    } else if bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe])
        || bytes.starts_with(&[0xbe, 0xba, 0xfe, 0xca])
    {
        // TODO(WM-ARTIFACT-FAT): select a slice from Mach-O universal binaries.
        Err(ArtifactError::UnsupportedClass("Mach-O universal"))
    } else {
        Err(ArtifactError::UnknownFormat)
    }
}

fn inspect_elf(bytes: &[u8]) -> Result<ArtifactIdentity, ArtifactError> {
    if bytes.len() < 20 {
        return Err(ArtifactError::Truncated("ELF"));
    }
    if bytes[4] != 2 {
        // TODO(WM-ARTIFACT-32): add 32-bit ISA contracts before accepting ELF32.
        return Err(ArtifactError::UnsupportedClass("ELF32"));
    }
    if bytes[5] != 1 {
        return Err(ArtifactError::UnsupportedEndian("ELF big-endian"));
    }
    let machine = u16::from_le_bytes([bytes[18], bytes[19]]) as u32;
    let isa = match machine {
        0x3e => Isa::X86_64,
        0xb7 => Isa::Aarch64,
        machine => {
            return Err(ArtifactError::UnsupportedMachine {
                format: BinaryFormat::Elf64,
                machine,
            });
        }
    };
    Ok(ArtifactIdentity {
        format: BinaryFormat::Elf64,
        guest_os: GuestOs::Linux,
        guest_abi: GuestAbi::LinuxSyscall,
        isa,
    })
}

fn inspect_pe(bytes: &[u8]) -> Result<ArtifactIdentity, ArtifactError> {
    if bytes.len() < 0x40 {
        return Err(ArtifactError::Truncated("DOS/PE"));
    }
    let offset = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    let header_end = offset
        .checked_add(26)
        .ok_or(ArtifactError::InvalidHeader("PE header offset overflow"))?;
    if header_end > bytes.len() {
        return Err(ArtifactError::Truncated("PE"));
    }
    if bytes.get(offset..offset + 4) != Some(b"PE\0\0".as_slice()) {
        return Err(ArtifactError::InvalidHeader("missing PE signature"));
    }
    let machine = u16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]]) as u32;
    let optional_magic = u16::from_le_bytes([bytes[offset + 24], bytes[offset + 25]]);
    if optional_magic != 0x20b {
        // TODO(WM-ARTIFACT-32): add 32-bit ISA contracts before accepting PE32.
        return Err(ArtifactError::UnsupportedClass("PE32"));
    }
    let isa = match machine {
        0x8664 => Isa::X86_64,
        0xaa64 => Isa::Aarch64,
        machine => {
            return Err(ArtifactError::UnsupportedMachine {
                format: BinaryFormat::Pe32Plus,
                machine,
            });
        }
    };
    Ok(ArtifactIdentity {
        format: BinaryFormat::Pe32Plus,
        guest_os: GuestOs::Windows,
        guest_abi: GuestAbi::WindowsNt,
        isa,
    })
}

fn inspect_macho(bytes: &[u8]) -> Result<ArtifactIdentity, ArtifactError> {
    if bytes.len() < 8 {
        return Err(ArtifactError::Truncated("Mach-O"));
    }
    let machine = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let isa = match machine {
        0x0100_0007 => Isa::X86_64,
        0x0100_000c => Isa::Aarch64,
        machine => {
            return Err(ArtifactError::UnsupportedMachine {
                format: BinaryFormat::MachO64,
                machine,
            });
        }
    };
    Ok(ArtifactIdentity {
        format: BinaryFormat::MachO64,
        guest_os: GuestOs::Macos,
        guest_abi: GuestAbi::Darwin,
        isa,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0; 20];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    fn pe(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0; 0x80];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x40_u32.to_le_bytes());
        bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
        bytes[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
        bytes[0x58..0x5a].copy_from_slice(&0x20b_u16.to_le_bytes());
        bytes
    }

    fn macho(machine: u32) -> Vec<u8> {
        let mut bytes = vec![0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0];
        bytes[4..8].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    #[test]
    fn recognizes_all_prefilled_guest_and_isa_pairs() {
        let cases = [
            (elf(0x3e), GuestOs::Linux, Isa::X86_64),
            (elf(0xb7), GuestOs::Linux, Isa::Aarch64),
            (pe(0x8664), GuestOs::Windows, Isa::X86_64),
            (pe(0xaa64), GuestOs::Windows, Isa::Aarch64),
            (macho(0x0100_0007), GuestOs::Macos, Isa::X86_64),
            (macho(0x0100_000c), GuestOs::Macos, Isa::Aarch64),
        ];
        for (bytes, guest, isa) in cases {
            let identity = inspect_artifact(&bytes).unwrap();
            assert_eq!(identity.guest_os, guest);
            assert_eq!(identity.isa, isa);
        }
    }

    #[test]
    fn rejects_32_bit_and_unknown_machine_headers() {
        let mut elf32 = elf(0x3e);
        elf32[4] = 1;
        assert_eq!(
            inspect_artifact(&elf32),
            Err(ArtifactError::UnsupportedClass("ELF32"))
        );
        assert!(matches!(
            inspect_artifact(&pe(0x014c)),
            Err(ArtifactError::UnsupportedMachine { .. })
        ));
    }

    #[test]
    fn rejects_truncated_and_fake_headers() {
        assert_eq!(
            inspect_artifact(b"\x7fELF"),
            Err(ArtifactError::Truncated("ELF"))
        );
        let mut fake_pe = vec![0; 0x50];
        fake_pe[..2].copy_from_slice(b"MZ");
        fake_pe[0x3c..0x40].copy_from_slice(&0x40_u32.to_le_bytes());
        assert!(matches!(
            inspect_artifact(&fake_pe),
            Err(ArtifactError::Truncated("PE"))
                | Err(ArtifactError::InvalidHeader("missing PE signature"))
        ));
        assert_eq!(
            inspect_artifact(b"plain text"),
            Err(ArtifactError::UnknownFormat)
        );
    }
}
