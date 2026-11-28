use super::*;

const WAIT: Duration = Duration::from_secs(5);

struct BlockedObservation {
    release: mpsc::SyncSender<()>,
    receive: mpsc::Receiver<Reply<()>>,
}

fn block_observation(desktop: &Desktop) -> BlockedObservation {
    let (started, observed) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let (receive, _) = desktop
        .enqueue(move |_worker, _authority| {
            started.send(()).map_err(|err| err.to_string())?;
            wait.recv_timeout(WAIT).map_err(|err| err.to_string())?;
            Ok(())
        })
        .unwrap();
    observed.recv_timeout(WAIT).unwrap();
    BlockedObservation { release, receive }
}

impl BlockedObservation {
    fn finish(self) {
        self.release.send(()).unwrap();
        self.receive.recv_timeout(WAIT).unwrap().unwrap();
    }
}

fn scoped(desktop: &Desktop) -> Desktop {
    Desktop {
        sender: desktop.sender.clone(),
        permit: OperationPermit {
            generation: Arc::new(AtomicU64::new(3)),
            expected: 3,
            active: Arc::new(AtomicBool::new(true)),
        },
    }
}

#[test]
fn queued_mutation_never_runs_after_epoch_revocation() {
    let worker = Desktop::new();
    let desktop = scoped(&worker);
    let blocked = block_observation(&worker);
    let mutated = Arc::new(AtomicBool::new(false));
    let delivered = mutated.clone();
    let (receive, _) = desktop
        .enqueue(move |_worker, _authority| {
            delivered.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap();
    desktop.permit.generation.fetch_add(1, Ordering::AcqRel);
    blocked.finish();
    assert!(receive.recv_timeout(WAIT).unwrap().is_err());
    worker.call(|_worker, _authority| Ok(())).unwrap();
    assert!(!mutated.load(Ordering::Acquire));
}

#[test]
fn queued_mutation_never_runs_after_caller_lifetime_ends() {
    let worker = Desktop::new();
    let desktop = scoped(&worker);
    let blocked = block_observation(&worker);
    let mutated = Arc::new(AtomicBool::new(false));
    let delivered = mutated.clone();
    let (receive, _) = desktop
        .enqueue(move |_worker, _authority| {
            delivered.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap();
    desktop.permit.active.store(false, Ordering::Release);
    blocked.finish();
    assert!(receive.recv_timeout(WAIT).unwrap().is_err());
    worker.call(|_worker, _authority| Ok(())).unwrap();
    assert!(!mutated.load(Ordering::Acquire));
}

#[test]
fn real_response_timeout_cancels_a_queued_mutation() {
    let desktop = Desktop::new();
    let blocked = block_observation(&desktop);
    let mutated = Arc::new(AtomicBool::new(false));
    let delivered = mutated.clone();
    let result =
        desktop.call_with_timeout(Duration::from_millis(50), move |_worker, _authority| {
            delivered.store(true, Ordering::Release);
            Ok(())
        });
    assert!(result.unwrap_err().contains("DeliveryUncertain"));
    assert!(!mutated.load(Ordering::Acquire));
    blocked.finish();
    desktop.call(|_worker, _authority| Ok(())).unwrap();
    assert!(!mutated.load(Ordering::Acquire));
}

#[test]
fn queued_job_retains_entry_after_original_control_is_dropped() {
    let desktop = Desktop::new();
    let blocked = block_observation(&desktop);
    let entry = Arc::new(ad::RefEntry {
        process: ad::RefProcess {
            pid: ad::ProcessId::new(1),
            process_instance: Some("test-process".into()),
        },
        identity: ad::RefEntryIdentity {
            retained_object: Some("retained-native-object".into()),
            role: "button".into(),
            name: Some("Original target".into()),
            value: None,
            description: None,
            native_id: None,
        },
        geometry: ad::RefGeometry {
            bounds: None,
            bounds_hash: None,
            bounds_space: None,
        },
        capabilities: ad::RefCapabilities {
            states: Vec::new(),
            available_actions: vec!["Click".into()],
        },
        source: ad::RefSource {
            source_app: Some("test-app".into()),
            source_window_id: Some("w-1".into()),
            source_window_title: Some("test-window".into()),
            source_window_bounds_hash: None,
            source_surface: ad::SnapshotSurface::Window,
        },
        scope: ad::RefScope {
            root_ref: None,
            path_is_absolute: false,
            path: Default::default(),
        },
    });
    let weak = Arc::downgrade(&entry);
    let control = NativeControl {
        sender: desktop.sender.clone(),
        epoch: desktop.epoch(),
        entry: Some(entry.clone()),
        bounds: None,
    };
    let retained = control.entry.clone().unwrap();
    let (receive, _) = desktop
        .enqueue(move |_worker, _authority| Ok(retained.identity.retained_object.clone()))
        .unwrap();
    drop(control);
    drop(entry);
    assert_eq!(weak.strong_count(), 1);
    blocked.finish();
    assert_eq!(
        receive.recv_timeout(WAIT).unwrap().unwrap().as_deref(),
        Some("retained-native-object")
    );
    desktop.call(|_worker, _authority| Ok(())).unwrap();
    assert!(weak.upgrade().is_none());
}

#[test]
fn native_guard_rechecks_revocation_after_a_blocked_pre_dispatch_phase() {
    let desktop = Desktop::new();
    let (started, observed) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let (receive, _) = desktop
        .enqueue(move |worker, authority| {
            let adapter = guard::Guarded {
                native: &worker.native,
                authority,
            };
            let lease = ad::InteractionLease::guarded(ad::Deadline::after(5000).unwrap(), ())
                .map_err(error)?;
            started.send(()).map_err(|err| err.to_string())?;
            wait.recv_timeout(WAIT).map_err(|err| err.to_string())?;
            let result = ad::ActionOps::execute_action(
                &adapter,
                &ad::NativeHandle::null(),
                ActionRequest::headed(Action::Click),
                &lease,
            );
            let err = result.unwrap_err();
            Ok((err.code, err.disposition))
        })
        .unwrap();
    observed.recv_timeout(WAIT).unwrap();
    desktop.permit.generation.fetch_add(1, Ordering::AcqRel);
    release.send(()).unwrap();
    let (code, disposition) = receive.recv_timeout(WAIT).unwrap().unwrap();
    assert_eq!(code, ad::ErrorCode::PolicyDenied);
    assert_eq!(disposition, ad::DeliverySemantics::NotDelivered);
}

#[test]
fn stopped_app_launches_once_and_refreshes_its_windows() {
    let mut lists = 0;
    let mut launches = 0;
    let windows = windows_for_open(
        || {
            lists += 1;
            if lists == 1 {
                Err(Box::new(ad::AdapterError::new(
                    ad::ErrorCode::AppNotFound,
                    "stopped",
                )))
            } else {
                Ok(vec![42])
            }
        },
        || {
            launches += 1;
            Ok(None)
        },
    )
    .unwrap();
    assert_eq!((windows, lists, launches), (vec![42], 2, 1));
}

#[test]
fn a_window_the_launch_produced_is_used_without_listing_again() {
    let mut lists = 0;
    let windows = windows_for_open(
        || {
            lists += 1;
            Ok(Vec::new())
        },
        || Ok(Some(7)),
    )
    .unwrap();
    assert_eq!((windows, lists), (vec![7], 1));
}

#[test]
fn existing_window_does_not_launch_again() {
    let windows = windows_for_open(|| Ok(vec![42]), || panic!("already running")).unwrap();
    assert_eq!(windows, vec![42]);
}

#[test]
fn initial_inventory_errors_do_not_launch() {
    for code in [
        ad::ErrorCode::PermDenied,
        ad::ErrorCode::PolicyDenied,
        ad::ErrorCode::Timeout,
        ad::ErrorCode::Internal,
    ] {
        let result = windows_for_open::<()>(
            || {
                Err(Box::new(ad::AdapterError::new(
                    code.clone(),
                    "inventory failed",
                )))
            },
            || panic!("inventory failure must not authorize launch"),
        );
        assert_eq!(result.unwrap_err().code, code);
    }
}

#[test]
fn post_launch_inventory_error_does_not_relaunch() {
    let mut launches = 0;
    let result = windows_for_open::<()>(
        || {
            Err(Box::new(ad::AdapterError::new(
                ad::ErrorCode::AppNotFound,
                "not found",
            )))
        },
        || {
            launches += 1;
            Ok(None)
        },
    );
    assert_eq!(result.unwrap_err().code, ad::ErrorCode::AppNotFound);
    assert_eq!(launches, 1);
}

#[test]
fn rejected_launch_does_not_refresh_inventory() {
    let mut lists = 0;
    let result = windows_for_open::<()>(
        || {
            lists += 1;
            Ok(Vec::new())
        },
        || Err(Box::new(revoked())),
    );
    assert_eq!(result.unwrap_err().code, ad::ErrorCode::PolicyDenied);
    assert_eq!(lists, 1);
}
