use serde_json::json;
use tauri_app_lib::server::state::{uptime_secs, RateLimitInfo};

#[test]
fn rate_limit_info_parses_cli_snapshot() {
    let info = RateLimitInfo::from_value(&json!({
        "status": "allowed",
        "rateLimitType": "five_hour",
        "resetsAt": 1781022000_i64,
        "overageStatus": "allowed",
        "overageResetsAt": 1782864000_i64,
        "isUsingOverage": false
    }))
    .expect("parses object");

    assert_eq!(info.status.as_deref(), Some("allowed"));
    assert_eq!(info.rate_limit_type.as_deref(), Some("five_hour"));
    assert_eq!(info.resets_at, Some(1781022000));
    assert_eq!(info.overage_resets_at, Some(1782864000));
    assert_eq!(info.is_using_overage, Some(false));
    assert!(info.captured_at > 0);
}

#[test]
fn rate_limit_info_rejects_non_object() {
    assert!(RateLimitInfo::from_value(&json!("nope")).is_none());
    assert!(RateLimitInfo::from_value(&json!(null)).is_none());
}

#[test]
fn uptime_is_floored_difference_in_seconds() {
    assert_eq!(uptime_secs(10_000, 25_500), 15);
    assert_eq!(uptime_secs(10_000, 10_000), 0);
    // never negative if clock skews backwards
    assert_eq!(uptime_secs(20_000, 10_000), 0);
}

#[tokio::test]
async fn store_rate_limit_persists_and_returns_parsed_info() {
    let slot = tokio::sync::Mutex::new(None);
    let info = tauri_app_lib::server::state::store_rate_limit(
        &slot,
        None,
        json!({ "rateLimitType": "five_hour", "resetsAt": 1781022000_i64 }),
    )
    .await;

    assert_eq!(info.unwrap().rate_limit_type.as_deref(), Some("five_hour"));
    assert!(slot.lock().await.is_some());
}

#[tokio::test]
async fn store_rate_limit_ignores_non_object() {
    let slot = tokio::sync::Mutex::new(None);
    let info = tauri_app_lib::server::state::store_rate_limit(&slot, None, json!("nope")).await;
    assert!(info.is_none());
    assert!(slot.lock().await.is_none());
}
