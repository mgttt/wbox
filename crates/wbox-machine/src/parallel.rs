#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParallelExecution {
    Serial,
    Simd,
    Threads,
    Processes,
    SimdThreads,
    SimdProcesses,
}

impl ParallelExecution {
    pub const ALL: [Self; 6] = [
        Self::Serial,
        Self::Simd,
        Self::Threads,
        Self::Processes,
        Self::SimdThreads,
        Self::SimdProcesses,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Serial => "serial",
            Self::Simd => "simd",
            Self::Threads => "threads",
            Self::Processes => "processes",
            Self::SimdThreads => "simd-threads",
            Self::SimdProcesses => "simd-processes",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataPath {
    PrivateCopy,
    BorrowedShared,
    SharedMapping,
    RingBuffer,
    ScatterGather,
}

impl DataPath {
    pub const ALL: [Self; 5] = [
        Self::PrivateCopy,
        Self::BorrowedShared,
        Self::SharedMapping,
        Self::RingBuffer,
        Self::ScatterGather,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PrivateCopy => "private-copy",
            Self::BorrowedShared => "borrowed-shared",
            Self::SharedMapping => "shared-mapping",
            Self::RingBuffer => "ring-buffer",
            Self::ScatterGather => "scatter-gather",
        }
    }

    pub const fn logical_data_copies(self) -> u8 {
        match self {
            Self::PrivateCopy => 1,
            Self::BorrowedShared | Self::SharedMapping | Self::RingBuffer | Self::ScatterGather => {
                0
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelRouteStatus {
    Declared,
    Planned,
    Research,
}

impl ParallelRouteStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Planned => "planned",
            Self::Research => "research",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParallelRoute {
    pub execution: ParallelExecution,
    pub data_path: DataPath,
    pub status: ParallelRouteStatus,
    pub todo: Option<&'static str>,
}

pub fn parallel_routes() -> Vec<ParallelRoute> {
    use DataPath::{BorrowedShared, PrivateCopy, RingBuffer, ScatterGather, SharedMapping};
    use ParallelExecution::{Processes, Serial, Simd, SimdProcesses, SimdThreads, Threads};
    use ParallelRouteStatus::{Declared, Planned, Research};

    ParallelExecution::ALL
        .into_iter()
        .flat_map(|execution| {
            DataPath::ALL.into_iter().map(move |data_path| {
                let (status, todo) = match (execution, data_path) {
                    (Serial, PrivateCopy)
                    | (Simd, BorrowedShared)
                    | (Threads, BorrowedShared)
                    | (Processes, SharedMapping)
                    | (SimdThreads, BorrowedShared) => (Declared, None),
                    // TODO(WM-HPC-SIMD-PROCESS): execute ISA kernels over shared mappings.
                    (SimdProcesses, SharedMapping) => (Planned, Some("WM-HPC-SIMD-PROCESS")),
                    // TODO(WM-HPC-RING): add bounded SPSC/MPMC ownership and backpressure gates.
                    (Threads | Processes, RingBuffer) => (Research, Some("WM-HPC-RING")),
                    // TODO(WM-HPC-SCATTER): connect vectored I/O to borrowed buffer lifetimes.
                    (Threads | Processes, ScatterGather) => (Research, Some("WM-HPC-SCATTER")),
                    // TODO(WM-HPC-MEMORY): justify any remaining execution/data combination.
                    _ => (Research, Some("WM-HPC-MEMORY")),
                };
                ParallelRoute {
                    execution,
                    data_path,
                    status,
                    todo,
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_by_data_path_matrix_is_complete() {
        let routes = parallel_routes();
        assert_eq!(routes.len(), 30);
        let mut tuples = routes
            .iter()
            .map(|route| (route.execution.as_str(), route.data_path.as_str()))
            .collect::<Vec<_>>();
        tuples.sort_unstable();
        tuples.dedup();
        assert_eq!(tuples.len(), 30);
        assert_eq!(
            routes
                .iter()
                .filter(|route| route.status == ParallelRouteStatus::Declared)
                .count(),
            5
        );
        for route in routes {
            if route.status != ParallelRouteStatus::Declared {
                assert!(route.todo.is_some());
            }
        }
    }

    #[test]
    fn shared_data_paths_are_logically_zero_copy() {
        for path in [
            DataPath::BorrowedShared,
            DataPath::SharedMapping,
            DataPath::RingBuffer,
            DataPath::ScatterGather,
        ] {
            assert_eq!(path.logical_data_copies(), 0);
        }
    }
}
