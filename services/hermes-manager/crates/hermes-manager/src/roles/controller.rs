//! The `controller` role: one leader at a time reconciles every instance.
//!
//! A term starts when this process wins the Lease and advances the
//! controller epoch. It ends when renewal fails for longer than the renew
//! deadline, another candidate holds the Lease, or a status write finds a
//! newer epoch. Nothing is carried from one term to the next: the queue is
//! rebuilt from PostgreSQL.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hermes_core::app::controller::queue::{Outcome, WorkQueue};
use hermes_core::app::controller::{Controller, ControllerError, InstanceStore};
use hermes_core::app::error::AppError;
use hermes_core::app::execution::ExecutionStore;
use hermes_core::domain::ErrorKind;
use hermes_kube::{KubeCluster, LeaderElection, RENEW_DEADLINE, RETRY_PERIOD};
use hermes_native::materials::ControllerVault;
use hermes_native::probe::{NativeChecks, NativeProbe};
use hermes_store::PgStore;
use tokio::sync::watch;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::config::Config;
use crate::faults;

/// How often due leases are expired (database time).
const LEASE_EXPIRY: Duration = Duration::from_secs(5);
/// How often owned namespaces and volumes are compared with the registry.
const ORPHAN_SCAN: Duration = Duration::from_secs(60);

type Reconciler = Controller<PgStore, KubeCluster, ControllerVault, NativeChecks>;

enum End {
    Shutdown,
    Lost,
}

pub async fn run(config: Config) -> Result<(), AppError> {
    let section = config.controller.as_ref().ok_or_else(AppError::invalid)?;
    let render = section.render_config(&config).await?;
    let store = config.store().await?;
    store.check(&config.installation).await?;
    let client = section.client().await?;
    // A different cluster blocks the controller until an operator rebinds.
    let identity = hermes_kube::cluster_identity(&client)
        .await
        .map_err(|_| AppError::unavailable())?;
    if identity != store.cluster_uid().await? {
        return Err(AppError::new(ErrorKind::Unavailable, "cluster_changed"));
    }
    let vault = ControllerVault::new(*section.sealing_key().await?);
    let cluster = KubeCluster::new(client.clone(), render).map_err(|_| AppError::invalid())?;
    let controller = Arc::new(Controller {
        store: store.clone(),
        cluster,
        vault,
        native: NativeChecks {
            probe: NativeProbe::new()?,
        },
        settings: config.settings()?,
        faults: faults::hook(),
    });
    // A key that opens none of the stored material would let this controller
    // seal material for new instances that the right key could never open.
    controller
        .verify_vault()
        .await
        .map_err(|error| match error {
            ControllerError::Vault(error) => error,
            _ => AppError::unavailable(),
        })?;
    let candidate = format!(
        "{}-{}",
        std::env::var("HOSTNAME").unwrap_or_else(|_| "controller".into()),
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let leader = LeaderElection::new(client, &section.namespace, &section.lease_name, &candidate);
    let (stop, mut stopped) = watch::channel(false);
    tokio::spawn(async move {
        super::shutdown_signal().await;
        let _ = stop.send(true);
    });
    let resync = Duration::from_secs(section.resync_seconds);
    loop {
        // Campaign until leading or stopped.
        loop {
            if *stopped.borrow() {
                return Ok(());
            }
            if matches!(leader.try_lead().await, Ok(true)) {
                break;
            }
            tokio::select! {
                _ = stopped.changed() => {}
                _ = tokio::time::sleep(RETRY_PERIOD) => {}
            }
        }
        let epoch = store.advance_epoch().await.map_err(AppError::from)?;
        eprintln!("hermes-manager controller: leading, epoch {epoch}");
        match term(
            &controller,
            &store,
            &leader,
            epoch,
            section.workers,
            resync,
            &mut stopped,
        )
        .await
        {
            End::Shutdown => {
                let _ = leader.release().await;
                return Ok(());
            }
            End::Lost => eprintln!("hermes-manager controller: leadership lost"),
        }
    }
}

async fn term(
    controller: &Arc<Reconciler>,
    store: &PgStore,
    leader: &LeaderElection,
    epoch: i64,
    workers: usize,
    resync: Duration,
    stopped: &mut watch::Receiver<bool>,
) -> End {
    let mut queue = WorkQueue::default();
    let mut notifications = store.notifications().await.ok();
    let mut running: JoinSet<(Uuid, Result<(), ControllerError>, Option<u64>)> = JoinSet::new();
    let mut renew = tokio::time::interval(RETRY_PERIOD);
    let mut resync = tokio::time::interval(resync);
    let mut expiry = tokio::time::interval(LEASE_EXPIRY);
    let mut orphans = tokio::time::interval(ORPHAN_SCAN);
    let mut renewed = Instant::now();
    loop {
        while running.len() < workers {
            let Some(instance) = queue.take() else { break };
            let controller = controller.clone();
            running.spawn(async move {
                match controller.reconcile(epoch, instance).await {
                    Ok(pass) => (instance, Ok(()), pass.and_then(|p| p.requeue_seconds)),
                    Err(error) => (instance, Err(error), None),
                }
            });
        }
        let due = queue.next_due();
        tokio::select! {
            _ = stopped.changed() => {
                running.abort_all();
                return End::Shutdown;
            }
            _ = renew.tick() => match leader.try_lead().await {
                Ok(true) => renewed = Instant::now(),
                Ok(false) => {
                    running.abort_all();
                    return End::Lost;
                }
                Err(_) if renewed.elapsed() > RENEW_DEADLINE => {
                    running.abort_all();
                    return End::Lost;
                }
                Err(_) => {}
            },
            // Expired leases free their environments for draining.
            _ = expiry.tick() => {
                if store.expire_leases().await.is_err() {
                    eprintln!("hermes-manager controller: registry unavailable");
                }
            }
            // Reported, never adopted: an operator decides what they are.
            _ = orphans.tick() => match controller.report_orphans(epoch).await {
                Ok(0) => {}
                Ok(found) => eprintln!("hermes-manager controller: {found} orphaned objects"),
                Err(ControllerError::Deposed) => {
                    running.abort_all();
                    return End::Lost;
                }
                Err(error) => eprintln!("hermes-manager controller: orphan scan: {error}"),
            },
            _ = resync.tick() => match store.instance_ids().await {
                Ok(ids) => ids.into_iter().for_each(|id| queue.add(id)),
                Err(_) => eprintln!("hermes-manager controller: registry unavailable"),
            },
            notification = async {
                match notifications.as_mut() {
                    Some(listener) => listener.next().await,
                    None => std::future::pending().await,
                }
            } => match notification {
                Ok(Some(instance)) => queue.add(instance),
                Ok(None) => {}
                // The periodic resync covers the gap; reconnect on the next tick.
                Err(_) => notifications = store.notifications().await.ok(),
            },
            Some(finished) = running.join_next() => {
                let Ok((instance, result, requeue)) = finished else { continue };
                let outcome = match result {
                    Ok(()) => Outcome::Requeue(requeue.map(Duration::from_secs)),
                    Err(ControllerError::Deposed) => {
                        running.abort_all();
                        return End::Lost;
                    }
                    Err(error) => {
                        eprintln!("hermes-manager controller: instance {instance}: {error}");
                        Outcome::Failed
                    }
                };
                queue.done(instance, outcome, Instant::now());
            }
            _ = async {
                match due {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending().await,
                }
            } => queue.promote(Instant::now()),
        }
    }
}
