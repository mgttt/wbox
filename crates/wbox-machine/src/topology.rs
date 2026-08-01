use std::collections::HashSet;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResourceKind {
    Host,
    Cpu,
    Gpu,
    Npu,
    Lpu,
    Microcontroller,
    Memory,
    Storage,
    Network,
}

impl ResourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::Npu => "npu",
            Self::Lpu => "lpu",
            Self::Microcontroller => "microcontroller",
            Self::Memory => "memory",
            Self::Storage => "storage",
            Self::Network => "network",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyState {
    Declared,
    Observed,
    Available,
    Planned,
    Research,
}

impl TopologyState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Observed => "observed",
            Self::Available => "available",
            Self::Planned => "planned",
            Self::Research => "research",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceNode {
    pub id: String,
    pub kind: ResourceKind,
    pub state: TopologyState,
    pub todo: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransportClass {
    InProcess,
    SharedMemory,
    AcceleratorInterconnect,
    Peripheral,
    Network,
}

impl TransportClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "in-process",
            Self::SharedMemory => "shared-memory",
            Self::AcceleratorInterconnect => "accelerator-interconnect",
            Self::Peripheral => "peripheral",
            Self::Network => "network",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDirection {
    Directed,
    Bidirectional,
}

impl LinkDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Directed => "directed",
            Self::Bidirectional => "bidirectional",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLink {
    pub id: String,
    pub from: String,
    pub to: String,
    pub transport: TransportClass,
    pub direction: LinkDirection,
    pub state: TopologyState,
    pub todo: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DistributionModel {
    SharedScheduling,
    Pipeline,
    DataParallel,
    TaskGraph,
    Federated,
}

impl DistributionModel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SharedScheduling => "shared-scheduling",
            Self::Pipeline => "pipeline",
            Self::DataParallel => "data-parallel",
            Self::TaskGraph => "task-graph",
            Self::Federated => "federated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionDomain {
    pub id: String,
    pub members: Vec<String>,
    pub distribution: DistributionModel,
    pub state: TopologyState,
    pub todo: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CoordinationModel {
    Local,
    MessagePassing,
    EventDriven,
    Federated,
}

impl CoordinationModel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::MessagePassing => "message-passing",
            Self::EventDriven => "event-driven",
            Self::Federated => "federated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputeFabric {
    pub id: String,
    pub domains: Vec<String>,
    pub coordination: CoordinationModel,
    pub state: TopologyState,
    pub todo: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InfrastructureTopology {
    pub nodes: Vec<ResourceNode>,
    pub links: Vec<ResourceLink>,
    pub domains: Vec<ExecutionDomain>,
    pub fabrics: Vec<ComputeFabric>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopologyError {
    EmptyId { layer: &'static str },
    DuplicateId { layer: &'static str, id: String },
    MissingNode { owner: String, node: String },
    SelfLink { link: String, node: String },
    EmptyMembers { layer: &'static str, id: String },
    DuplicateMember { owner: String, member: String },
    MissingDomain { fabric: String, domain: String },
}

impl fmt::Display for TopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId { layer } => write!(f, "{layer} contains an empty id"),
            Self::DuplicateId { layer, id } => write!(f, "duplicate {layer} id: {id}"),
            Self::MissingNode { owner, node } => {
                write!(f, "{owner} references missing node {node}")
            }
            Self::SelfLink { link, node } => write!(f, "link {link} loops to node {node}"),
            Self::EmptyMembers { layer, id } => write!(f, "{layer} {id} has no members"),
            Self::DuplicateMember { owner, member } => {
                write!(f, "{owner} repeats member {member}")
            }
            Self::MissingDomain { fabric, domain } => {
                write!(f, "fabric {fabric} references missing domain {domain}")
            }
        }
    }
}

impl std::error::Error for TopologyError {}

impl InfrastructureTopology {
    pub fn validate(&self) -> Result<(), TopologyError> {
        let node_ids = unique_ids("node", self.nodes.iter().map(|node| node.id.as_str()))?;
        let link_ids = unique_ids("link", self.links.iter().map(|link| link.id.as_str()))?;
        let domain_ids = unique_ids(
            "domain",
            self.domains.iter().map(|domain| domain.id.as_str()),
        )?;
        let _fabric_ids = unique_ids(
            "fabric",
            self.fabrics.iter().map(|fabric| fabric.id.as_str()),
        )?;
        debug_assert_eq!(link_ids.len(), self.links.len());

        for link in &self.links {
            if link.from == link.to {
                return Err(TopologyError::SelfLink {
                    link: link.id.clone(),
                    node: link.from.clone(),
                });
            }
            for endpoint in [&link.from, &link.to] {
                if !node_ids.contains(endpoint.as_str()) {
                    return Err(TopologyError::MissingNode {
                        owner: format!("link {}", link.id),
                        node: endpoint.clone(),
                    });
                }
            }
        }

        for domain in &self.domains {
            validate_members("domain", &domain.id, &domain.members)?;
            for member in &domain.members {
                if !node_ids.contains(member.as_str()) {
                    return Err(TopologyError::MissingNode {
                        owner: format!("domain {}", domain.id),
                        node: member.clone(),
                    });
                }
            }
        }

        for fabric in &self.fabrics {
            validate_members("fabric", &fabric.id, &fabric.domains)?;
            for domain in &fabric.domains {
                if !domain_ids.contains(domain.as_str()) {
                    return Err(TopologyError::MissingDomain {
                        fabric: fabric.id.clone(),
                        domain: domain.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn unique_ids<'a>(
    layer: &'static str,
    ids: impl Iterator<Item = &'a str>,
) -> Result<HashSet<&'a str>, TopologyError> {
    let mut unique = HashSet::new();
    for id in ids {
        if id.is_empty() {
            return Err(TopologyError::EmptyId { layer });
        }
        if !unique.insert(id) {
            return Err(TopologyError::DuplicateId {
                layer,
                id: id.to_owned(),
            });
        }
    }
    Ok(unique)
}

fn validate_members(
    layer: &'static str,
    id: &str,
    members: &[String],
) -> Result<(), TopologyError> {
    if members.is_empty() {
        return Err(TopologyError::EmptyMembers {
            layer,
            id: id.to_owned(),
        });
    }
    let mut unique = HashSet::new();
    for member in members {
        if !unique.insert(member) {
            return Err(TopologyError::DuplicateMember {
                owner: format!("{layer} {id}"),
                member: member.clone(),
            });
        }
    }
    Ok(())
}

/// Returns a conceptual topology used by the lab and contract tests.
///
/// Research nodes and links are placeholders, not detected host capabilities.
pub fn prefilled_topology() -> InfrastructureTopology {
    use ResourceKind::{Cpu, Gpu, Host, Lpu, Microcontroller, Npu};
    use TopologyState::{Declared, Research};

    let node = |id: &str, kind, state, todo: Option<&str>| ResourceNode {
        id: id.to_owned(),
        kind,
        state,
        todo: todo.map(str::to_owned),
    };
    InfrastructureTopology {
        nodes: vec![
            node("host", Host, Declared, None),
            node("cpu", Cpu, Declared, None),
            node("gpu", Gpu, Research, Some("WM-GPU")),
            node("npu", Npu, Research, Some("WM-NPU")),
            node("lpu", Lpu, Research, Some("WM-LPU")),
            node(
                "esp32",
                Microcontroller,
                Research,
                Some("WM-ESP32-TRANSPORT"),
            ),
        ],
        links: vec![
            ResourceLink {
                id: "host-cpu".to_owned(),
                from: "host".to_owned(),
                to: "cpu".to_owned(),
                transport: TransportClass::InProcess,
                direction: LinkDirection::Bidirectional,
                state: Declared,
                todo: None,
            },
            ResourceLink {
                id: "cpu-gpu".to_owned(),
                from: "cpu".to_owned(),
                to: "gpu".to_owned(),
                transport: TransportClass::AcceleratorInterconnect,
                direction: LinkDirection::Bidirectional,
                state: Research,
                // TODO(WM-TOPOLOGY-ACCEL): replace conceptual links with discovered topology.
                todo: Some("WM-TOPOLOGY-ACCEL".to_owned()),
            },
            ResourceLink {
                id: "cpu-npu".to_owned(),
                from: "cpu".to_owned(),
                to: "npu".to_owned(),
                transport: TransportClass::AcceleratorInterconnect,
                direction: LinkDirection::Bidirectional,
                state: Research,
                todo: Some("WM-TOPOLOGY-ACCEL".to_owned()),
            },
            ResourceLink {
                id: "cpu-lpu".to_owned(),
                from: "cpu".to_owned(),
                to: "lpu".to_owned(),
                transport: TransportClass::AcceleratorInterconnect,
                direction: LinkDirection::Bidirectional,
                state: Research,
                todo: Some("WM-TOPOLOGY-ACCEL".to_owned()),
            },
            ResourceLink {
                id: "cpu-esp32".to_owned(),
                from: "cpu".to_owned(),
                to: "esp32".to_owned(),
                transport: TransportClass::Peripheral,
                direction: LinkDirection::Bidirectional,
                state: Research,
                // TODO(WM-TOPOLOGY-DEVICE): bind USB/JTAG/UART discovery to this link.
                todo: Some("WM-TOPOLOGY-DEVICE".to_owned()),
            },
        ],
        domains: vec![
            ExecutionDomain {
                id: "local-compute".to_owned(),
                members: ["host", "cpu", "gpu", "npu", "lpu"]
                    .map(str::to_owned)
                    .to_vec(),
                distribution: DistributionModel::TaskGraph,
                state: Research,
                todo: Some("WM-TOPOLOGY-SCHEDULER".to_owned()),
            },
            ExecutionDomain {
                id: "edge-control".to_owned(),
                members: ["cpu", "esp32"].map(str::to_owned).to_vec(),
                distribution: DistributionModel::Pipeline,
                state: Research,
                todo: Some("WM-TOPOLOGY-EDGE".to_owned()),
            },
        ],
        fabrics: vec![ComputeFabric {
            id: "wbox-fabric".to_owned(),
            domains: ["local-compute", "edge-control"]
                .map(str::to_owned)
                .to_vec(),
            coordination: CoordinationModel::Federated,
            state: Research,
            // TODO(WM-FABRIC): define placement, failure-domain, and consistency policy.
            todo: Some("WM-FABRIC".to_owned()),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefilled_point_line_plane_fabric_topology_is_valid() {
        let topology = prefilled_topology();
        topology.validate().unwrap();
        assert_eq!(topology.nodes.len(), 6);
        assert_eq!(topology.links.len(), 5);
        assert_eq!(topology.domains.len(), 2);
        assert_eq!(topology.fabrics.len(), 1);
    }

    #[test]
    fn validation_rejects_dangling_links() {
        let mut topology = prefilled_topology();
        topology.links[0].to = "missing".to_owned();
        assert!(matches!(
            topology.validate(),
            Err(TopologyError::MissingNode { .. })
        ));
    }

    #[test]
    fn validation_rejects_duplicate_domain_members() {
        let mut topology = prefilled_topology();
        topology.domains[0].members.push("cpu".to_owned());
        assert!(matches!(
            topology.validate(),
            Err(TopologyError::DuplicateMember { .. })
        ));
    }

    #[test]
    fn validation_rejects_dangling_fabric_domains() {
        let mut topology = prefilled_topology();
        topology.fabrics[0].domains.push("missing".to_owned());
        assert!(matches!(
            topology.validate(),
            Err(TopologyError::MissingDomain { .. })
        ));
    }
}
