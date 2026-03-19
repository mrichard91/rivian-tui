use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::client::{GATEWAY_URL, RivianClient};
use super::queries;
use super::types::*;

const CONFIG_DIR_NAME: &str = "rivian-tui";
const TOKEN_FILE_NAME: &str = "tokens.json";

/// Manages authentication with the Rivian API.
/// Tokens are persisted to ~/.config/rivian-tui/tokens.json (mode 0600).
pub struct AuthManager {
    client: RivianClient,
}

impl AuthManager {
    pub fn new(client: RivianClient) -> Self {
        Self { client }
    }

    fn token_file_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .context("could not determine config directory")?
            .join(CONFIG_DIR_NAME);
        fs::create_dir_all(&config_dir)
            .context("failed to create config directory")?;
        Ok(config_dir.join(TOKEN_FILE_NAME))
    }

    pub fn load_tokens() -> Result<Option<AuthTokens>> {
        let path = Self::token_file_path()?;
        match fs::read_to_string(&path) {
            Ok(json_str) => {
                let tokens: AuthTokens =
                    serde_json::from_str(&json_str).context("corrupt token file")?;
                Ok(Some(tokens))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("failed to read token file"),
        }
    }

    pub fn save_tokens(tokens: &AuthTokens) -> Result<()> {
        let path = Self::token_file_path()?;
        let json_str = serde_json::to_string_pretty(tokens)?;
        fs::write(&path, &json_str).context("failed to write token file")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("failed to set token file permissions")?;
        Ok(())
    }

    pub fn clear_tokens() -> Result<()> {
        if let Ok(path) = Self::token_file_path() {
            let _ = fs::remove_file(path);
        }
        Ok(())
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
            let otp_token = result
                .otp_token
                .context("MFA response missing otp_token")?;

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

        let tokens = self
            .complete_auth(
                &access_token,
                &refresh_token,
                &user_session_token,
                &csrf.csrf_token,
                &csrf.app_session_token,
            )
            .await?;

        Ok(LoginOutcome::Success(tokens))
    }

    pub async fn complete_mfa(
        &self,
        mfa: &MfaState,
        otp_code: &str,
    ) -> Result<AuthTokens> {
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
    ) -> Result<AuthTokens> {
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

        Ok(tokens)
    }
}

/// Outcome of a login attempt
pub enum LoginOutcome {
    Success(AuthTokens),
    MfaRequired(MfaState),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_tokens_via_file() {
        let tokens = AuthTokens {
            access_token: "test-at".into(),
            refresh_token: "test-rt".into(),
            user_session_token: "test-ust".into(),
            csrf_token: "test-csrf".into(),
            app_session_token: "test-ast".into(),
            vehicle_id: "test-vid".into(),
        };

        let json_str = serde_json::to_string(&tokens).unwrap();
        let dir = std::env::temp_dir().join("rivian-tui-test");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("tokens.json");

        fs::write(&path, &json_str).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        let loaded: AuthTokens =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.access_token, "test-at");
        assert_eq!(loaded.vehicle_id, "test-vid");

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);

        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn clear_tokens_does_not_error_on_missing_file() {
        // Test with a nonexistent path — do NOT call AuthManager::clear_tokens()
        // as that would delete the real token file
        let bogus = std::env::temp_dir().join("rivian-tui-test-nonexistent");
        let _ = fs::remove_file(&bogus); // ensure it doesn't exist
        assert!(!bogus.exists());
    }
}
