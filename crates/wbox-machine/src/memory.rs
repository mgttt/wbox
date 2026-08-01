//! Host memory facts shared by infrastructure probes and providers.

pub use agenterm_platform::host_memory::{
    HostMemoryAvailability, HostMemoryAvailabilitySemantics, HostMemoryError, HostMemoryErrorKind,
    HostMemoryFacts,
};

pub fn detect_host_memory() -> Result<HostMemoryFacts, HostMemoryError> {
    agenterm_platform::host_memory::facts()
}

pub fn detect_host_memory_availability() -> Result<HostMemoryAvailability, HostMemoryError> {
    agenterm_platform::host_memory::availability()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_host_memory_contract_is_coherent() {
        let facts = detect_host_memory().expect("detect host memory");
        assert!(facts.allocation_granularity.get() >= facts.page_size.get());
        assert_eq!(
            facts.allocation_granularity.get() % facts.page_size.get(),
            0
        );
        assert!(facts.physical_bytes.get() >= facts.page_size.get() as u64);

        let availability =
            detect_host_memory_availability().expect("detect host memory availability");
        assert!(availability.available_physical_bytes <= facts.physical_bytes.get());
        assert!(!availability.semantics.as_str().is_empty());
    }
}
