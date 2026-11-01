use super::*;
use serde_json::json;

#[test]
fn codex_windows_follow_duration_not_position_and_skip_other_buckets() {
    let raw = json!({"primary":{"used_percent":61,"window_minutes":10080,"resets_at":2000000000},"secondary":{"used_percent":32,"window_minutes":300,"resets_at":2000000001}});
    let parsed: CodexLimits = serde_json::from_value(raw.clone()).unwrap();
    let snapshot = parsed.snapshot(1900000000);
    assert_eq!(snapshot.five_hour.unwrap().used_percent, 32.);
    assert_eq!(snapshot.weekly.unwrap().used_percent, 61.);
    for raw in [
        json!({"limit_id":42}),
        json!({"limit_id":{"unexpected":[1,2,3]}}),
        json!(null),
        json!([]),
        json!(42),
        json!({"primary":{"used_percent":{"bad":[1,2]},"window_minutes":300,"resets_at":2000000000}}),
    ] {
        assert!(
            serde_json::from_value::<CodexLimits>(raw)
                .unwrap()
                .snapshot(1900000000)
                .is_empty()
        );
    }
    let mut other = raw;
    other["limit_id"] = json!("code_review");
    assert!(
        serde_json::from_value::<CodexLimits>(other)
            .unwrap()
            .snapshot(1900000000)
            .is_empty()
    );
}

#[test]
fn observations_do_not_regress_and_missing_windows_are_not_zero() {
    let window = |used, observed| {
        Some(QuotaWindow {
            used_percent: used,
            resets_at: 2000000000,
            observed_at: observed,
        })
    };
    let mut snapshot = QuotaSnapshot {
        five_hour: window(40., 100),
        weekly: window(60., 100),
    };
    snapshot.merge(QuotaSnapshot {
        five_hour: window(20., 99),
        weekly: None,
    });
    assert_eq!(snapshot.five_hour.unwrap().used_percent, 40.);
    assert_eq!(snapshot.weekly.unwrap().used_percent, 60.);
    assert!(snapshot.five_hour.unwrap().expired(2000000000));
    assert!(wire::window(Some(101.), Some(2000000000.), 100).is_none());
    assert!(wire::window(Some(f64::NAN), Some(2000000000.), 100).is_none());
}

#[test]
fn latest_launch_fences_historical_observations_and_profile_storage_is_bounded() {
    let limits = Limits::new("/app".into());
    limits.register("codex", "/scratch/codex".into(), None);
    let old = QuotaSnapshot {
        five_hour: Some(QuotaWindow {
            used_percent: 40.,
            resets_at: now() + 1000,
            observed_at: now() - 100,
        }),
        weekly: None,
    };
    limits.observe_codex(Path::new("/scratch/codex"), old);
    assert!(limits.snapshot()[0].snapshot.is_empty());
    let fresh = QuotaSnapshot {
        five_hour: Some(QuotaWindow {
            observed_at: now() + 1,
            ..old.five_hour.unwrap()
        }),
        weekly: None,
    };
    limits.observe_codex(Path::new("/scratch/codex"), fresh);
    assert_eq!(limits.snapshot()[0].snapshot, fresh);
    limits.register("codex", "/scratch/codex".into(), None);
    assert!(limits.snapshot()[0].snapshot.is_empty());
    for i in 0..100 {
        limits.register("codex", format!("/scratch/{i}").into(), None);
    }
    assert_eq!(limits.snapshot().len(), 16);
}

#[test]
fn on_demand_reads_are_coalesced_cached_and_do_not_let_old_sessions_override_them() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let scratch = claude::Source::create().unwrap();
    let root = scratch.path().join("home/.codex");
    fs::create_dir_all(&root).unwrap();
    let program = scratch.path().join("codex");
    let calls = scratch.path().join("calls");
    fs::write(&program, format!(r#"#!/bin/sh
printf 'probe\n' >> '{}'
while IFS= read -r line; do
 case "$line" in
 *'"id":1'*) printf '%s\n' '{{"id":1,"result":{{}}}}';;
 *'"id":2'*) printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt"}}}}}}';;
 *'"id":3'*) printf '%s\n' '{{"id":3,"result":{{"rateLimits":{{"primary":{{"usedPercent":23,"windowDurationMins":300,"resetsAt":2000000000}}}}}}}}';;
 esac
done
"#, calls.display())).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let env =
        crate::PathEnv::from_lookup("/usr/bin:/bin".into(), None, &[("codex", Some(program))]);
    let mut core = Core::open_with(scratch.path().join("data"), env).unwrap();
    core.cli_home = Some(CliHome::at(scratch.path().join("home")));
    core.start_subscription_limits("/unused-helper".into());
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                core.subscription_limits(true);
            });
        }
    });
    let quotas = core.subscription_limits(true);
    assert_eq!(quotas[0].snapshot.five_hour.unwrap().used_percent, 23.);
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 1);
    let limits = core.limits.get().unwrap();
    limits.observe_codex(
        &root,
        QuotaSnapshot {
            five_hour: Some(QuotaWindow {
                used_percent: 99.,
                resets_at: 2000000000,
                observed_at: now() + 100,
            }),
            weekly: None,
        },
    );
    assert_eq!(
        core.subscription_limits(false)[0]
            .snapshot
            .five_hour
            .unwrap()
            .used_percent,
        23.
    );
    limits.register("codex", profile_root(Path::new("/"), &root), None);
    // Launch invalidates the snapshot, but cannot bypass the per-root minute floor.
    assert!(core.subscription_limits(true)[0].snapshot.is_empty());
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 1);
    core.stop_subscription_limits();
}
