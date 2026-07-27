use super::{Result, RuntimeError};

const ELF_HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;

pub(super) struct ElfImage {
    pub(super) entry: u64,
    segments: Vec<LoadSegment>,
}

struct LoadSegment {
    address: u64,
    memory_size: u64,
    flags: u32,
    bytes: Vec<u8>,
}

impl ElfImage {
    pub(super) fn parse(input: &[u8]) -> Result<Self> {
        if input.len() < ELF_HEADER_SIZE {
            return Err(RuntimeError::new("truncated ELF64 header"));
        }
        if &input[..4] != b"\x7fELF" || input[4] != 2 || input[5] != 1 || input[6] != 1 {
            return Err(RuntimeError::new("expected little-endian ELF64 version 1"));
        }
        if read_u16(input, 16)? != 2 {
            return Err(RuntimeError::new("only static ET_EXEC ELF is supported"));
        }
        if read_u16(input, 18)? != 62 {
            return Err(RuntimeError::new("ELF machine is not x86-64"));
        }
        if read_u32(input, 20)? != 1 {
            return Err(RuntimeError::new("unsupported ELF version"));
        }

        let entry = read_u64(input, 24)?;
        let ph_offset = usize_from_u64(read_u64(input, 32)?, "program header offset")?;
        let ph_size = read_u16(input, 54)? as usize;
        let ph_count = read_u16(input, 56)? as usize;
        if ph_size != PROGRAM_HEADER_SIZE {
            return Err(RuntimeError::new("unexpected ELF64 program header size"));
        }
        let table_size = ph_size
            .checked_mul(ph_count)
            .ok_or_else(|| RuntimeError::new("program header table overflows"))?;
        checked_range(input.len(), ph_offset, table_size, "program header table")?;

        let mut segments = Vec::new();
        for index in 0..ph_count {
            let ph = ph_offset + index * ph_size;
            if read_u32(input, ph)? != PT_LOAD {
                continue;
            }
            let flags = read_u32(input, ph + 4)?;
            let file_offset = usize_from_u64(read_u64(input, ph + 8)?, "segment offset")?;
            let address = read_u64(input, ph + 16)?;
            let file_size = usize_from_u64(read_u64(input, ph + 32)?, "segment file size")?;
            let memory_size = read_u64(input, ph + 40)?;
            if file_size as u64 > memory_size {
                return Err(RuntimeError::new(
                    "ELF segment file size exceeds memory size",
                ));
            }
            address
                .checked_add(memory_size)
                .ok_or_else(|| RuntimeError::new("ELF segment address overflows"))?;
            let range = checked_range(input.len(), file_offset, file_size, "ELF segment contents")?;
            segments.push(LoadSegment {
                address,
                memory_size,
                flags,
                bytes: input[range].to_vec(),
            });
        }
        if segments.is_empty() {
            return Err(RuntimeError::new("ELF contains no loadable segments"));
        }
        if !segments.iter().any(|segment| {
            segment.flags & PF_X != 0
                && entry >= segment.address
                && entry < segment.address + segment.memory_size
        }) {
            return Err(RuntimeError::new(
                "ELF entry point is outside executable memory",
            ));
        }
        Ok(Self { entry, segments })
    }
}

pub(super) struct AddressSpace {
    regions: Vec<Region>,
}

struct Region {
    start: u64,
    flags: u32,
    bytes: Vec<u8>,
}

impl AddressSpace {
    pub(super) fn load(image: &ElfImage) -> Result<Self> {
        let mut segments: Vec<_> = image.segments.iter().collect();
        segments.sort_unstable_by_key(|segment| segment.address);
        let mut regions = Vec::with_capacity(segments.len());
        let mut previous_end = 0;
        for segment in segments {
            if segment.address < previous_end {
                return Err(RuntimeError::new("overlapping ELF load segments"));
            }
            let size = usize_from_u64(segment.memory_size, "segment memory size")?;
            let mut bytes = vec![0; size];
            bytes[..segment.bytes.len()].copy_from_slice(&segment.bytes);
            previous_end = segment.address + segment.memory_size;
            regions.push(Region {
                start: segment.address,
                flags: segment.flags,
                bytes,
            });
        }
        Ok(Self { regions })
    }

    pub(super) fn read_executable(&self, address: u64, length: usize) -> Result<&[u8]> {
        let end = address
            .checked_add(length as u64)
            .ok_or_else(|| RuntimeError::new("guest instruction address overflows"))?;
        let region = self
            .regions
            .iter()
            .find(|region| {
                address >= region.start && end <= region.start + region.bytes.len() as u64
            })
            .ok_or_else(|| {
                RuntimeError::new(format!("unmapped guest instruction at {address:#x}"))
            })?;
        if region.flags & PF_X == 0 {
            return Err(RuntimeError::new(format!(
                "instruction fetch from non-executable memory at {address:#x}"
            )));
        }
        let offset = (address - region.start) as usize;
        Ok(&region.bytes[offset..offset + length])
    }
}

fn checked_range(
    total: usize,
    offset: usize,
    length: usize,
    description: &str,
) -> Result<std::ops::Range<usize>> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| RuntimeError::new(format!("{description} overflows")))?;
    if end > total {
        return Err(RuntimeError::new(format!("truncated {description}")));
    }
    Ok(offset..end)
}

fn usize_from_u64(value: u64, description: &str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| RuntimeError::new(format!("{description} does not fit the host")))
}

fn read_u16(input: &[u8], offset: usize) -> Result<u16> {
    let bytes = checked_range(input.len(), offset, 2, "ELF field")?;
    Ok(u16::from_le_bytes(input[bytes].try_into().unwrap()))
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32> {
    let bytes = checked_range(input.len(), offset, 4, "ELF field")?;
    Ok(u32::from_le_bytes(input[bytes].try_into().unwrap()))
}

fn read_u64(input: &[u8], offset: usize) -> Result<u64> {
    let bytes = checked_range(input.len(), offset, 8, "ELF field")?;
    Ok(u64::from_le_bytes(input[bytes].try_into().unwrap()))
}
