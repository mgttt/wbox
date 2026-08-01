//! Host memory facts shared by infrastructure probes and providers.

pub use agenterm_platform::host_memory::{HostMemoryError, HostMemoryErrorKind, HostMemoryFacts};

pub fn detect_host_memory() -> Result<HostMemoryFacts, HostMemoryError> {
    agenterm_platform::host_memory::facts()
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
    }
}
