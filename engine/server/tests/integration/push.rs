use super::*;

#[tokio::test]
async fn push_subscribe_list_unsubscribe() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // VAPID public key is reachable without auth and non-empty.
    let pk: Value = reqwest::get(format!("{base}/api/push/public-key"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!pk["vapid_public_key"].as_str().unwrap_or("").is_empty());

    // Subscribe a fake FCM token.
    let r: Value = client
        .post(format!("{base}/api/push/subscribe"))
        .json(&json!({
            "kind": "fcm",
            "token": "fake-test-token-12345",
            "label": "test-device",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = r["id"].as_str().unwrap().to_string();

    let list: Vec<Value> = client
        .get(format!("{base}/api/push/list"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().any(|s| s["id"] == id));

    // Re-subscribing the same token is idempotent (same id).
    let r2: Value = client
        .post(format!("{base}/api/push/subscribe"))
        .json(&json!({"kind":"fcm","token":"fake-test-token-12345"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r2["id"], r["id"]);

    let r: Value = client
        .post(format!("{base}/api/push/unsubscribe"))
        .json(&json!({"id": id}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["ok"], true);
}

#[tokio::test]
async fn push_preferences_and_quiet_hour_digest_queue_are_exposed() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let r: Value = client
        .post(format!("{base}/api/push/subscribe"))
        .json(&json!({
            "kind": "fcm",
            "token": "fake-digest-token-12345",
            "label": "quiet-device",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = r["id"].as_str().unwrap().to_string();

    let now = OffsetDateTime::now_utc();
    let minute = (u16::from(now.hour()) * 60) + u16::from(now.minute());
    let start = minute.saturating_sub(1);
    let end = (minute + 2) % (24 * 60);

    let updated: Value = client
        .post(format!("{base}/api/push/update"))
        .json(&json!({
            "id": id,
            "prefs": {
                "waiting_for_input": true,
                "agent_task_started": false,
                "agent_task_notify": false,
                "quiet_hours": {
                    "enabled": true,
                    "start_minute": start,
                    "end_minute": end,
                    "utc_offset_minutes": 0,
                    "digest": true
                }
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["ok"], true);
    assert_eq!(updated["prefs"]["quiet_hours"]["enabled"], true);

    let queued: Value = client
        .post(format!("{base}/api/push/test"))
        .json(&json!({"title": "Queued test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queued["ok"], 0);
    assert_eq!(queued["fail"], 0);
    assert_eq!(queued["queued"], 1);

    let list: Vec<Value> = client
        .get(format!("{base}/api/push/list"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entry = list
        .iter()
        .find(|sub| sub["id"] == id)
        .expect("subscription listed");
    assert_eq!(entry["pending_digest_count"], 1);

    let flush: Value = client
        .post(format!("{base}/api/push/flush-digests"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(flush["ok"], 0);
    assert_eq!(flush["fail"], 0);

    let _updated: Value = client
        .post(format!("{base}/api/push/update"))
        .json(&json!({
            "id": id,
            "prefs": {
                "waiting_for_input": true,
                "agent_task_started": false,
                "agent_task_notify": false,
                "quiet_hours": {
                    "enabled": false,
                    "start_minute": start,
                    "end_minute": end,
                    "utc_offset_minutes": 0,
                    "digest": true
                }
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let flush_after_disable: Value = client
        .post(format!("{base}/api/push/flush-digests"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(flush_after_disable["ok"], 0);
    assert_eq!(flush_after_disable["fail"], 1);

    let r: Value = client
        .post(format!("{base}/api/push/unsubscribe"))
        .json(&json!({"id": id}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["ok"], true);
}
