//! Integration test scenarios 1–4.
//!
//! 1. Multi-user — different tokens call the same model
//! 2. Dual-layer billing — user quota + token quota both decrease
//! 3. Group routing — different groups route to different channels
//! 4. Token model_limits — restrict which models a token can access

mod common;

use axum::http::StatusCode;

// ---------------------------------------------------------------------------
// Scenario 1: Multi-user — different tokens call the same model
// ---------------------------------------------------------------------------
//
// token1 (user1, group=default) and token2 (user2, group=premium) both
// request gpt-4o.  token1 routes to channel1 (OpenAI-compatible); token2
// routes to channel2 (Anthropic) with cross-format translation.
// Both should succeed and usage_logs should record the correct user_id.

#[tokio::test]
async fn test_scenario_1_multi_user() {
    // Arrange
    let env = common::setup().await;
    common::mock_openai_ok(&env.mock_openai, 100, 50).await;
    common::mock_claude_ok(&env.mock_claude, 100, 50).await;

    // Act
    let (status1, _body1) = common::send_chat_request(
        &env.app,
        common::TOKEN1,
        common::chat_body("gpt-4o"),
    )
    .await;

    let (status2, _body2) = common::send_chat_request(
        &env.app,
        common::TOKEN2,
        common::chat_body("gpt-4o"),
    )
    .await;

    // Assert
    assert_eq!(status1, StatusCode::OK, "token1 request should succeed");
    assert_eq!(status2, StatusCode::OK, "token2 request should succeed");

    let conn = env.db().await;
    assert_eq!(
        common::count_usage_logs(&conn),
        2,
        "two usage_log rows should exist"
    );

    // Verify each row has the correct user_id.
    let mut stmt = conn
        .prepare("SELECT user_id FROM usage_logs ORDER BY id")
        .unwrap();
    let user_ids: Vec<i64> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        user_ids.contains(&common::USER1_ID),
        "usage_logs should contain user_id={}",
        common::USER1_ID
    );
    assert!(
        user_ids.contains(&common::USER2_ID),
        "usage_logs should contain user_id={}",
        common::USER2_ID
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: Dual-layer billing
// ---------------------------------------------------------------------------
//
// token1 requests gpt-4o with a mock that returns total_tokens=150000.
// Billing math (price = 0.01 元/1K tokens, 内部存储为微元 1 元 = 1_000_000 微元):
//   estimate_cost = (500/1000 * 0.01 + 500/1000 * 0.01) * 1_000_000 = 10000 微元
//   actual_cost   = (100000/1000 * 0.01 + 50000/1000 * 0.01) * 1_000_000
//                 = (1.0 + 0.5) * 1_000_000 = 1_500_000 微元
//   pre_charge deducts 10000 from both user and token layers.
//   settle charges extra 1_490_000 (1_500_000 - 10000) to both layers.
//   Net: user.used_quota = 1_500_000, token.remain_quota = 5_000_000_000 - 1_500_000 = 4_998_500_000.

#[tokio::test]
async fn test_scenario_2_dual_layer_billing() {
    // Arrange
    let env = common::setup().await;
    // Large token counts so actual_cost > 0 with price=0.01.
    common::mock_openai_ok(&env.mock_openai, 100_000, 50_000).await;

    // Act
    let (status, _body) = common::send_chat_request(
        &env.app,
        common::TOKEN1,
        common::chat_body("gpt-4o"),
    )
    .await;

    // Assert
    assert_eq!(status, StatusCode::OK, "request should succeed");

    let conn = env.db().await;

    // User layer: used_quota should be 1_500_000 微元 (net after pre-charge + settle).
    let user_used = common::get_user_used_quota(&conn, common::USER1_ID);
    assert_eq!(
        user_used, 1_500_000,
        "user.used_quota should be 1_500_000 微元 (actual_cost), got {}",
        user_used
    );

    // Token layer: remain_quota should be 5_000_000_000 - 1_500_000 = 4_998_500_000.
    let token_remain = common::get_token_remain(&conn, common::TOKEN1_ID);
    assert_eq!(
        token_remain, 4_998_500_000,
        "token.remain_quota should be 4_998_500_000, got {}",
        token_remain
    );

    // Token used_quota should also be 1_500_000.
    let token_used = common::get_token_used(&conn, common::TOKEN1_ID);
    assert_eq!(
        token_used, 1_500_000,
        "token.used_quota should be 1_500_000, got {}",
        token_used
    );

    // usage_logs should have one row with quota_cost = 1_500_000.
    assert_eq!(common::count_usage_logs(&conn), 1);
    let (_, _, _, quota_cost) = common::get_first_usage_log(&conn);
    assert_eq!(quota_cost, 1_500_000, "quota_cost in usage_logs should be 1_500_000 微元");
}

// ---------------------------------------------------------------------------
// Scenario 3: Group routing
// ---------------------------------------------------------------------------
//
// token1 (group=default) requests gpt-4o → routes to channel1 (OpenAI).
// token2 (group=premium) requests gpt-4o → routes to channel2 (Claude).
// The response content differs because each mock returns different text.
// token2's response is translated from Claude format back to OpenAI format.

#[tokio::test]
async fn test_scenario_3_group_routing() {
    // Arrange
    let env = common::setup().await;
    common::mock_openai_ok_with_content(&env.mock_openai, "openai-channel-response")
        .await;
    common::mock_claude_ok_with_content(&env.mock_claude, "claude-channel-response")
        .await;

    // Act — token1 (default group) → should hit channel1 (OpenAI mock)
    let (status1, body1) = common::send_chat_request(
        &env.app,
        common::TOKEN1,
        common::chat_body("gpt-4o"),
    )
    .await;

    // Act — token2 (premium group) → should hit channel2 (Claude mock)
    let (status2, body2) = common::send_chat_request(
        &env.app,
        common::TOKEN2,
        common::chat_body("gpt-4o"),
    )
    .await;

    // Assert
    assert_eq!(status1, StatusCode::OK, "token1 request should succeed");
    assert_eq!(status2, StatusCode::OK, "token2 request should succeed");

    // token1's response should come from the OpenAI mock.
    let content1 = body1["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    assert!(
        content1.contains("openai-channel-response"),
        "token1 should get response from OpenAI channel, got: {}",
        content1
    );

    // token2's response should come from the Claude mock (translated to
    // OpenAI format). The translator puts the Claude text content into
    // choices[0].message.content.
    let content2 = body2["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    assert!(
        content2.contains("claude-channel-response"),
        "token2 should get response from Claude channel, got: {}",
        content2
    );

    // Verify channel routing in usage_logs.
    let conn = env.db().await;
    let mut stmt = conn
        .prepare("SELECT channel_id FROM usage_logs ORDER BY id")
        .unwrap();
    let channel_ids: Vec<i64> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        channel_ids.contains(&common::CHANNEL1_ID),
        "token1 should have been routed to channel1"
    );
    assert!(
        channel_ids.contains(&common::CHANNEL2_ID),
        "token2 should have been routed to channel2"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: Token model_limits
// ---------------------------------------------------------------------------
//
// Set token1's model_limits to ["gpt-4o"] (canonical name).
// Request gpt-4o → should succeed (canonical "gpt-4o" is in the list).
// Request claude-3-5-sonnet → should fail with 400 (InvalidRequest).
//
// Note: The task spec mentioned gpt-4o vs gpt-4o-mini. The model_limits
// check compares against the canonical name. We use two different canonical
// models (gpt-4o vs claude-3-5-sonnet) to properly test the feature.

#[tokio::test]
async fn test_scenario_4_token_model_limits() {
    // Arrange
    let env = common::setup().await;
    common::mock_openai_ok(&env.mock_openai, 100, 50).await;

    // Enable model_limits on token1, only allowing "gpt-4o".
    {
        let conn = env.db().await;
        common::set_token_model_limits(&conn, common::TOKEN1_ID, &["gpt-4o"]);
    }

    // Act — request gpt-4o (allowed)
    let (status_ok, _body_ok) = common::send_chat_request(
        &env.app,
        common::TOKEN1,
        common::chat_body("gpt-4o"),
    )
    .await;

    // Act — request claude-3-5-sonnet (not allowed)
    let (status_denied, body_denied) = common::send_chat_request(
        &env.app,
        common::TOKEN1,
        common::chat_body("claude-3-5-sonnet"),
    )
    .await;

    // Assert
    assert_eq!(
        status_ok,
        StatusCode::OK,
        "gpt-4o should be allowed (in model_limits)"
    );
    assert_eq!(
        status_denied,
        StatusCode::BAD_REQUEST,
        "claude-3-5-sonnet should be denied (not in model_limits), got {}",
        status_denied
    );
    // The error response should mention the model name.
    let err_msg = body_denied["error"]["message"]
        .as_str()
        .unwrap_or("");
    assert!(
        err_msg.contains("claude-3-5-sonnet"),
        "error message should mention the denied model, got: {}",
        err_msg
    );
}
