//! A test server for the API's integration tests, already signed in.
//!
//! Every request a [`TestApp`] sends carries its credential, so a test reads
//! as the requests it makes rather than as the plumbing to authenticate them,
//! and testing as a restricted caller costs no more than testing as the admin:
//!
//! ```ignore
//! let app = TestApp::admin().await;
//! app.get("/deployments").await.assert_status_ok();
//!
//! let app = TestApp::with_pat(&["deployments:write"], &["tenant-a"]).await;
//! app.delete(&format!("/deployments/{other_tenant_id}")).await.assert_status_forbidden();
//! ```
//!
//! Credentials are minted in the database the way `/login` and `POST /tokens`
//! mint them, rather than by logging in with a password, so the suite does
//! not depend on the seeded admin's password.

use crate::api::server::tests::new_test_app_with_pool;
use crate::models::security_level::SecurityLevel;
use crate::models::token::{self, TokenKind};
use crate::models::users::Role;
use axum::Router;
use axum::http::header::AUTHORIZATION;
use axum_test::TestServer;
use sqlx::SqlitePool;
use std::ops::Deref;

/// The admin account the first migration seeds.
pub(crate) const ADMIN_ID: &str = "1c5a5fe9-84e0-4a18-821e-8058232c2c23";

pub(crate) struct TestApp {
    server: TestServer,
    /// The database behind the server, to arrange state and check effects.
    pub(crate) pool: SqlitePool,
    router: Router,
}

impl Deref for TestApp {
    type Target = TestServer;

    fn deref(&self) -> &TestServer {
        &self.server
    }
}

impl TestApp {
    /// No credential: what an anonymous caller gets.
    pub(crate) async fn anonymous() -> Self {
        let (pool, router) = new_test_app_with_pool().await;
        Self::from_parts(pool, router)
    }

    /// Signed in with a login session of the seeded admin.
    pub(crate) async fn admin() -> Self {
        let mut app = Self::anonymous().await;
        let session = app.session(ADMIN_ID, Role::Admin).await;
        app.authenticate(&session);
        app
    }

    /// Signed in with a login session of a user holding `role`, created for
    /// this test.
    pub(crate) async fn with_role(role: Role) -> Self {
        let mut app = Self::anonymous().await;
        let user_id = app.user(role).await;
        let session = app.session(&user_id, role).await;
        app.authenticate(&session);
        app
    }

    /// Signed in with a personal access token of the admin, limited to
    /// `scopes` and, unless empty, to `namespaces`.
    pub(crate) async fn with_pat(scopes: &[&str], namespaces: &[&str]) -> Self {
        let mut app = Self::anonymous().await;
        let pat = app.pat(scopes, namespaces).await;
        app.authenticate(&pat);
        app
    }

    fn from_parts(pool: SqlitePool, router: Router) -> Self {
        Self {
            server: TestServer::new(router.clone()),
            pool,
            router,
        }
    }

    /// The router, for tests that need a server of their own (a real socket
    /// for WebSockets, say).
    pub(crate) fn router(&self) -> Router {
        self.router.clone()
    }

    /// Send `token` as the bearer of every following request, in place of the
    /// current credential. Pair with [`Self::session`] or [`Self::pat`] to act
    /// as a second caller in the same database.
    pub(crate) fn authenticate(&mut self, token: &str) {
        self.server.clear_headers();
        self.server
            .add_header(AUTHORIZATION, format!("Bearer {token}"));
    }

    /// Stop sending any credential.
    pub(crate) fn sign_out(&mut self) {
        self.server.clear_headers();
    }

    /// Swap the credential for a new PAT of the admin, limited to `scopes`
    /// and, unless empty, to `namespaces`. Arrange as the admin first, then
    /// call this to act as a restricted caller on what was arranged.
    pub(crate) async fn act_as_pat(&mut self, scopes: &[&str], namespaces: &[&str]) {
        let pat = self.pat(scopes, namespaces).await;
        self.authenticate(&pat);
    }

    /// POST `body` to `path` with the current credential and return the id
    /// of what it created. Panics unless the API answers 201.
    pub(crate) async fn create(&self, path: &str, body: serde_json::Value) -> String {
        let response = self.post(path).json(&body).await;
        response.assert_status(axum::http::StatusCode::CREATED);
        response.json::<serde_json::Value>()["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no id in the response of POST {path}"))
            .to_string()
    }

    /// A login session for `user_id`, with the scopes `/login` gives `role`.
    pub(crate) async fn session(&self, user_id: &str, role: Role) -> String {
        let (clear, _) = token::create(
            &self.pool,
            user_id,
            "session",
            TokenKind::Session,
            &role.scopes(),
            &[],
            None,
        )
        .await
        .expect("mint a test session");
        clear
    }

    /// A personal access token of the admin, limited to `scopes` and, unless
    /// empty, to `namespaces`.
    pub(crate) async fn pat(&self, scopes: &[&str], namespaces: &[&str]) -> String {
        let owned = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        let (clear, _) = token::create(
            &self.pool,
            ADMIN_ID,
            "test-pat",
            TokenKind::Pat,
            &owned(scopes),
            &owned(namespaces),
            None,
        )
        .await
        .expect("mint a test PAT");
        clear
    }

    /// Create the namespace `name`, which secrets and volumes need to exist.
    pub(crate) async fn namespace(&self, name: &str) {
        self.create_namespace(name, None).await;
    }

    /// Create the namespace `name` at the security level `level`.
    pub(crate) async fn namespace_at(&self, name: &str, level: SecurityLevel) {
        self.create_namespace(name, Some(level)).await;
    }

    async fn create_namespace(&self, name: &str, security_level: Option<SecurityLevel>) {
        crate::models::namespace::create(
            &self.pool,
            crate::models::namespace::Namespace {
                id: uuid::Uuid::new_v4().to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                updated_at: None,
                name: name.to_string(),
                security_level,
            },
        )
        .await
        .expect("create a test namespace");
    }

    /// A new active user holding `role`, returning its id.
    pub(crate) async fn user(&self, role: Role) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO user (id, created_at, status, role, username, password) \
             VALUES (?, ?, 'active', ?, ?, '')",
        )
        .bind(&id)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(role.as_str())
        .bind(format!("{}-{}", role.as_str(), &id[..8]))
        .execute(&self.pool)
        .await
        .expect("insert a test user");
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn admin_reaches_admin_routes() {
        let app = TestApp::admin().await;
        app.get("/tokens").await.assert_status_ok();
    }

    #[tokio::test]
    async fn anonymous_is_refused() {
        let app = TestApp::anonymous().await;
        app.get("/deployments").await.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn viewer_reads_but_does_not_write() {
        let app = TestApp::with_role(Role::Viewer).await;
        app.get("/deployments").await.assert_status_ok();
        app.delete("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118")
            .await
            .assert_status_forbidden();
    }

    #[tokio::test]
    async fn pat_is_held_to_its_scopes() {
        let app = TestApp::with_pat(&["deployments:read"], &[]).await;
        app.get("/deployments").await.assert_status_ok();
        app.get("/secrets").await.assert_status_forbidden();
    }

    #[tokio::test]
    async fn authenticate_replaces_the_credential() {
        let mut app = TestApp::admin().await;
        let reader = app.pat(&["deployments:read"], &[]).await;
        app.authenticate(&reader);
        app.get("/tokens").await.assert_status_forbidden();
        app.sign_out();
        app.get("/tokens").await.assert_status_unauthorized();
    }
}
