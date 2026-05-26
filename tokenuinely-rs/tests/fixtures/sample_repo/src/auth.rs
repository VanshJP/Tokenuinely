/// Authentication module for the sample application.
/// Handles user login, token generation, and session management.

pub struct AuthService {
    secret_key: String,
}

impl AuthService {
    pub fn new(secret_key: &str) -> Self {
        Self {
            secret_key: secret_key.to_string(),
        }
    }

    pub fn login(&self, username: &str, password: &str) -> Result<String, AuthError> {
        // Validate credentials
        if username.is_empty() || password.is_empty() {
            return Err(AuthError::InvalidCredentials);
        }
        // Generate a session token
        Ok(format!("token_{}_{}", username, self.secret_key))
    }

    pub fn validate_token(&self, token: &str) -> bool {
        token.starts_with("token_") && token.contains(&self.secret_key)
    }
}

#[derive(Debug)]
pub enum AuthError {
    InvalidCredentials,
    TokenExpired,
}
