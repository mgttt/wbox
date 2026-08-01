//! Machine-level contracts shared by wbox execution providers.
//!
//! This crate owns wbox product semantics: ISA identity, host hardware facts,
//! guest ABI identity, provider classes, and the host/guest/ISA route matrix.
//! Native OS mechanisms remain below this boundary.

mod accelerator;
mod architecture;
mod artifact;
mod device;
mod guest;
mod parallel;
mod provider;
mod route;
mod topology;
mod wasm;

pub use accelerator::{
    accelerator_routes, AcceleratorClass, AcceleratorRoute, AcceleratorRouteStatus,
    AcceleratorWorkload,
};
pub use architecture::{
    current_isa, detect_hardware, AccelerationApi, CpuFeature, HardwareCapabilities, Isa,
    ProbeState, ProcessorIsa,
};
pub use artifact::{inspect_artifact, ArtifactError, ArtifactIdentity};
pub use device::{esp32_routes, DeviceFamily, DeviceRoute, DeviceRouteStatus, FirmwareEnvironment};
pub use guest::{guest_contract, BinaryFormat, GuestAbi, GuestContract, GuestOs};
pub use parallel::{
    parallel_routes, DataPath, ParallelExecution, ParallelRoute, ParallelRouteStatus,
};
pub use provider::{ExecutionProvider, IsolationModel, ProviderCapabilities};
pub use route::{current_host, route, Availability, HostOs, Priority, Route, CONTRACT_REVISION};
pub use topology::{
    prefilled_topology, ComputeFabric, CoordinationModel, DistributionModel, ExecutionDomain,
    InfrastructureTopology, LinkDirection, ResourceKind, ResourceLink, ResourceNode, TopologyError,
    TopologyState, TransportClass,
};
pub use wasm::{
    wasm_machine_routes, WasmHostSurface, WasmMachineCapability, WasmMachineRoute, WasmRouteStatus,
};
