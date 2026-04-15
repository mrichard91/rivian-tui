use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;

use super::client::{RivianClient, GATEWAY_URL};
use super::queries;
use super::types::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const CONFIG_DIR_NAME: &str = "rivian-tui";
const TOKEN_FILE_NAME: &str = "tokens.json";
const KEYRING_SERVICE_NAME: &str = "rivian-tui";
const KEYRING_ACCOUNT_NAME: &str = "auth_tokens";

#[cfg(test)]
mod test_support {
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct AuthTestOverrides {
        pub legacy_token_path: Option<PathBuf>,
        pub keyring_service_name: Option<String>,
        pub keyring_account_name: Option<String>,
    }

    pub static AUTH_TEST_LOCK: Mutex<()> = Mutex::new(());
    pub static AUTH_TEST_OVERRIDES: Mutex<AuthTestOverrides> = Mutex::new(AuthTestOverrides {
        legacy_token_path: None,
        keyring_service_name: None,
        keyring_account_name: None,
    });
}

/// Manages authentication with the Rivian API.
/// Tokens are persisted to the OS keychain.
pub struct AuthManager {
    client: RivianClient,
}

#[derive(Debug, Clone)]
pub struct PendingVehicleSelection {
    pub access_token: String,
    pub refresh_token: String,
    pub user_session_token: String,
    pub csrf_token: String,
    pub app_session_token: String,
    pub vehicles: Vec<Vehicle>,
}

impl PendingVehicleSelection {
    pub fn into_tokens(self, vehicle_id: String) -> AuthTokens {
        AuthTokens {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            user_session_token: self.user_session_token,
            csrf_token: self.csrf_token,
            app_session_token: self.app_session_token,
            vehicle_id,
        }
    }
}

/// Build the authenticated session headers used for saved-session API calls.
///
/// Rivian's saved access tokens can expire while the app and user session tokens
/// remain valid, so dashboard polling should rely on the session headers instead
/// of sending a potentially stale bearer token.
pub fn authenticated_headers(tokens: &AuthTokens) -> Vec<(&'static str, String)> {
    vec![
        ("Csrf-Token", tokens.csrf_token.clone()),
        ("A-Sess", tokens.app_session_token.clone()),
        ("U-Sess", tokens.user_session_token.clone()),
        ("Dc-Cid", format!("m-ios-{}", uuid::Uuid::new_v4())),
    ]
}

impl AuthManager {
    pub fn new(client: RivianClient) -> Self {
        Self { client }
    }

    fn legacy_token_file_path() -> Result<PathBuf> {
        #[cfg(test)]
        {
            let overrides = test_support::AUTH_TEST_OVERRIDES.lock().unwrap();
            if let Some(path) = overrides.legacy_token_path.clone() {
                return Ok(path);
            }
        }

        let config_dir = dirs::config_dir()
            .context("could not determine config directory")?
            .join(CONFIG_DIR_NAME);
        fs::create_dir_all(&config_dir).context("failed to create config directory")?;
        Ok(config_dir.join(TOKEN_FILE_NAME))
    }

    fn keyring_entry() -> Result<keyring::Entry> {
        #[cfg(test)]
        let (service_name, account_name) = {
            let overrides = test_support::AUTH_TEST_OVERRIDES.lock().unwrap();
            (
                overrides
                    .keyring_service_name
                    .clone()
                    .unwrap_or_else(|| KEYRING_SERVICE_NAME.to_string()),
                overrides
                    .keyring_account_name
                    .clone()
                    .unwrap_or_else(|| KEYRING_ACCOUNT_NAME.to_string()),
            )
        };

        #[cfg(not(test))]
        let (service_name, account_name) = (
            KEYRING_SERVICE_NAME.to_string(),
            KEYRING_ACCOUNT_NAME.to_string(),
        );

        keyring::Entry::new(&service_name, &account_name).context("failed to create keyring entry")
    }

    fn load_tokens_from_keyring() -> Result<Option<AuthTokens>> {
        let entry = Self::keyring_entry()?;
        match entry.get_password() {
            Ok(json_str) => {
                let tokens: AuthTokens =
                    serde_json::from_str(&json_str).context("corrupt keychain token payload")?;
                Ok(Some(tokens))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("failed to read tokens from keychain"),
        }
    }

    fn load_legacy_tokens() -> Result<Option<AuthTokens>> {
        let path = Self::legacy_token_file_path()?;
        match fs::read_to_string(&path) {
            Ok(json_str) => {
                let tokens: AuthTokens =
                    serde_json::from_str(&json_str).context("corrupt legacy token file")?;
                Ok(Some(tokens))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("failed to read legacy token file"),
        }
    }

    fn save_legacy_tokens(tokens: &AuthTokens) -> Result<()> {
        let path = Self::legacy_token_file_path()?;
        let json_str = serde_json::to_string_pretty(tokens)?;
        fs::write(&path, json_str).context("failed to write legacy token file")?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("failed to set legacy token file permissions")?;
        Ok(())
    }

    fn clear_legacy_token_file() -> Result<()> {
        let path = Self::legacy_token_file_path()?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("failed to remove legacy token file"),
        }
    }

    fn save_tokens_to_keyring(tokens: &AuthTokens) -> Result<()> {
        let json_str = serde_json::to_string_pretty(tokens)?;
        let entry = Self::keyring_entry()?;
        entry
            .set_password(&json_str)
            .context("failed to save tokens to keychain")
    }

    pub fn load_tokens() -> Result<Option<AuthTokens>> {
        let mut keyring_error = None;
        match Self::load_tokens_from_keyring() {
            Ok(Some(tokens)) => return Ok(Some(tokens)),
            Ok(None) => {}
            Err(e) => keyring_error = Some(e),
        }

        if let Some(tokens) = Self::load_legacy_tokens()? {
            let _ = Self::save_tokens_to_keyring(&tokens);
            return Ok(Some(tokens));
        }

        if let Some(err) = keyring_error {
            return Err(err);
        }

        Ok(None)
    }

    pub fn save_tokens(tokens: &AuthTokens) -> Result<()> {
        let keyring_result = Self::save_tokens_to_keyring(tokens);
        let legacy_result = Self::save_legacy_tokens(tokens);

        match (keyring_result, legacy_result) {
            (Ok(()), Ok(())) | (Ok(()), Err(_)) | (Err(_), Ok(())) => Ok(()),
            (Err(keyring_err), Err(legacy_err)) => Err(anyhow!(
                "failed to save tokens to both keychain and legacy file: {keyring_err}; {legacy_err}"
            )),
        }
    }

    pub fn clear_tokens() -> Result<()> {
        let keyring_result = if let Ok(entry) = Self::keyring_entry() {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(anyhow!(e).context("failed to clear tokens from keychain")),
            }
        } else {
            Ok(())
        };
        let legacy_result = Self::clear_legacy_token_file();

        match (keyring_result, legacy_result) {
            (Ok(()), Ok(())) | (Ok(()), Err(_)) | (Err(_), Ok(())) => Ok(()),
            (Err(keyring_err), Err(legacy_err)) => Err(anyhow!(
                "failed to clear both keychain and legacy token file: {keyring_err}; {legacy_err}"
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Auth flow
    // -----------------------------------------------------------------------

    async fn get_csrf_token(&self) -> Result<CsrfToken> {
        let data: CsrfData = self
            .client
            .graphql(
                GATEWAY_URL,
                "CreateCSRFToken",
                queries::CREATE_CSRF_TOKEN,
                None,
                None,
            )
            .await
            .context("failed to get CSRF token")?;
        Ok(data.create_csrf_token)
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<LoginOutcome> {
        let csrf = self.get_csrf_token().await?;

        let client_id = format!("m-ios-{}", uuid::Uuid::new_v4());
        let headers = vec![
            ("Csrf-Token", csrf.csrf_token.clone()),
            ("A-Sess", csrf.app_session_token.clone()),
            ("Dc-Cid", client_id),
        ];

        let vars = json!({
            "email": email,
            "password": password,
        });

        let data: LoginData = self
            .client
            .graphql(
                GATEWAY_URL,
                "Login",
                queries::LOGIN,
                Some(vars),
                Some(headers),
            )
            .await
            .context("login request failed")?;

        let result = data.login;

        if result.typename == "MobileMFALoginResponse" {
            let otp_token = result.otp_token.context("MFA response missing otp_token")?;

            let mfa = MfaState {
                email: email.to_string(),
                csrf_token: csrf.csrf_token,
                app_session_token: csrf.app_session_token,
                otp_token,
                timestamp: chrono::Utc::now().timestamp(),
            };
            return Ok(LoginOutcome::MfaRequired(mfa));
        }

        let access_token = result.access_token.context("missing access_token")?;
        let refresh_token = result.refresh_token.context("missing refresh_token")?;
        let user_session_token = result
            .user_session_token
            .context("missing user_session_token")?;

        self.complete_auth(
            &access_token,
            &refresh_token,
            &user_session_token,
            &csrf.csrf_token,
            &csrf.app_session_token,
        )
        .await
    }

    pub async fn complete_mfa(&self, mfa: &MfaState, otp_code: &str) -> Result<LoginOutcome> {
        let client_id = format!("m-ios-{}", uuid::Uuid::new_v4());
        let headers = vec![
            ("Csrf-Token", mfa.csrf_token.clone()),
            ("A-Sess", mfa.app_session_token.clone()),
            ("Dc-Cid", client_id),
        ];

        let vars = json!({
            "email": mfa.email,
            "otpCode": otp_code,
            "otpToken": mfa.otp_token,
        });

        let data: OtpLoginData = self
            .client
            .graphql(
                GATEWAY_URL,
                "LoginWithOTP",
                queries::LOGIN_WITH_OTP,
                Some(vars),
                Some(headers),
            )
            .await
            .context("OTP login failed")?;

        let r = data.login_with_otp;

        self.complete_auth(
            &r.access_token,
            &r.refresh_token,
            &r.user_session_token,
            &mfa.csrf_token,
            &mfa.app_session_token,
        )
        .await
    }

    async fn complete_auth(
        &self,
        access_token: &str,
        refresh_token: &str,
        user_session_token: &str,
        csrf_token: &str,
        app_session_token: &str,
    ) -> Result<LoginOutcome> {
        let client_id = format!("m-ios-{}", uuid::Uuid::new_v4());
        let headers = vec![
            ("Csrf-Token", csrf_token.to_string()),
            ("A-Sess", app_session_token.to_string()),
            ("U-Sess", user_session_token.to_string()),
            ("Dc-Cid", client_id),
        ];

        let data: UserInfoData = self
            .client
            .graphql(
                GATEWAY_URL,
                "getUserInfo",
                queries::GET_USER_INFO,
                None,
                Some(headers),
            )
            .await
            .context("failed to fetch vehicles")?;

        if data.current_user.vehicles.is_empty() {
            bail!("no vehicles found on account");
        }

        if data.current_user.vehicles.len() > 1 {
            return Ok(LoginOutcome::VehicleSelectionRequired(
                PendingVehicleSelection {
                    access_token: access_token.to_string(),
                    refresh_token: refresh_token.to_string(),
                    user_session_token: user_session_token.to_string(),
                    csrf_token: csrf_token.to_string(),
                    app_session_token: app_session_token.to_string(),
                    vehicles: data.current_user.vehicles,
                },
            ));
        }

        let vehicle_id = data.current_user.vehicles[0].id.clone();

        let tokens = AuthTokens {
            access_token: access_token.to_string(),
            refresh_token: refresh_token.to_string(),
            user_session_token: user_session_token.to_string(),
            csrf_token: csrf_token.to_string(),
            app_session_token: app_session_token.to_string(),
            vehicle_id,
        };

        Self::save_tokens(&tokens)?;

        Ok(LoginOutcome::Success(tokens))
    }
}

/// Outcome of a login attempt
pub enum LoginOutcome {
    Success(AuthTokens),
    MfaRequired(MfaState),
    VehicleSelectionRequired(PendingVehicleSelection),
}

#[cfg(test)]
pub(crate) struct AuthTestContext {
    _lock: std::sync::MutexGuard<'static, ()>,
    legacy_token_dir: PathBuf,
    keyring_service_name: String,
    keyring_account_name: String,
}

#[cfg(test)]
impl AuthTestContext {
    pub(crate) fn new() -> Self {
        let lock = test_support::AUTH_TEST_LOCK.lock().unwrap();
        let unique = uuid::Uuid::new_v4().to_string();
        let legacy_token_dir = std::env::temp_dir().join(format!("rivian-tui-test-{unique}"));
        fs::create_dir_all(&legacy_token_dir).unwrap();

        let keyring_service_name = format!("rivian-tui-test-{unique}");
        let keyring_account_name = "auth_tokens".to_string();

        let mut guard = test_support::AUTH_TEST_OVERRIDES.lock().unwrap();
        guard.legacy_token_path = Some(legacy_token_dir.join(TOKEN_FILE_NAME));
        guard.keyring_service_name = Some(keyring_service_name.clone());
        guard.keyring_account_name = Some(keyring_account_name.clone());
        drop(guard);

        Self {
            _lock: lock,
            legacy_token_dir,
            keyring_service_name,
            keyring_account_name,
        }
    }
}

#[cfg(test)]
impl Drop for AuthTestContext {
    fn drop(&mut self) {
        if let Ok(entry) =
            keyring::Entry::new(&self.keyring_service_name, &self.keyring_account_name)
        {
            let _ = entry.delete_credential();
        }

        let _ = fs::remove_file(self.legacy_token_dir.join(TOKEN_FILE_NAME));
        let _ = fs::remove_dir_all(&self.legacy_token_dir);
        let mut guard = test_support::AUTH_TEST_OVERRIDES.lock().unwrap();
        guard.legacy_token_path = None;
        guard.keyring_service_name = None;
        guard.keyring_account_name = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_vehicle_selection_builds_tokens() {
        let pending = PendingVehicleSelection {
            access_token: "test-at".into(),
            refresh_token: "test-rt".into(),
            user_session_token: "test-ust".into(),
            csrf_token: "test-csrf".into(),
            app_session_token: "test-ast".into(),
            vehicles: vec![Vehicle {
                id: "test-vid".into(),
                name: Some("My Rivian".into()),
            }],
        };

        let tokens = pending.into_tokens("test-vid".into());
        assert_eq!(tokens.access_token, "test-at");
        assert_eq!(tokens.vehicle_id, "test-vid");
    }

    #[test]
    fn authenticated_headers_use_session_tokens_only() {
        let headers = authenticated_headers(&AuthTokens {
            access_token: "test-at".into(),
            refresh_token: "test-rt".into(),
            user_session_token: "test-ust".into(),
            csrf_token: "test-csrf".into(),
            app_session_token: "test-ast".into(),
            vehicle_id: "test-vid".into(),
        });

        assert!(headers.iter().any(|(k, _)| *k == "Csrf-Token"));
        assert!(headers.iter().any(|(k, _)| *k == "A-Sess"));
        assert!(headers.iter().any(|(k, _)| *k == "U-Sess"));
        assert!(headers.iter().any(|(k, _)| *k == "Dc-Cid"));
        assert!(!headers.iter().any(|(k, _)| *k == "Authorization"));
    }

    #[test]
    fn load_legacy_tokens_parses_saved_json() {
        let _ctx = AuthTestContext::new();
        let tokens = AuthTokens {
            access_token: "test-at".into(),
            refresh_token: "test-rt".into(),
            user_session_token: "test-ust".into(),
            csrf_token: "test-csrf".into(),
            app_session_token: "test-ast".into(),
            vehicle_id: "test-vid".into(),
        };

        let json_str = serde_json::to_string(&tokens).unwrap();
        let path = AuthManager::legacy_token_file_path().unwrap();

        fs::write(&path, &json_str).unwrap();

        let loaded = AuthManager::load_legacy_tokens().unwrap().unwrap();
        assert_eq!(loaded.access_token, "test-at");
        assert_eq!(loaded.vehicle_id, "test-vid");

        let meta = fs::metadata(&path).unwrap();
        assert!(meta.is_file());
    }

    #[test]
    fn remove_legacy_token_file_ignores_missing_file() {
        let _ctx = AuthTestContext::new();
        let path = AuthManager::legacy_token_file_path().unwrap();
        let _ = fs::remove_file(&path);
        AuthManager::clear_legacy_token_file().unwrap();
    }

    #[test]
    fn save_load_and_clear_tokens_use_isolated_storage() {
        let _ctx = AuthTestContext::new();
        let tokens = AuthTokens {
            access_token: "test-at".into(),
            refresh_token: "test-rt".into(),
            user_session_token: "test-ust".into(),
            csrf_token: "test-csrf".into(),
            app_session_token: "test-ast".into(),
            vehicle_id: "test-vid".into(),
        };

        AuthManager::save_tokens(&tokens).unwrap();
        let loaded = AuthManager::load_tokens().unwrap().unwrap();
        assert_eq!(loaded.vehicle_id, "test-vid");
        assert!(AuthManager::legacy_token_file_path().unwrap().exists());

        AuthManager::clear_tokens().unwrap();
        assert!(AuthManager::load_legacy_tokens().unwrap().is_none());
    }
}
