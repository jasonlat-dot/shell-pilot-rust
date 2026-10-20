#![cfg(windows)]
mod support;
use shellpilot_server::{error::Error, terminal::TerminalSettings};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use support::*;

#[tokio::test]
async fn resources_share_authenticated_transport_but_not_shell_output_or_other_tabs_cache() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let first = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    let second = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    output_contains(&manager, &first.session_id, "ready").await;
    output_contains(&manager, &second.session_id, "ready").await;
    let (one, cached) = tokio::join!(
        manager.resources(&first.session_id),
        manager.resources(&first.session_id)
    );
    let one = one.unwrap();
    assert!(one.supported && !one.stale);
    assert_eq!(one.memory_used_bytes, Some(614400));
    assert_eq!(one.uptime_seconds, Some(1000.0));
    assert_eq!(one.sampled_at_ms, cached.unwrap().sampled_at_ms);
    assert_eq!(fixture.stats.execs.lock().unwrap().len(), 1);
    let other = manager.resources(&second.session_id).await.unwrap();
    assert_eq!(other.cpu_percent, None);
    assert_eq!(fixture.stats.execs.lock().unwrap().len(), 2);
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 1);
    assert!(fixture.stats.inputs.lock().unwrap().is_empty());
    *fixture.stats.resource_output.lock().unwrap() = Some(
        include_str!("fixtures/resources.txt")
            .replace("1000.00 5000.00", "1005.00 5020.00")
            .replace(
                "cpu 100 0 50 750 50 10 10 30",
                "cpu 150 0 50 900 50 10 10 30",
            )
            .replace("eth0: 1000", "eth0: 6000")
            .replace("0 2000 0", "0 12000 0")
            .into_bytes(),
    );
    tokio::time::sleep(Duration::from_millis(2050)).await;
    let next = manager.resources(&first.session_id).await.unwrap();
    assert_eq!(next.cpu_percent, Some(25.0));
    assert_eq!(next.network[0].receive_bytes_per_second, Some(1000.0));
    assert_eq!(next.network[0].transmit_bytes_per_second, Some(2000.0));
    manager
        .write(&first.session_id, "interactive-marker".into())
        .await
        .unwrap();
    let output = manager.read(&first.session_id).await;
    assert!(output.output.contains("interactive-marker"));
    assert!(!output.output.contains("SHELLPILOT_RESOURCES"));
    manager.close(&first.session_id);
    assert!(matches!(
        manager.resources(&first.session_id).await,
        Err(Error::TerminalMissing)
    ));
    assert!(manager.connected(&second.session_id).connected);
    manager.shutdown().await;
}

#[tokio::test]
async fn failed_probe_keeps_last_sample_stale_and_channel_exhaustion_does_not_reconnect() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let terminal = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    let first = manager.resources(&terminal.session_id).await.unwrap();
    *fixture.stats.resource_output.lock().unwrap() = Some(b"invalid probe output".to_vec());
    tokio::time::sleep(Duration::from_millis(2050)).await;
    let failed = manager.resources(&terminal.session_id).await.unwrap();
    assert!(failed.stale);
    assert_eq!(first.sampled_at_ms, failed.sampled_at_ms);
    assert_eq!(first.memory_used_bytes, failed.memory_used_bytes);
    assert!(manager.connected(&terminal.session_id).connected);
    manager.shutdown().await;

    let (manager, connection) = fixture.manager(TerminalSettings {
        channels_per_transport: 1,
        ..TerminalSettings::default()
    });
    let terminal = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    let busy = manager.resources(&terminal.session_id).await.unwrap();
    assert!(busy.stale);
    assert_eq!(busy.cpu_percent, None);
    assert!(manager.connected(&terminal.session_id).connected);
    manager.shutdown().await;
}

#[tokio::test]
async fn cancelling_monitor_closes_only_its_channel_and_releases_the_slot() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let manager = Arc::new(manager);
    let terminal = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    fixture.stats.resource_quiet.store(true, Ordering::SeqCst);
    let running = manager.clone();
    let id = terminal.session_id.clone();
    let task = tokio::spawn(async move { running.resources(&id).await });
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 2).await;
    task.abort();
    let _ = task.await;
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 1).await;
    assert!(manager.connected(&terminal.session_id).connected);
    fixture.stats.resource_quiet.store(false, Ordering::SeqCst);
    assert!(!manager.resources(&terminal.session_id).await.unwrap().stale);
    manager.shutdown().await;
}
