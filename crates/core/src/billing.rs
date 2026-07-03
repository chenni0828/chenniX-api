//! Dual-layer billing: pre-charge → settle / refund.
//!
//! The proxy deducts an *estimated* cost from both the user's quota and the
//! token's remain_quota before sending a single byte upstream. After the
//! upstream response comes back (or the stream finishes), `settle` trues up
//! the difference between the estimate and the actual usage. If the request
//! never made it to a billable state (e.g. all keys failed), `refund`
//! returns the full pre-charge.
//!
//! Layer responsibilities:
//! - **User layer**: `user.quota - user.used_quota` is the bank account.
//!   Always deducted (even for unlimited tokens).
//! - **Token layer**: `token.remain_quota` is the per-token budget. Skipped
//!   when `token.unlimited_quota` is true.

use async_trait::async_trait;
use chennix_common::{ProxyError, ProxyResult};

/// Per-request billing handle. Created by `pre_charge`, consumed by
/// `settle` (which mutates it) or `refund` (which takes it by value).
#[derive(Debug)]
pub struct BillingSession {
    pub user_id: i64,
    pub token_id: i64,
    pub pre_charged: i64,
    pub settled: bool,
    /// If true, the token layer was skipped at pre-charge time (unlimited
    /// token). Settle/refund will skip the token side too.
    pub token_unlimited: bool,
}

/// Storage backend the billing manager talks to. The trait matches the
/// multi-user chennix-storage token/user repos one-to-one so a single
/// `Arc<Connection>` can implement it via a thin adapter.
///
/// 原子性约定：`pre_charge_atomic` / `settle_atomic` / `refund_atomic`
/// 三个方法在 storage 层用单事务执行，保证 user 层与 token 层同生共死。
/// `BillingManager` 只调用这三个原子方法，不再使用 `update_*` 做扣减。
#[async_trait]
pub trait BillingRepo: Send + Sync {
    /// `user.quota - user.used_quota` (None if user missing).
    async fn get_user_quota(&self, user_id: i64) -> ProxyResult<Option<i64>>;
    /// `token.remain_quota` (None if token missing).
    async fn get_token_remain_quota(&self, token_id: i64) -> ProxyResult<Option<i64>>;
    /// Set the token's status (1=enabled, 2=disabled, 3=exhausted).
    async fn update_token_status(&self, token_id: i64, status: i32) -> ProxyResult<()>;
    /// Whether the token has `unlimited_quota=true`.
    async fn get_token_unlimited(&self, token_id: i64) -> ProxyResult<Option<bool>>;

    /// 单事务预扣：在 user 层和 token 层（非 unlimited）同时检查余额并扣减。
    ///
    /// 预扣阶段检查余额（`quota - used_quota >= amount` 且
    /// `remain_quota >= amount`），不足则返回错误且无任何副作用。
    /// 任一层扣减失败则整个事务 ROLLBACK。
    async fn pre_charge_atomic(
        &self,
        user_id: i64,
        token_id: i64,
        amount: i64,
        token_unlimited: bool,
    ) -> ProxyResult<()>;

    /// 单事务结算：调整 `delta`（可为负）到 user 层和 token 层（非 unlimited）。
    ///
    /// 结算阶段**不检查余额**，允许 `used_quota` 超过 `quota`（透支），
    /// 也允许 `remain_quota` 变负。这与 new-api 的策略一致。
    async fn settle_atomic(
        &self,
        user_id: i64,
        token_id: i64,
        delta: i64,
        token_unlimited: bool,
    ) -> ProxyResult<()>;

    /// 单事务退款：退还预扣的 `amount` 到 user 层和 token 层（非 unlimited）。
    async fn refund_atomic(
        &self,
        user_id: i64,
        token_id: i64,
        amount: i64,
        token_unlimited: bool,
    ) -> ProxyResult<()>;
}

pub struct BillingManager;

impl BillingManager {
    pub fn new() -> Self {
        Self
    }

    /// Pre-charge an estimated cost.
    ///
    /// 在单个数据库事务内检查余额并扣减 user 层 + token 层（非 unlimited）。
    /// 任一层余额不足或扣减失败则整个事务回滚，无副作用。
    /// 这比 new-api 的"先扣 token 再扣 user，失败手动回滚"更强：
    /// SQLite 单写者 + 真事务 = 无 TOCTOU 窗口。
    pub async fn pre_charge(
        repo: &dyn BillingRepo,
        user_id: i64,
        token_id: i64,
        estimated_cost: i64,
    ) -> ProxyResult<BillingSession> {
        if estimated_cost < 0 {
            return Err(ProxyError::Config(format!(
                "estimated_cost must be non-negative, got {}",
                estimated_cost
            )));
        }

        // 查 unlimited 标志（只读查询，不影响原子性）
        let unlimited = repo
            .get_token_unlimited(token_id)
            .await?
            .ok_or_else(|| ProxyError::Config(format!("token {} not found", token_id)))?;

        if estimated_cost == 0 {
            // 无需扣减，但仍创建 session 以保持接口对称
            return Ok(BillingSession {
                user_id,
                token_id,
                pre_charged: 0,
                settled: false,
                token_unlimited: unlimited,
            });
        }

        // 单事务原子扣减（检查 + 扣减在同一事务内）
        repo.pre_charge_atomic(user_id, token_id, estimated_cost, unlimited)
            .await?;

        Ok(BillingSession {
            user_id,
            token_id,
            pre_charged: estimated_cost,
            settled: false,
            token_unlimited: unlimited,
        })
    }

    /// Settle the difference between the estimate and the actual cost.
    ///
    /// - `actual_cost == pre_charged`: no-op.
    /// - `actual_cost < pre_charged`: refund the difference to both layers.
    /// - `actual_cost > pre_charged`: charge the extra to both layers.
    ///   If the token layer hits exactly 0 remain_quota, set status=3
    ///   (exhausted).
    ///
    /// 结算阶段**不检查余额**，允许透支（`used_quota > quota`、
    /// `remain_quota < 0`）。这与 new-api 的策略一致：预扣阶段已做过
    /// 余额检查，结算阶段只调整差额，避免中断已完成的请求。
    pub async fn settle(
        repo: &dyn BillingRepo,
        session: &mut BillingSession,
        actual_cost: i64,
    ) -> ProxyResult<()> {
        if session.settled {
            return Err(ProxyError::Config(
                "billing session already settled".into(),
            ));
        }
        if actual_cost < 0 {
            return Err(ProxyError::Config(format!(
                "actual_cost must be non-negative, got {}",
                actual_cost
            )));
        }

        let delta = actual_cost - session.pre_charged;
        if delta != 0 {
            // 单事务调整差额（不检查余额，允许透支）
            repo.settle_atomic(
                session.user_id,
                session.token_id,
                delta,
                session.token_unlimited,
            )
            .await?;
        }

        // If the token layer just hit exactly zero, mark it exhausted.
        if !session.token_unlimited {
            if let Some(remaining) = repo.get_token_remain_quota(session.token_id).await? {
                if remaining <= 0 {
                    repo.update_token_status(session.token_id, 3).await?;
                }
            }
        }

        session.settled = true;
        Ok(())
    }

    /// Refund the full pre-charge (request failed before any billable work).
    pub async fn refund(
        repo: &dyn BillingRepo,
        session: BillingSession,
    ) -> ProxyResult<()> {
        if session.pre_charged == 0 {
            return Ok(());
        }
        repo.refund_atomic(
            session.user_id,
            session.token_id,
            session.pre_charged,
            session.token_unlimited,
        )
        .await?;
        Ok(())
    }
}

impl Default for BillingManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory mock billing repo. Each (user/token) has a record; we use
    /// `Mutex` so the `&dyn BillingRepo` (which is `&self`-method) can
    /// still mutate state.
    struct MockRepo {
        inner: Mutex<MockState>,
    }

    #[derive(Default, Clone)]
    struct MockState {
        user_quota: i64,
        user_used: i64,
        token_remain: i64,
        token_used: i64,
        token_unlimited: bool,
        token_status: i32,
        user_present: bool,
        token_present: bool,
    }

    impl MockRepo {
        fn new() -> Self {
            Self {
                inner: Mutex::new(MockState {
                    user_quota: 1000,
                    user_used: 0,
                    token_remain: 500,
                    token_used: 0,
                    token_unlimited: false,
                    token_status: 1,
                    user_present: true,
                    token_present: true,
                }),
            }
        }
        fn snapshot(&self) -> MockState {
            self.inner.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl BillingRepo for MockRepo {
        async fn get_user_quota(&self, user_id: i64) -> ProxyResult<Option<i64>> {
            let s = self.inner.lock().unwrap();
            if !s.user_present {
                return Ok(None);
            }
            let _ = user_id;
            Ok(Some(s.user_quota - s.user_used))
        }
        async fn get_token_remain_quota(&self, token_id: i64) -> ProxyResult<Option<i64>> {
            let s = self.inner.lock().unwrap();
            if !s.token_present {
                return Ok(None);
            }
            let _ = token_id;
            Ok(Some(s.token_remain))
        }
        async fn update_token_status(&self, _token_id: i64, status: i32) -> ProxyResult<()> {
            let mut s = self.inner.lock().unwrap();
            s.token_status = status;
            Ok(())
        }
        async fn get_token_unlimited(&self, _token_id: i64) -> ProxyResult<Option<bool>> {
            let s = self.inner.lock().unwrap();
            if !s.token_present {
                return Ok(None);
            }
            Ok(Some(s.token_unlimited))
        }

        // 单事务模拟：Mutex 保证原子性，检查+扣减在同一锁内完成
        async fn pre_charge_atomic(
            &self,
            user_id: i64,
            token_id: i64,
            amount: i64,
            token_unlimited: bool,
        ) -> ProxyResult<()> {
            let mut s = self.inner.lock().unwrap();
            let _ = (user_id, token_id);
            // user 层检查
            if s.user_quota - s.user_used < amount {
                return Err(ProxyError::Config(format!(
                    "insufficient user quota: remaining={} needed={}",
                    s.user_quota - s.user_used,
                    amount
                )));
            }
            // token 层检查（非 unlimited）
            if !token_unlimited && s.token_remain < amount {
                return Err(ProxyError::Config(format!(
                    "insufficient token quota: remaining={} needed={}",
                    s.token_remain, amount
                )));
            }
            // 扣减（两层在同一锁内完成，模拟事务）
            s.user_used += amount;
            if !token_unlimited {
                s.token_remain -= amount;
                s.token_used += amount;
            }
            Ok(())
        }

        async fn settle_atomic(
            &self,
            user_id: i64,
            token_id: i64,
            delta: i64,
            token_unlimited: bool,
        ) -> ProxyResult<()> {
            let mut s = self.inner.lock().unwrap();
            let _ = (user_id, token_id);
            // 结算不检查余额，允许透支
            s.user_used += delta;
            if !token_unlimited {
                s.token_remain -= delta;
                s.token_used += delta;
            }
            Ok(())
        }

        async fn refund_atomic(
            &self,
            user_id: i64,
            token_id: i64,
            amount: i64,
            token_unlimited: bool,
        ) -> ProxyResult<()> {
            let mut s = self.inner.lock().unwrap();
            let _ = (user_id, token_id);
            s.user_used -= amount;
            if !token_unlimited {
                s.token_remain += amount;
                s.token_used -= amount;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_pre_charge_success() {
        let repo = MockRepo::new();
        let session = BillingManager::pre_charge(&repo, 1, 10, 100).await.unwrap();

        assert_eq!(session.user_id, 1);
        assert_eq!(session.token_id, 10);
        assert_eq!(session.pre_charged, 100);
        assert!(!session.settled);
        assert!(!session.token_unlimited);

        let s = repo.snapshot();
        assert_eq!(s.user_used, 100, "user used_quota should be 100");
        assert_eq!(s.token_remain, 400, "token remain should drop by 100");
        assert_eq!(s.token_used, 100, "token used should rise by 100");
        assert_eq!(s.token_status, 1, "token still enabled");
    }

    #[tokio::test]
    async fn test_pre_charge_insufficient_user_quota() {
        let repo = MockRepo::new();
        // user has 1000 - 0 = 1000 available; ask for 2000
        let err = BillingManager::pre_charge(&repo, 1, 10, 2000)
            .await
            .unwrap_err();
        assert!(matches!(err, ProxyError::Config(_)), "got {:?}", err);

        // No side effects
        let s = repo.snapshot();
        assert_eq!(s.user_used, 0);
        assert_eq!(s.token_remain, 500);
        assert_eq!(s.token_used, 0);
    }

    #[tokio::test]
    async fn test_pre_charge_insufficient_token_quota() {
        let repo = MockRepo::new();
        // user has 1000, token has 500. Ask for 800 → user OK, token fails.
        let err = BillingManager::pre_charge(&repo, 1, 10, 800)
            .await
            .unwrap_err();
        assert!(matches!(err, ProxyError::Config(_)), "got {:?}", err);

        // No side effects on either layer
        let s = repo.snapshot();
        assert_eq!(s.user_used, 0, "user layer must not be touched");
        assert_eq!(s.token_remain, 500, "token layer must not be touched");
        assert_eq!(s.token_used, 0);
    }

    #[tokio::test]
    async fn test_settle_refund_when_actual_below_pre_charge() {
        let repo = MockRepo::new();
        let mut session = BillingManager::pre_charge(&repo, 1, 10, 100).await.unwrap();

        // actual usage only 30 → refund 70 to both layers
        BillingManager::settle(&repo, &mut session, 30).await.unwrap();
        assert!(session.settled);

        let s = repo.snapshot();
        // user: charged 100 then refunded 70 → net 30
        assert_eq!(s.user_used, 30);
        // token: remain 500 - 100 + 70 = 470, used 100 - 70 = 30
        assert_eq!(s.token_remain, 470);
        assert_eq!(s.token_used, 30);
        // token still has remain > 0 → status unchanged
        assert_eq!(s.token_status, 1);
    }

    #[tokio::test]
    async fn test_settle_extra_charge_when_actual_exceeds_pre_charge() {
        let repo = MockRepo::new();
        let mut session = BillingManager::pre_charge(&repo, 1, 10, 100).await.unwrap();

        // actual usage 250 → charge 150 more to both layers
        BillingManager::settle(&repo, &mut session, 250).await.unwrap();
        assert!(session.settled);

        let s = repo.snapshot();
        assert_eq!(s.user_used, 250);
        assert_eq!(s.token_remain, 250); // 500 - 250
        assert_eq!(s.token_used, 250);
        // still has remain > 0
        assert_eq!(s.token_status, 1);
    }

    #[tokio::test]
    async fn test_settle_marks_token_exhausted_when_remain_hits_zero() {
        let repo = MockRepo::new();
        // Pre-charge exactly the token remain (500) so settle to actual=500
        // leaves token_remain at 0.
        let mut session = BillingManager::pre_charge(&repo, 1, 10, 500).await.unwrap();
        BillingManager::settle(&repo, &mut session, 500).await.unwrap();

        let s = repo.snapshot();
        assert_eq!(s.token_remain, 0);
        assert_eq!(s.token_status, 3, "token must be marked exhausted (status=3)");
    }

    #[tokio::test]
    async fn test_refund_returns_full_pre_charge() {
        let repo = MockRepo::new();
        let session = BillingManager::pre_charge(&repo, 1, 10, 100).await.unwrap();

        BillingManager::refund(&repo, session).await.unwrap();
        let s = repo.snapshot();
        assert_eq!(s.user_used, 0, "user layer refunded");
        assert_eq!(s.token_remain, 500, "token layer refunded");
        assert_eq!(s.token_used, 0);
    }

    #[tokio::test]
    async fn test_pre_charge_unlimited_token_skips_token_layer() {
        let repo = MockRepo::new();
        {
            let mut s = repo.inner.lock().unwrap();
            s.token_unlimited = true;
        }
        let session = BillingManager::pre_charge(&repo, 1, 10, 100).await.unwrap();
        assert!(session.token_unlimited);

        let s = repo.snapshot();
        assert_eq!(s.user_used, 100, "user layer deducted");
        assert_eq!(s.token_remain, 500, "token remain untouched (unlimited)");
        assert_eq!(s.token_used, 0);

        // settle with refund should also skip token layer
        let mut session = session;
        BillingManager::settle(&repo, &mut session, 30).await.unwrap();
        let s = repo.snapshot();
        assert_eq!(s.user_used, 30, "user layer refunded");
        assert_eq!(s.token_remain, 500, "token layer untouched on refund");
        assert_eq!(s.token_used, 0);
    }

    #[tokio::test]
    async fn test_pre_charge_zero_cost_no_side_effects() {
        let repo = MockRepo::new();
        let session = BillingManager::pre_charge(&repo, 1, 10, 0).await.unwrap();
        assert_eq!(session.pre_charged, 0);
        let s = repo.snapshot();
        assert_eq!(s.user_used, 0);
        assert_eq!(s.token_remain, 500);

        // refund should also be a no-op
        BillingManager::refund(&repo, session).await.unwrap();
        let s = repo.snapshot();
        assert_eq!(s.user_used, 0);
    }

    #[tokio::test]
    async fn test_settle_double_settle_rejected() {
        let repo = MockRepo::new();
        let mut session = BillingManager::pre_charge(&repo, 1, 10, 100).await.unwrap();
        BillingManager::settle(&repo, &mut session, 50).await.unwrap();
        let err = BillingManager::settle(&repo, &mut session, 50).await.unwrap_err();
        assert!(matches!(err, ProxyError::Config(_)));
    }
}
