//! Eye-owned SQLx PostgreSQL persistence for identity, OAuth and sessions.
//!
//! All mounted authentication routes use this adapter. Raw bearer credentials and
//! one-time codes are never persisted; only SHA-256 hashes are stored.

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};
use sqlx::{PgPool, Row};
use thiserror::Error;

use crate::auth::{UserRecord, UserRole, hash_password};
use crate::config::OAuthRuntimeConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DurableStoreError {
    #[error("durable identity store is unavailable")]
    Unavailable,
    #[error("durable identity store rejected the operation")]
    Rejected,
    #[error("durable identity record is invalid")]
    InvalidRecord,
}

#[derive(Clone)]
pub struct DurableEyeStore {
    pool: PgPool,
    encryption_key: [u8; 32],
}

#[derive(Clone)]
pub struct PasswordCredentials {
    pub user_id: String,
    pub email: String,
    pub password_hash: String,
    pub salt: String,
    pub role: UserRole,
    pub scopes: Vec<String>,
    pub is_active: bool,
    pub created_at_ms: u64,
}
impl std::fmt::Debug for PasswordCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordCredentials")
            .field("user_id", &self.user_id)
            .field("role", &self.role)
            .field("is_active", &self.is_active)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct DurableUser {
    pub user_id: String,
    pub email: String,
    pub role: UserRole,
    pub scopes: Vec<String>,
    pub is_active: bool,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct OAuthTransaction {
    pub nonce: String,
    pub code_verifier: String,
    pub callback_url: String,
    pub state_valid: bool,
}
#[derive(Debug, Clone)]
pub struct ConsumedHandoff {
    pub user_id: String,
}
#[derive(sqlx::FromRow)]
struct UserRow {
    user_id: String,
    email: String,
    role: String,
    scopes: Vec<String>,
    is_active: bool,
    created_at_ms: i64,
}
#[derive(sqlx::FromRow)]
struct CredentialRow {
    user_id: String,
    email: String,
    password_hash: String,
    salt: String,
    role: String,
    scopes: Vec<String>,
    is_active: bool,
    created_at_ms: i64,
}
#[derive(sqlx::FromRow)]
struct RefreshRow {
    family_id: String,
    user_id: String,
    role: String,
    scopes: Vec<String>,
    event_id: Option<String>,
    expires_at_ms: i64,
    used_at_ms: Option<i64>,
    revoked_at_ms: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct RefreshRotation {
    pub outcome: String,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub role: Option<UserRole>,
    pub scopes: Option<Vec<String>>,
    pub is_active: Option<bool>,
    pub created_at_ms: Option<u64>,
    pub event_id: Option<String>,
    pub family_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct RegisterUserInput<'a> {
    pub user_id: &'a str,
    pub email: &'a str,
    pub password_hash: &'a str,
    pub salt: &'a str,
    pub role: UserRole,
    pub scopes: &'a [String],
    pub now_ms: u64,
}

impl DurableEyeStore {
    pub fn new(pool: PgPool, config: &OAuthRuntimeConfig) -> Self {
        Self { pool, encryption_key: config.oauth_encryption_key }
    }

    fn cipher(&self) -> Result<LessSafeKey, DurableStoreError> {
        let key = UnboundKey::new(&AES_256_GCM, &self.encryption_key)
            .map_err(|_| DurableStoreError::InvalidRecord)?;
        Ok(LessSafeKey::new(key))
    }

    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, DurableStoreError> {
        let mut nonce_bytes = [0u8; 12];
        SystemRandom::new().fill(&mut nonce_bytes).map_err(|_| DurableStoreError::Unavailable)?;
        let mut in_out = plaintext.to_vec();
        self.cipher()?.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::empty(),
            &mut in_out,
        ).map_err(|_| DurableStoreError::Unavailable)?;
        let mut output = nonce_bytes.to_vec();
        output.extend_from_slice(&in_out);
        Ok(output)
    }

    fn decrypt(&self, ciphertext: &[u8]) -> Result<String, DurableStoreError> {
        if ciphertext.len() < 12 + 16 { return Err(DurableStoreError::InvalidRecord); }
        let nonce_bytes: [u8; 12] = ciphertext[..12].try_into().map_err(|_| DurableStoreError::InvalidRecord)?;
        let mut in_out = ciphertext[12..].to_vec();
        let plaintext = self.cipher()?.open_in_place(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::empty(),
            &mut in_out,
        ).map_err(|_| DurableStoreError::InvalidRecord)?;
        String::from_utf8(plaintext.to_vec()).map_err(|_| DurableStoreError::InvalidRecord)
    }

    pub async fn create_oauth_transaction(&self, cookie_hash: &str, state_hash: &str, nonce: &str, code_verifier: &str, callback_url: &str, _now_ms: u64) -> Result<(), DurableStoreError> {
        let cookie = decode_hash(cookie_hash)?;
        let state = decode_hash(state_hash)?;
        let nonce_hash = decode_hash(&hash_hex(nonce))?;
        let nonce_enc = self.encrypt(nonce.as_bytes())?;
        let verifier_enc = self.encrypt(code_verifier.as_bytes())?;
        sqlx::query("INSERT INTO oauth_transactions (cookie_hash,state_hash,nonce_hash,nonce_enc,pkce_verifier_enc,callback_url,expires_at) VALUES ($1,$2,$3,$4,$5,$6,now()+interval '10 minutes')")
            .bind(cookie).bind(state).bind(nonce_hash).bind(nonce_enc).bind(verifier_enc).bind(callback_url)
            .execute(&self.pool).await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn consume_oauth_transaction(&self, cookie_hash: &str, state_hash: &str, _now_ms: u64) -> Result<Option<OAuthTransaction>, DurableStoreError> {
        let cookie = decode_hash(cookie_hash)?;
        let state = decode_hash(state_hash)?;
        let row = sqlx::query("UPDATE oauth_transactions SET consumed_at=now() WHERE cookie_hash=$1 AND consumed_at IS NULL RETURNING nonce_enc,pkce_verifier_enc,callback_url,(state_hash=$2 AND expires_at>now()) AS state_valid")
            .bind(cookie).bind(state).fetch_optional(&self.pool).await.map_err(map_sqlx)?;
        let Some(row) = row else { return Ok(None); };
        Ok(Some(OAuthTransaction {
            nonce: self.decrypt(&row.try_get::<Vec<u8>,_>("nonce_enc").map_err(|_| DurableStoreError::InvalidRecord)?)?,
            code_verifier: self.decrypt(&row.try_get::<Vec<u8>,_>("pkce_verifier_enc").map_err(|_| DurableStoreError::InvalidRecord)?)?,
            callback_url: row.try_get("callback_url").map_err(|_| DurableStoreError::InvalidRecord)?,
            state_valid: row.try_get("state_valid").map_err(|_| DurableStoreError::InvalidRecord)?,
        }))
    }

    pub async fn resolve_google_identity(&self, provider_subject: &str, email: &str, email_verified: bool, now_ms: u64, new_user: &UserRecord) -> Result<DurableUser, DurableStoreError> {
        if !email_verified || provider_subject.trim().is_empty() || email.trim().is_empty() { return Err(DurableStoreError::Rejected); }
        let mut tx = self.pool.begin().await.map_err(map_sqlx)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("eye-google-sub:{provider_subject}")).execute(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("eye-email:{}", email.trim().to_ascii_lowercase())).execute(&mut *tx).await.map_err(map_sqlx)?;
        let linked = sqlx::query_as::<_, UserRow>("SELECT u.user_id,u.email,u.role,u.scopes,u.is_active,u.created_at_ms FROM identity_links i JOIN eye_auth_users u ON u.user_id=i.user_id WHERE i.provider='google' AND i.provider_subject=$1 FOR UPDATE OF u")
            .bind(provider_subject).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
        let user = if let Some(row) = linked {
            row
        } else {
            let existing = sqlx::query_as::<_, UserRow>("SELECT user_id,email,role,scopes,is_active,created_at_ms FROM eye_auth_users WHERE lower(email)=lower($1) FOR UPDATE")
                .bind(email.trim()).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
            let row = if let Some(row) = existing {
                let conflict: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM identity_links WHERE provider='google' AND user_id=$1 AND provider_subject<>$2)")
                    .bind(&row.user_id).bind(provider_subject).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
                if conflict { return Err(DurableStoreError::Rejected); }
                row
            } else {
                sqlx::query("INSERT INTO eye_auth_users(user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms) VALUES($1,lower($2),$3,$4,$5,'{}',true,$6) RETURNING user_id,email,role,scopes,is_active,created_at_ms")
                    .bind(&new_user.user_id).bind(email.trim()).bind(&new_user.password_hash).bind(&new_user.salt)
                    .bind(role_name(new_user.role)).bind(to_i64(now_ms)?)
                    .fetch_one(&mut *tx).await.map_err(map_sqlx)?
            };
            sqlx::query("INSERT INTO identity_links(provider,provider_subject,user_id,email_verified) VALUES('google',$1,$2,true)")
                .bind(provider_subject).bind(&row.user_id).execute(&mut *tx).await.map_err(map_sqlx)?;
            row
        };
        tx.commit().await.map_err(map_sqlx)?;
        user_row(user)
    }

    pub async fn user_by_id(&self, user_id: &str) -> Result<Option<DurableUser>, DurableStoreError> {
        let row = sqlx::query_as::<_, UserRow>("SELECT user_id,email,role,scopes,is_active,created_at_ms FROM eye_auth_users WHERE user_id=$1")
            .bind(user_id).fetch_optional(&self.pool).await.map_err(map_sqlx)?;
        row.map(user_row).transpose()
    }

    pub async fn issue_handoff(&self, code_hash: &str, user_id: &str, callback_url: &str, _now_ms: u64) -> Result<(), DurableStoreError> {
        sqlx::query("INSERT INTO oauth_handoffs(code_hash,user_id,callback_url,expires_at) VALUES($1,$2,$3,now()+interval '60 seconds')")
            .bind(decode_hash(code_hash)?).bind(user_id).bind(callback_url).execute(&self.pool).await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn consume_handoff(&self, code_hash: &str, callback_url: &str, _now_ms: u64) -> Result<Option<ConsumedHandoff>, DurableStoreError> {
        let row = sqlx::query("UPDATE oauth_handoffs SET consumed_at=now() WHERE code_hash=$1 AND consumed_at IS NULL RETURNING user_id,(callback_url=$2 AND expires_at>now()) AS valid")
            .bind(decode_hash(code_hash)?).bind(callback_url).fetch_optional(&self.pool).await.map_err(map_sqlx)?;
        let Some(row) = row else { return Ok(None); };
        let valid: bool = row.try_get("valid").map_err(|_| DurableStoreError::InvalidRecord)?;
        if !valid { return Ok(None); }
        Ok(Some(ConsumedHandoff { user_id: row.try_get("user_id").map_err(|_| DurableStoreError::InvalidRecord)? }))
    }

    pub async fn allow_oauth_attempt(&self, bucket_hash: &str, now_ms: u64, window_ms: u64, max_attempts: u32) -> Result<bool, DurableStoreError> {
        if window_ms == 0 || max_attempts == 0 { return Err(DurableStoreError::Rejected); }
        let now = to_i64(now_ms)?;
        let window = to_i64(window_ms)?;
        let start = now - now.rem_euclid(window);
        let attempts: i32 = sqlx::query_scalar("INSERT INTO eye_oauth_rate_limits(bucket_hash,window_start_ms,attempts) VALUES($1,$2,1) ON CONFLICT(bucket_hash,window_start_ms) DO UPDATE SET attempts=eye_oauth_rate_limits.attempts+1 RETURNING attempts")
            .bind(decode_hash(bucket_hash)?).bind(start).fetch_one(&self.pool).await.map_err(map_sqlx)?;
        sqlx::query("DELETE FROM eye_oauth_rate_limits WHERE window_start_ms<$1").bind(now.saturating_sub(86_400_000)).execute(&self.pool).await.map_err(map_sqlx)?;
        Ok(attempts <= max_attempts as i32)
    }

    pub async fn update_password_hash(&self, user_id: &str, expected_hash: &str, new_hash: &str) -> Result<bool, DurableStoreError> {
        let result = sqlx::query("UPDATE eye_auth_users SET password_hash=$3 WHERE user_id=$1 AND password_hash=$2")
            .bind(user_id).bind(expected_hash).bind(new_hash).execute(&self.pool).await.map_err(map_sqlx)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn password_credentials_by_email(&self, email: &str) -> Result<Option<PasswordCredentials>, DurableStoreError> {
        let row = sqlx::query_as::<_, CredentialRow>("SELECT user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms FROM eye_auth_users WHERE lower(email)=lower($1) LIMIT 1")
            .bind(email.trim()).fetch_optional(&self.pool).await.map_err(map_sqlx)?;
        row.map(|r| Ok(PasswordCredentials { user_id:r.user_id,email:r.email,password_hash:r.password_hash,salt:r.salt,role:parse_role(&r.role)?,scopes:r.scopes,is_active:r.is_active,created_at_ms:to_u64(r.created_at_ms)? })).transpose()
    }

    pub async fn register_user(&self, input: RegisterUserInput<'_>) -> Result<DurableUser, DurableStoreError> {
        let row = sqlx::query_as::<_, UserRow>("INSERT INTO eye_auth_users(user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms) VALUES($1,lower($2),$3,$4,$5,$6,true,$7) RETURNING user_id,email,role,scopes,is_active,created_at_ms")
            .bind(input.user_id).bind(input.email.trim()).bind(input.password_hash).bind(input.salt)
            .bind(role_name(input.role)).bind(input.scopes).bind(to_i64(input.now_ms)?)
            .fetch_one(&self.pool).await.map_err(map_sqlx)?;
        user_row(row)
    }

    pub async fn create_password_reset_token(&self, token_hash_hex: &str, email: &str, now_ms: u64, ttl_ms: u64) -> Result<Option<String>, DurableStoreError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx)?;
        let user = sqlx::query("SELECT user_id,email FROM eye_auth_users WHERE lower(email)=lower($1) AND is_active=true FOR UPDATE")
            .bind(email.trim()).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
        let Some(user) = user else { tx.commit().await.map_err(map_sqlx)?; return Ok(None); };
        let user_id: String = user.try_get("user_id").map_err(|_| DurableStoreError::InvalidRecord)?;
        let user_email: String = user.try_get("email").map_err(|_| DurableStoreError::InvalidRecord)?;
        let now = to_i64(now_ms)?;
        sqlx::query("UPDATE eye_password_reset_tokens SET consumed_at_ms=$2 WHERE user_id=$1 AND consumed_at_ms IS NULL")
            .bind(&user_id).bind(now).execute(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query("INSERT INTO eye_password_reset_tokens(token_hash,user_id,created_at_ms,expires_at_ms) VALUES($1,$2,$3,$4)")
            .bind(decode_hash(token_hash_hex)?).bind(user_id).bind(now).bind(now.saturating_add(to_i64(ttl_ms)?))
            .execute(&mut *tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(Some(user_email))
    }

    pub async fn consume_password_reset_token(&self, token_hash_hex: &str, new_password_hash: &str, new_salt: &str, now_ms: u64) -> Result<bool, DurableStoreError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx)?;
        let now = to_i64(now_ms)?;
        let row = sqlx::query("SELECT user_id,expires_at_ms,consumed_at_ms FROM eye_password_reset_tokens WHERE token_hash=$1 FOR UPDATE")
            .bind(decode_hash(token_hash_hex)?).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
        let Some(row) = row else { tx.commit().await.map_err(map_sqlx)?; return Ok(false); };
        let expires: i64 = row.try_get("expires_at_ms").map_err(|_| DurableStoreError::InvalidRecord)?;
        let consumed: Option<i64> = row.try_get("consumed_at_ms").map_err(|_| DurableStoreError::InvalidRecord)?;
        if consumed.is_some() || expires <= now { tx.commit().await.map_err(map_sqlx)?; return Ok(false); }
        let user_id: String = row.try_get("user_id").map_err(|_| DurableStoreError::InvalidRecord)?;
        sqlx::query("UPDATE eye_password_reset_tokens SET consumed_at_ms=$2 WHERE token_hash=$1").bind(decode_hash(token_hash_hex)?).bind(now).execute(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query("UPDATE eye_auth_users SET password_hash=$2,salt=$3 WHERE user_id=$1").bind(user_id).bind(new_password_hash).bind(new_salt).execute(&mut *tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(true)
    }

    pub async fn issue_refresh_token(&self, token_hash: &str, family_id: &str, user: &DurableUser, event_id: Option<&str>, now_ms: u64, expires_at_ms: u64) -> Result<(), DurableStoreError> {
        sqlx::query("INSERT INTO eye_refresh_tokens(token_hash,family_id,user_id,role,scopes,event_id,issued_at_ms,expires_at_ms) SELECT $1,$2,u.user_id,$4,$5,$6,$7,$8 FROM eye_auth_users u WHERE u.user_id=$3 AND u.is_active=true")
            .bind(decode_hash(token_hash)?).bind(family_id).bind(&user.user_id).bind(role_name(user.role)).bind(&user.scopes).bind(event_id).bind(to_i64(now_ms)?).bind(to_i64(expires_at_ms)?)
            .execute(&self.pool).await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn rotate_refresh_token(&self, old_token_hash: &str, new_token_hash: &str, now_ms: u64, new_expires_at_ms: u64) -> Result<RefreshRotation, DurableStoreError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx)?;
        let now = to_i64(now_ms)?;
        let old = sqlx::query_as::<_, RefreshRow>("SELECT family_id,user_id,role,scopes,event_id,expires_at_ms,used_at_ms,revoked_at_ms FROM eye_refresh_tokens WHERE token_hash=$1 FOR UPDATE")
            .bind(decode_hash(old_token_hash)?).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
        let Some(old) = old else { tx.commit().await.map_err(map_sqlx)?; return Ok(invalid_rotation("invalid")); };
        if old.used_at_ms.is_some() {
            sqlx::query("UPDATE eye_refresh_tokens SET revoked_at_ms=$2 WHERE family_id=$1 AND revoked_at_ms IS NULL")
                .bind(&old.family_id).bind(now).execute(&mut *tx).await.map_err(map_sqlx)?;
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(invalid_rotation("replay"));
        }
        if old.revoked_at_ms.is_some() || old.expires_at_ms <= now || to_i64(new_expires_at_ms)? <= now {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(invalid_rotation("invalid"));
        }
        let user = sqlx::query_as::<_, UserRow>("SELECT user_id,email,role,scopes,is_active,created_at_ms FROM eye_auth_users WHERE user_id=$1 FOR UPDATE")
            .bind(&old.user_id).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
        let Some(user) = user.filter(|u| u.is_active) else {
            sqlx::query("UPDATE eye_refresh_tokens SET revoked_at_ms=$2 WHERE family_id=$1 AND revoked_at_ms IS NULL")
                .bind(&old.family_id).bind(now).execute(&mut *tx).await.map_err(map_sqlx)?;
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(invalid_rotation("invalid"));
        };
        sqlx::query("UPDATE eye_refresh_tokens SET used_at_ms=$2 WHERE token_hash=$1")
            .bind(decode_hash(old_token_hash)?).bind(now).execute(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query("INSERT INTO eye_refresh_tokens(token_hash,family_id,user_id,role,scopes,event_id,issued_at_ms,expires_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(decode_hash(new_token_hash)?).bind(&old.family_id).bind(&old.user_id).bind(&old.role).bind(&old.scopes).bind(&old.event_id).bind(now).bind(to_i64(new_expires_at_ms)?)
            .execute(&mut *tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(RefreshRotation {
            outcome:"ok".to_owned(),user_id:Some(user.user_id),email:Some(user.email),role:Some(parse_role(&user.role)?),
            scopes:Some(user.scopes),is_active:Some(user.is_active),created_at_ms:Some(to_u64(user.created_at_ms)?),
            event_id:old.event_id,family_id:Some(old.family_id),
        })
    }
}

fn invalid_rotation(outcome: &str) -> RefreshRotation {
    RefreshRotation { outcome:outcome.to_owned(),user_id:None,email:None,role:None,scopes:None,is_active:None,created_at_ms:None,event_id:None,family_id:None }
}
fn user_row(row: UserRow) -> Result<DurableUser, DurableStoreError> {
    Ok(DurableUser { user_id:row.user_id,email:row.email,role:parse_role(&row.role)?,scopes:row.scopes,is_active:row.is_active,created_at_ms:to_u64(row.created_at_ms)? })
}
fn parse_role(role: &str) -> Result<UserRole, DurableStoreError> {
    match role { "GateOperator"=>Ok(UserRole::GateOperator),"Admin"=>Ok(UserRole::Admin),"Auditor"=>Ok(UserRole::Auditor),_=>Err(DurableStoreError::InvalidRecord) }
}
fn to_i64(value: u64) -> Result<i64, DurableStoreError> { i64::try_from(value).map_err(|_| DurableStoreError::InvalidRecord) }
fn to_u64(value: i64) -> Result<u64, DurableStoreError> { u64::try_from(value).map_err(|_| DurableStoreError::InvalidRecord) }
fn decode_hash(value: &str) -> Result<Vec<u8>, DurableStoreError> {
    if value.len()!=64 { return Err(DurableStoreError::InvalidRecord); }
    (0..64).step_by(2).map(|i| u8::from_str_radix(&value[i..i+2],16).map_err(|_| DurableStoreError::InvalidRecord)).collect()
}
fn map_sqlx(error: sqlx::Error) -> DurableStoreError {
    if let sqlx::Error::Database(db) = &error {
        if db.code().as_deref() == Some("23505") { return DurableStoreError::Rejected; }
    }
    DurableStoreError::Unavailable
}

pub fn hash_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(value.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}
pub fn role_name(role: UserRole) -> &'static str {
    match role { UserRole::GateOperator=>"GateOperator",UserRole::Admin=>"Admin",UserRole::Auditor=>"Auditor" }
}
pub fn new_oauth_user(email: &str, now_ms: u64) -> Result<UserRecord, DurableStoreError> {
    let salt=crate::auth::google::random_urlsafe(32).map_err(|_|DurableStoreError::Unavailable)?;
    let password=crate::auth::google::random_urlsafe(32).map_err(|_|DurableStoreError::Unavailable)?;
    Ok(UserRecord {
        user_id:format!("usr_{}",crate::auth::google::random_urlsafe(16).map_err(|_|DurableStoreError::Unavailable)?),
        email:email.to_owned(),password_hash:hash_password(&password,&salt),salt,role:UserRole::GateOperator,
        scopes:Vec::new(),is_active:true,created_at_ms:now_ms,
    })
}
