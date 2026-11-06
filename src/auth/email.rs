//! Email delivery adapter for password reset instructions and notifications.

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error("email delivery failed")]
    DeliveryFailed,
}

#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send_password_reset(&self, to_email: &str, reset_link: &str)
    -> Result<(), EmailError>;
}

/// Local/development email sender that logs delivery via tracing.
#[derive(Debug, Clone, Default)]
pub struct LoggingEmailSender;

#[async_trait]
impl EmailSender for LoggingEmailSender {
    async fn send_password_reset(
        &self,
        to_email: &str,
        reset_link: &str,
    ) -> Result<(), EmailError> {
        tracing::info!(
            target: "auth::email",
            to = to_email,
            link = reset_link,
            "Dispatched password reset instructions"
        );
        Ok(())
    }
}
