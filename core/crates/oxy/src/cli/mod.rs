pub(crate) mod extensions;
pub(crate) mod manifest;
pub(crate) mod query;
pub(crate) mod send;
pub(crate) mod test;

use oxy_core::settings::paths as dirs;

/// One connection to the daemon, whichever shape the platform gives it.
pub(crate) async fn connect_daemon() -> std::io::Result<interprocess::local_socket::tokio::Stream> {
    use interprocess::local_socket::tokio::prelude::*;

    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        let name = dirs::socket_name().to_fs_name::<GenericFilePath>()?;
        interprocess::local_socket::tokio::Stream::connect(name).await
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        let name = dirs::socket_name().to_ns_name::<GenericNamespaced>()?;
        interprocess::local_socket::tokio::Stream::connect(name).await
    }
}
