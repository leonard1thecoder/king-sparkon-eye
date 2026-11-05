//! Browser pages for Eye accounts: sign-in, registration, password recovery and the Google
//! OAuth callback landing page.
//!
//! The pages are static HTML with small inline scripts. They call only the JSON endpoints under
//! `/v1/auth`. Query parameters are never rendered into the markup: status messages are fixed
//! strings chosen in script and written with `textContent`. Routes whose endpoints are not yet
//! mounted report "not available on this server yet" rather than pretending to succeed.

use axum::Router;
use axum::response::Html;
use axum::routing::get;

use crate::api::AppState;

/// Minimum password length. Matches `MIN_PASSWORD_LEN` in `auth/mod.rs`.
const MIN_PASSWORD_LEN: usize = 10;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/sign-in", get(sign_in))
        .route("/auth/register", get(register))
        .route("/auth/forgot-password", get(forgot_password))
        .route("/auth/reset-password", get(reset_password))
        .route("/auth/callback", get(callback))
}

async fn sign_in() -> Html<String> {
    render("Sign in", "Sign in", SIGN_IN_BODY, SIGN_IN_SCRIPT)
}

async fn register() -> Html<String> {
    render(
        "Create account",
        "Create account",
        REGISTER_BODY,
        REGISTER_SCRIPT,
    )
}

async fn forgot_password() -> Html<String> {
    render(
        "Forgot password",
        "Reset your password",
        FORGOT_BODY,
        FORGOT_SCRIPT,
    )
}

async fn reset_password() -> Html<String> {
    render(
        "Choose a new password",
        "Choose a new password",
        RESET_BODY,
        RESET_SCRIPT,
    )
}

async fn callback() -> Html<String> {
    render(
        "Google sign-in",
        "Google sign-in",
        CALLBACK_BODY,
        CALLBACK_SCRIPT,
    )
}

fn render(title: &str, heading: &str, body: &str, script: &str) -> Html<String> {
    let script = format!("{COMMON_SCRIPT}\n{script}");
    let html = TEMPLATE
        .replace("{{TITLE}}", title)
        .replace("{{HEADING}}", heading)
        .replace("{{BODY}}", body)
        .replace("{{SCRIPT}}", &script);
    Html(html.replace("{{MIN_PASSWORD}}", &MIN_PASSWORD_LEN.to_string()))
}

const TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex">
<title>{{TITLE}} · King Sparkon Eye</title>
<style>
:root{color-scheme:light dark;--bg:#f4f2eb;--fg:#1d1c19;--muted:#5b5850;--line:#c9c4b6;--accent:#2f5d50;--bad:#8a2b1f;--ok:#2f5d50;--gold:#d99b08}
@media (prefers-color-scheme:dark){:root{--bg:#171714;--fg:#ece9df;--muted:#aca89a;--line:#4a473e;--accent:#8ec5b1;--bad:#ff9b8a;--ok:#8ec5b1}}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.5 system-ui,sans-serif}
main{max-width:26rem;margin:3rem auto;padding:0 1rem}
.auth-brand{display:inline-flex;align-items:center;gap:.7rem;margin:0 0 2rem;color:var(--fg);text-decoration:none}\n.auth-brand:focus-visible{outline:3px solid var(--gold);outline-offset:4px;border-radius:4px}
.auth-brand-mark{display:block;flex:0 0 auto;width:46px;height:30px}
.auth-brand-mark .eye-shell{fill:#d99b08}
.auth-brand-mark .eye-core{fill:#17221e}
.auth-brand-mark .iris{fill:#303640;stroke:#f3b91b;stroke-width:3}
.auth-brand-mark .bolt{fill:#ffd23f}
.auth-brand-copy{display:grid;gap:.08rem;font-size:.86rem;font-weight:760;letter-spacing:-.025em}
.auth-brand-kicker{color:var(--muted);font:9px monospace;letter-spacing:.14em;text-transform:uppercase}
h1{font-size:1.5rem;margin:0 0 1rem}
label{display:block;margin:1rem 0 .25rem;font-weight:600}
input{width:100%;box-sizing:border-box;padding:.6rem;font:inherit;border:1px solid var(--line);border-radius:6px;background:transparent;color:inherit}
button,.button{display:inline-block;margin-top:1.25rem;padding:.65rem 1rem;font:inherit;font-weight:600;border:1px solid var(--accent);border-radius:6px;background:var(--accent);color:#fff;text-decoration:none;cursor:pointer;box-sizing:border-box}
button[disabled]{opacity:.6;cursor:wait}
.secondary{background:transparent;color:var(--accent)}
.links{margin-top:1.5rem;font-size:.95rem;color:var(--muted)}
.links a{color:var(--accent)}
.hint{font-size:.9rem;color:var(--muted);margin:.25rem 0 0}
.field-error{color:var(--bad);font-size:.9rem;margin:.25rem 0 0}
.notice{margin-top:1rem;padding:.75rem;border-radius:6px;border:1px solid var(--line)}
.notice.error{color:var(--bad);border-color:var(--bad)}
.notice.ok{color:var(--ok);border-color:var(--ok)}
[hidden]{display:none!important}
</style>
</head>
<body>
<main>
<a class="auth-brand" href="/" aria-label="King Sparkon Eye home">
<svg class="auth-brand-mark" viewBox="0 0 120 72" aria-hidden="true" focusable="false" xmlns="http://www.w3.org/2000/svg">
<path class="eye-shell" d="M5 36Q60 -4 115 36Q60 76 5 36Z"/>
<path class="eye-core" d="M18 36Q60 8 102 36Q60 64 18 36Z"/>
<circle class="iris" cx="60" cy="36" r="20"/>
<path class="bolt" d="M64 16L49 39H60L54 57L75 31H63L70 16Z"/>
</svg>
<span class="auth-brand-copy"><span>King Sparkon Eye</span><span class="auth-brand-kicker">Vision service</span></span>
</a>
<h1>{{HEADING}}</h1>
{{BODY}}
</main>
<script>
{{SCRIPT}}
</script>
</body>
</html>
"##;

const COMMON_SCRIPT: &str = r##"
const MIN_PASSWORD = {{MIN_PASSWORD}};
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
function byId(id) { return document.getElementById(id); }
function notice(text, kind) {
  const el = byId('notice');
  el.textContent = text;
  el.className = 'notice ' + kind;
  el.hidden = false;
}
function fieldError(id, message) {
  const el = byId(id + '-error');
  el.textContent = message || '';
  el.hidden = !message;
  return !message;
}
function checkEmail() {
  const value = byId('email').value.trim();
  return fieldError('email', EMAIL_RE.test(value) ? '' : 'Enter a valid email address.');
}
function checkPassword(requireLength) {
  const value = byId('password').value;
  if (!value) return fieldError('password', 'Enter your password.');
  if (requireLength && value.length < MIN_PASSWORD) {
    return fieldError('password', 'Use at least ' + MIN_PASSWORD + ' characters.');
  }
  return fieldError('password', '');
}
function checkConfirm() {
  const matches = byId('password').value === byId('confirm').value;
  return fieldError('confirm', matches ? '' : 'The passwords do not match.');
}
async function post(url, body) {
  return fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'accept': 'application/json' },
    credentials: 'same-origin',
    body: JSON.stringify(body),
  });
}
function failureMessage(status) {
  if (status === 404) return 'This is not available on this server yet.';
  if (status === 429) return 'Too many attempts. Please wait a few minutes and try again.';
  if (status >= 500) return 'The service is temporarily unavailable. Please try again shortly.';
  return null;
}
function busy(button, value) { button.disabled = value; }
"##;

const SIGN_IN_BODY: &str = r##"<form id="sign-in" novalidate>
<label for="email">Email</label>
<input id="email" name="email" type="email" autocomplete="email" required>
<p id="email-error" class="field-error" hidden></p>
<label for="password">Password</label>
<input id="password" name="password" type="password" autocomplete="current-password" required>
<p id="password-error" class="field-error" hidden></p>
<button type="submit">Sign in</button>
</form>
<a id="google" class="button secondary" href="#">Continue with Google</a>
<div id="notice" class="notice" role="status" aria-live="polite" hidden></div>
<p class="links"><a href="/auth/forgot-password">Forgot password?</a> · <a href="/auth/register">Create an account</a></p>"##;

const SIGN_IN_SCRIPT: &str = r##"
byId('google').href = '/v1/auth/google/start?callback_url=' + encodeURIComponent(location.origin + '/auth/callback');
byId('sign-in').addEventListener('submit', async (event) => {
  event.preventDefault();
  const emailOk = checkEmail();
  const passwordOk = checkPassword(false);
  if (!emailOk || !passwordOk) return;
  const button = event.submitter || event.target.querySelector('button');
  busy(button, true);
  try {
    const response = await post('/v1/auth/login', {
      email: byId('email').value.trim(),
      password: byId('password').value,
    });
    if (response.ok) {
      byId('password').value = '';
      notice('Signed in.', 'ok');
    } else {
      notice(failureMessage(response.status) || 'Those details did not match an account.', 'error');
    }
  } catch (_) {
    notice('Network error. Check your connection and try again.', 'error');
  } finally {
    busy(button, false);
  }
});
"##;

const REGISTER_BODY: &str = r##"<form id="register" novalidate>
<label for="email">Email</label>
<input id="email" name="email" type="email" autocomplete="email" required>
<p id="email-error" class="field-error" hidden></p>
<label for="password">Password</label>
<input id="password" name="password" type="password" autocomplete="new-password" required aria-describedby="password-hint">
<p id="password-hint" class="hint">At least {{MIN_PASSWORD}} characters.</p>
<p id="password-error" class="field-error" hidden></p>
<label for="confirm">Confirm password</label>
<input id="confirm" name="confirm" type="password" autocomplete="new-password" required>
<p id="confirm-error" class="field-error" hidden></p>
<button type="submit">Create account</button>
</form>
<div id="notice" class="notice" role="status" aria-live="polite" hidden></div>
<p class="links">Already have an account? <a href="/auth/sign-in">Sign in</a></p>"##;

const REGISTER_SCRIPT: &str = r##"
byId('register').addEventListener('submit', async (event) => {
  event.preventDefault();
  const emailOk = checkEmail();
  const passwordOk = checkPassword(true);
  const confirmOk = checkConfirm();
  if (!emailOk || !passwordOk || !confirmOk) return;
  const button = event.submitter || event.target.querySelector('button');
  busy(button, true);
  try {
    const response = await post('/v1/auth/register', {
      email: byId('email').value.trim(),
      password: byId('password').value,
    });
    if (response.ok) {
      event.target.reset();
      notice('Account created. You can now sign in.', 'ok');
    } else {
      notice(failureMessage(response.status) || 'We could not create the account. Check the details and try again.', 'error');
    }
  } catch (_) {
    notice('Network error. Check your connection and try again.', 'error');
  } finally {
    busy(button, false);
  }
});
"##;

const FORGOT_BODY: &str = r##"<form id="forgot" novalidate>
<label for="email">Email</label>
<input id="email" name="email" type="email" autocomplete="email" required>
<p id="email-error" class="field-error" hidden></p>
<button type="submit">Send reset instructions</button>
</form>
<div id="notice" class="notice" role="status" aria-live="polite" hidden></div>
<p class="links"><a href="/auth/sign-in">Back to sign in</a></p>"##;

const FORGOT_SCRIPT: &str = r##"
byId('forgot').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (!checkEmail()) return;
  const button = event.submitter || event.target.querySelector('button');
  busy(button, true);
  try {
    const response = await post('/v1/auth/password-reset/request', { email: byId('email').value.trim() });
    if (response.ok) {
      notice('If an account exists for that email, reset instructions are on their way.', 'ok');
    } else {
      notice(failureMessage(response.status) || 'We could not process the request. Please try again.', 'error');
    }
  } catch (_) {
    notice('Network error. Check your connection and try again.', 'error');
  } finally {
    busy(button, false);
  }
});
"##;

const RESET_BODY: &str = r##"<form id="reset" novalidate>
<label for="password">New password</label>
<input id="password" name="password" type="password" autocomplete="new-password" required aria-describedby="password-hint">
<p id="password-hint" class="hint">At least {{MIN_PASSWORD}} characters.</p>
<p id="password-error" class="field-error" hidden></p>
<label for="confirm">Confirm new password</label>
<input id="confirm" name="confirm" type="password" autocomplete="new-password" required>
<p id="confirm-error" class="field-error" hidden></p>
<button type="submit">Set new password</button>
</form>
<div id="notice" class="notice" role="status" aria-live="polite" hidden></div>
<p class="links"><a href="/auth/sign-in">Back to sign in</a></p>"##;

const RESET_SCRIPT: &str = r##"
const resetToken = new URLSearchParams(location.search).get('token') || '';
if (!resetToken) {
  byId('reset').hidden = true;
  notice('This link is missing its reset token. Request a new one.', 'error');
}
byId('reset').addEventListener('submit', async (event) => {
  event.preventDefault();
  const passwordOk = checkPassword(true);
  const confirmOk = checkConfirm();
  if (!passwordOk || !confirmOk) return;
  const button = event.submitter || event.target.querySelector('button');
  busy(button, true);
  try {
    const response = await post('/v1/auth/password-reset/confirm', {
      token: resetToken,
      password: byId('password').value,
    });
    if (response.ok) {
      event.target.reset();
      notice('Your password has been changed. You can now sign in.', 'ok');
    } else {
      notice(failureMessage(response.status) || 'This reset link is invalid or has expired. Request a new one.', 'error');
    }
  } catch (_) {
    notice('Network error. Check your connection and try again.', 'error');
  } finally {
    busy(button, false);
  }
});
"##;

const CALLBACK_BODY: &str = r##"<div id="notice" class="notice" role="status" aria-live="polite" hidden></div>
<p class="links"><a href="/auth/sign-in">Back to sign in</a></p>"##;

const CALLBACK_SCRIPT: &str = r##"
const params = new URLSearchParams(location.search);
if (params.has('error')) {
  // Provider and server failures are reported as a fixed code; no detail is shown.
  notice('Google sign-in did not complete. Please try again.', 'error');
} else if (params.has('code')) {
  notice('Google sign-in succeeded. Completing the session needs a trusted server step that is not available on this server yet.', 'ok');
} else {
  notice('This page completes Google sign-in. Start from the sign-in page.', 'error');
}
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_placeholder_is_filled_in_every_page() {
        for html in [
            render("t", "h", SIGN_IN_BODY, SIGN_IN_SCRIPT).0,
            render("t", "h", REGISTER_BODY, REGISTER_SCRIPT).0,
            render("t", "h", FORGOT_BODY, FORGOT_SCRIPT).0,
            render("t", "h", RESET_BODY, RESET_SCRIPT).0,
            render("t", "h", CALLBACK_BODY, CALLBACK_SCRIPT).0,
        ] {
            assert!(!html.contains("{{"), "unfilled placeholder in page");
        }
    }

    #[test]
    fn all_auth_pages_render_the_shared_eye_brand() {
        for html in [
            render("t", "h", SIGN_IN_BODY, SIGN_IN_SCRIPT).0,
            render("t", "h", REGISTER_BODY, REGISTER_SCRIPT).0,
            render("t", "h", FORGOT_BODY, FORGOT_SCRIPT).0,
            render("t", "h", RESET_BODY, RESET_SCRIPT).0,
            render("t", "h", CALLBACK_BODY, CALLBACK_SCRIPT).0,
        ] {
            assert!(html.contains("class=\"auth-brand\""));
            assert!(html.contains("King Sparkon Eye"));
            assert!(html.contains("auth-brand-mark"));
            assert!(html.contains("aria-label=\"King Sparkon Eye home\""));
            assert!(html.contains("color-scheme:light dark"));
            assert!(html.contains("prefers-color-scheme:dark"));
        }
    }

    #[test]
    fn password_rules_match_server_minimum() {
        assert_eq!(MIN_PASSWORD_LEN, crate::auth::MIN_PASSWORD_LEN);
    }
}
