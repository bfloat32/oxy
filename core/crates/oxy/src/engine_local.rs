use oxy_core::engine::{Engine, EngineCmd, EngineEvent};
use oxy_core::settings::paths as dirs;
use tokio::sync::mpsc;

/// An engine running in this process: the same pipeline the daemon hosts,
/// with the command channel fed by the command line.
pub(crate) async fn local_engine() -> (
    mpsc::Sender<EngineCmd>,
    mpsc::Receiver<EngineEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (evt_tx, evt_rx) = mpsc::channel(512);
    let (worker_tx, worker_rx) = mpsc::channel(512);
    let engine = Engine::start(
        &dirs::extensions_dir(),
        cmd_rx,
        evt_tx,
        worker_tx,
        oxy_core::provider::native::construct,
    )
    .await;
    let task = tokio::spawn(async move { engine.run(worker_rx).await });
    (cmd_tx, evt_rx, task)
}
