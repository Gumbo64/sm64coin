//! Peer plumbing for the service: the relay controller's probe peers and `GetNodes` gossip,
//! both driven by a [`Client`]'s node list.
use std::net::IpAddr;
use std::sync::Arc;

use crate::client::{Client, Connector, Transport};
use crate::nodes::NodeAddr;
use crate::proto::{Request, Response};
use crate::relay::PeerSource;
use crate::server::LcService;

/// Probe peers = the iroh nodes in the client's list (never ourselves).
pub struct ClientPeers {
    pub client: Arc<Client>,
    pub connector: Arc<dyn Connector>,
    pub me: String,
}

#[async_trait::async_trait]
impl PeerSource for ClientPeers {
    async fn peers(&self) -> Vec<(String, Option<IpAddr>, Arc<dyn Transport>)> {
        let addrs: Vec<NodeAddr> = self.client.with_list(|l| l.records().iter().map(|r| r.addr.clone()).collect());
        addrs
            .into_iter()
            .filter_map(|a| match &a {
                // the peer's address comes from the connection the probe uses (Transport::remote_ip), never from what the peer
                // announced about itself
                NodeAddr::Iroh { endpoint_id, .. } if *endpoint_id != self.me => Some((endpoint_id.clone(), None, self.connector.transport(&a)?)),
                _ => None,
            })
            .collect()
    }
}

/// Asks up to `rounds` nodes for their node lists; learns the verified announcements into the
/// client's list and the service's known set. Returns how many new nodes were learned.
pub async fn gossip_once(client: &Client, service: &LcService, rounds: usize) -> usize {
    let mut learned = 0;
    for _ in 0..rounds {
        match client.read(&Request::GetNodes { max: crate::proto::MAX_NODES }, crate::client::READ_TIMEOUT).await {
            Ok((_, Response::Nodes(v))) => {
                learned += client.learn(&v);
                for a in v {
                    service.add_known(a);
                }
            }
            Err(crate::client::ClientError::NoNodes) => break,
            Ok(_) | Err(_) => {}
        }
    }
    learned
}
