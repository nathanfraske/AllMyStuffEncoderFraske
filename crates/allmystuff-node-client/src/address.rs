//! Existing socket addressing shared by clients and the node's listener.
//!
//! These helpers do not bind, remove sockets or set access permissions.

use interprocess::local_socket::tokio::prelude::*;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;

use crate::error::Error;

/// The node's local address, distinct from the MyOwnMesh daemon socket.
pub enum SocketAddr {
    #[cfg(unix)]
    Path(std::path::PathBuf),
    #[cfg(not(unix))]
    Name(String),
}

#[cfg(not(unix))]
const NODE_SOCKET_NAME: &str = "allmystuff-node";

#[cfg(unix)]
pub(crate) const NAME_CONTEXT: &str = "node socket path → fs_name";
#[cfg(not(unix))]
pub(crate) const NAME_CONTEXT: &str = "node socket name → ns_name";

/// Resolve the current node address with the node's original error context.
pub fn node_socket_addr() -> anyhow::Result<SocketAddr> {
    resolve().map_err(Error::into_node)
}

pub(crate) fn resolve() -> Result<SocketAddr, Error> {
    #[cfg(unix)]
    {
        let home = allmystuff_protocol::myownmesh_state_dir().ok_or(Error::ResolveHome)?;
        Ok(SocketAddr::Path(home.join("allmystuff-node.sock")))
    }
    #[cfg(not(unix))]
    {
        Ok(SocketAddr::Name(NODE_SOCKET_NAME.to_string()))
    }
}

impl SocketAddr {
    /// Convert for connect/bind with the node's original error context.
    pub fn to_name(&self) -> anyhow::Result<interprocess::local_socket::Name<'_>> {
        self.raw_name()
            .map_err(Error::SocketName)
            .map_err(Error::into_node)
    }

    pub(crate) fn raw_name(&self) -> std::io::Result<interprocess::local_socket::Name<'_>> {
        match self {
            #[cfg(unix)]
            Self::Path(p) => p.as_path().to_fs_name::<GenericFilePath>(),
            #[cfg(not(unix))]
            Self::Name(n) => n.as_str().to_ns_name::<GenericNamespaced>(),
        }
    }

    /// The Unix socket path; cleanup and permission changes belong to the host.
    #[cfg(unix)]
    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::Path(p) => p.as_path(),
        }
    }
}
