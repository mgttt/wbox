//! Machine-level contracts shared by wbox execution providers.
//!
//! This crate owns wbox product semantics: ISA identity, host hardware facts,
//! guest ABI identity, provider classes, and the host/guest/ISA route matrix.
//! Native OS mechanisms remain below this boundary.

mod architecture;
mod guest;
mod provider;
mod route;

pub use architecture::{
    current_isa, detect_hardware, AccelerationApi, CpuFeature, HardwareCapabilities, Isa,
    ProbeState,
};
pub use guest::{guest_contract, BinaryFormat, GuestAbi, GuestContract, GuestOs};
pub use provider::{ExecutionProvider, IsolationModel, ProviderCapabilities};
pub use route::{current_host, route, Availability, HostOs, Priority, Route, CONTRACT_REVISION};
