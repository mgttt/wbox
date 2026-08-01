use crate::HostOs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcceleratorClass {
    Gpu,
    Npu,
    Lpu,
}

impl AcceleratorClass {
    pub const ALL: [Self; 3] = [Self::Gpu, Self::Npu, Self::Lpu];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Npu => "npu",
            Self::Lpu => "lpu",
        }
    }

    pub const fn workload(self) -> AcceleratorWorkload {
        match self {
            Self::Gpu => AcceleratorWorkload::ParallelCompute,
            Self::Npu => AcceleratorWorkload::TensorCompute,
            Self::Lpu => AcceleratorWorkload::LanguageCompute,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcceleratorWorkload {
    ParallelCompute,
    TensorCompute,
    LanguageCompute,
}

impl AcceleratorWorkload {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ParallelCompute => "parallel-compute",
            Self::TensorCompute => "tensor-compute",
            Self::LanguageCompute => "language-compute",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceleratorRouteStatus {
    Research,
}

impl AcceleratorRouteStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Research => "research",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceleratorRoute {
    pub host: HostOs,
    pub class: AcceleratorClass,
    pub workload: AcceleratorWorkload,
    pub status: AcceleratorRouteStatus,
    pub todo: &'static str,
}

pub fn accelerator_routes() -> Vec<AcceleratorRoute> {
    HostOs::ALL
        .into_iter()
        .flat_map(|host| {
            AcceleratorClass::ALL.into_iter().map(move |class| {
                let todo = match class {
                    // TODO(WM-GPU): define discovery, memory, queue, and isolation contracts.
                    AcceleratorClass::Gpu => "WM-GPU",
                    // TODO(WM-NPU): define tensor formats, scheduling, and isolation contracts.
                    AcceleratorClass::Npu => "WM-NPU",
                    // TODO(WM-LPU): define language compute, memory, and scheduling contracts.
                    AcceleratorClass::Lpu => "WM-LPU",
                };
                AcceleratorRoute {
                    host,
                    class,
                    workload: class.workload(),
                    status: AcceleratorRouteStatus::Research,
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
    fn three_hosts_by_three_accelerator_classes_are_prefilled() {
        let routes = accelerator_routes();
        assert_eq!(routes.len(), 9);
        let mut tuples = routes
            .into_iter()
            .map(|route| {
                assert!(!route.todo.is_empty());
                assert_eq!(route.workload, route.class.workload());
                (route.host.as_str(), route.class.as_str())
            })
            .collect::<Vec<_>>();
        tuples.sort_unstable();
        tuples.dedup();
        assert_eq!(tuples.len(), 9);
    }
}
