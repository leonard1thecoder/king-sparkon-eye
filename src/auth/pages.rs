//! Browser pages for Eye accounts: sign-in, registration, password recovery and the Google
//! OAuth callback landing page.
//!
//! The pages are static HTML with small inline scripts. They call only the JSON endpoints under
//! `/v1/auth`. Query parameters are never rendered into the markup: status messages are fixed
//! strings chosen in script and written with `textContent`.
//!
//! Routes whose endpoints are not mounted on this server are shown as unavailable: the form is
//! disabled inside a `<fieldset disabled>` and the page says so, rather than inviting a submission
//! that cannot succeed. The visual system follows the public landing page (black canvas, gold
//! accent, pill buttons, one focus ring).

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
:root{color-scheme:dark;--paper:#000;--panel:#0c0c0a;--ink:#f5f3ec;--muted:#a4a59b;--line:#2a2b24;--gold:#e9ae22;--gold-2:#ffc94d;--on-accent:#0a0a08;--bad:#ff9b8a;--ok:#8ec5b1}
body{margin:0;background:var(--paper);color:var(--ink);font:16px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:26rem;margin:0 auto;padding:2.5rem 1rem 3rem}
.auth-brand{display:inline-flex;align-items:center;gap:.7rem;margin:0 0 2rem;color:var(--ink);text-decoration:none}
.auth-brand-mark{display:block;flex:0 0 auto;width:46px;height:30px}
.auth-brand-mark .eye-shell{fill:#d99b08}
.auth-brand-mark .eye-core{fill:#17221e}
.auth-brand-mark .iris{fill:#303640;stroke:#f3b91b;stroke-width:3}
.auth-brand-mark .bolt{fill:#ffd23f}
.auth-brand-copy{display:grid;gap:.08rem;font-size:.86rem;font-weight:760;letter-spacing:-.025em}
.auth-brand-kicker{color:var(--muted);font:.7rem monospace;letter-spacing:.14em;text-transform:uppercase}
a{color:var(--gold-2)}
a:focus-visible,button:focus-visible,input:focus-visible,.auth-brand:focus-visible{outline:3px solid var(--gold-2);outline-offset:3px}
h1{font-size:1.6rem;line-height:1.2;margin:0 0 1.25rem}
fieldset{border:0;margin:0;padding:0;min-width:0}
fieldset:disabled{opacity:.55}
label{display:block;margin:1.1rem 0 .35rem;font-weight:600}
input{width:100%;box-sizing:border-box;min-height:44px;padding:.6rem .8rem;font:inherit;font-size:1rem;border:1px solid var(--line);border-radius:10px;background:var(--panel);color:var(--ink)}
input[aria-invalid="true"]{border-color:var(--bad)}
button,.button{display:flex;align-items:center;justify-content:center;width:100%;min-height:44px;margin-top:1.4rem;padding:.6rem 1.25rem;box-sizing:border-box;font:inherit;font-weight:700;border:1px solid var(--gold);border-radius:999px;background:var(--gold);color:var(--on-accent);text-decoration:none;cursor:pointer}
button:hover{background:var(--gold-2);border-color:var(--gold-2)}
button:disabled{cursor:not-allowed}
.button.secondary{background:transparent;color:var(--gold-2);margin-top:.9rem}
.button.secondary:hover{background:transparent;border-color:var(--gold-2)}
.links{margin-top:1.5rem;font-size:.95rem;color:var(--muted)}
.hint{font-size:.9rem;color:var(--muted);margin:.3rem 0 0}
.field-error{color:var(--bad);font-size:.9rem;margin:.35rem 0 0}
.notice{margin-top:1.1rem;padding:.8rem 1rem;border-radius:10px;border:1px solid var(--line);background:var(--panel)}
.notice.error{color:var(--bad);border-color:var(--bad)}
.notice.ok{color:var(--ok);border-color:var(--ok)}
.notice.unavailable{color:var(--ink);border-color:var(--gold)}
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
<noscript><p class="notice error">Turn on JavaScript to use this page.</p></noscript>
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
  const input = byId(id);
  const el = byId(id + '-error');
  el.textContent = message || '';
  el.hidden = !message;
  if (input) input.setAttribute('aria-invalid', message ? 'true' : 'false');
  return !message;
}
function focusFirstInvalid() {
  const first = document.querySelector('[aria-invalid="true"]');
  if (first) first.focus();
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
function busy(button, value, label) {
  if (!button) return;
  if (value) {
    button.dataset.idle = button.textContent;
    button.textContent = label;
  } else if (button.dataset.idle) {
    button.textContent = button.dataset.idle;
  }
  button.disabled = value;
}
"##;

const SIGN_IN_BODY: &str = r##"<form id="sign-in" novalidate>
<label for="email">Email</label>
<input id="email" name="email" type="email" autocomplete="email" required aria-describedby="email-error">
<p id="email-error" class="field-error" hidden></p>
<label for="password">Password</label>
<input id="password" name="password" type="password" autocomplete="current-password" required aria-describedby="password-error">
<p id="password-error" class="field-error" hidden></p>
<button type="submit">Sign in</button>
</form>
<a id="google" class="button secondary" href="#" hidden>Continue with Google</a>
<div id="notice" class="notice" role="status" aria-live="polite" hidden></div>
<p class="links"><a href="/auth/forgot-password">Forgot password?</a> · <a href="/auth/register">Create an account</a></p>"##;

const SIGN_IN_SCRIPT: &str = r##"
const google = byId('google');
google.href = '/v1/auth/google/start?callback_url=' + encodeURIComponent(location.origin + '/auth/callback');
google.hidden = false;
byId('sign-in').addEventListener('submit', async (event) => {
  event.preventDefault();
  const emailOk = checkEmail();
  const passwordOk = checkPassword(false);
  if (!emailOk || !passwordOk) {
    focusFirstInvalid();
    return;
  }
  const button = event.submitter || event.target.querySelector('button');
  busy(button, true, 'Signing in…');
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

const REGISTER_BODY: &str = r##"<p class="notice unavailable">Account creation is not available on this server yet. This form is disabled and nothing is sent.</p>
<form id="register" novalidate>
<fieldset disabled>
<label for="email">Email</label>
<input id="email" name="email" type="email" autocomplete="email" required aria-describedby="email-error">
<p id="email-error" class="field-error" hidden></p>
<label for="password">Password</label>
<input id="password" name="password" type="password" autocomplete="new-password" required aria-describedby="password-hint password-error">
<p id="password-hint" class="hint">At least {{MIN_PASSWORD}} characters.</p>
<p id="password-error" class="field-error" hidden></p>
<label for="confirm">Confirm password</label>
<input id="confirm" name="confirm" type="password" autocomplete="new-password" required aria-describedby="confirm-error">
<p id="confirm-error" class="field-error" hidden></p>
<button type="submit">Create account</button>
</fieldset>
</form>
<div id="notice" class="notice" role="status" aria-live="polite" hidden></div>
<p class="links">Already have an account? <a href="/auth/sign-in">Sign in</a></p>"##;

const REGISTER_SCRIPT: &str = r##"
byId('register').addEventListener('submit', async (event) => {
  event.preventDefault();
  const emailOk = checkEmail();
  const passwordOk = checkPassword(true);
  const confirmOk = checkConfirm();
  if (!emailOk || !passwordOk || !confirmOk) {
    focusFirstInvalid();
    return;
  }
  const button = event.submitter || event.target.querySelector('button');
  busy(button, true, 'Creating account…');
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

const FORGOT_BODY: &str = r##"<p class="notice unavailable">Password reset is not available on this server yet. This form is disabled and no email is sent.</p>
<form id="forgot" novalidate>
<fieldset disabled>
<label for="email">Email</label>
<input id="email" name="email" type="email" autocomplete="email" required aria-describedby="email-error">
<p id="email-error" class="field-error" hidden></p>
<button type="submit">Send reset instructions</button>
</fieldset>
</form>
<div id="notice" class="notice" role="status" aria-live="polite" hidden></div>
<p class="links"><a href="/auth/sign-in">Back to sign in</a></p>"##;

const FORGOT_SCRIPT: &str = r##"
byId('forgot').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (!checkEmail()) {
    focusFirstInvalid();
    return;
  }
  const button = event.submitter || event.target.querySelector('button');
  busy(button, true, 'Sending…');
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

const RESET_BODY: &str = r##"<p class="notice unavailable">Password reset is not available on this server yet. This form is disabled.</p>
<form id="reset" novalidate>
<fieldset disabled>
<label for="password">New password</label>
<input id="password" name="password" type="password" autocomplete="new-password" required aria-describedby="password-hint password-error">
<p id="password-hint" class="hint">At least {{MIN_PASSWORD}} characters.</p>
<p id="password-error" class="field-error" hidden></p>
<label for="confirm">Confirm new password</label>
<input id="confirm" name="confirm" type="password" autocomplete="new-password" required aria-describedby="confirm-error">
<p id="confirm-error" class="field-error" hidden></p>
<button type="submit">Set new password</button>
</fieldset>
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
  if (!passwordOk || !confirmOk) {
    focusFirstInvalid();
    return;
  }
  const button = event.submitter || event.target.querySelector('button');
  busy(button, true, 'Saving…');
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
  notice('Google returned a sign-in code, but you are not signed in yet. Completing the session needs a trusted server step that is not available on this server yet.', 'error');
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
            assert!(html.contains("color-scheme:dark"));
            assert!(html.contains("--gold:#e9ae22"));
        }
    }

    #[test]
    fn unavailable_pages_disable_their_forms_and_say_so() {
        for html in [
            render("t", "h", REGISTER_BODY, REGISTER_SCRIPT).0,
            render("t", "h", FORGOT_BODY, FORGOT_SCRIPT).0,
            render("t", "h", RESET_BODY, RESET_SCRIPT).0,
        ] {
            assert!(html.contains("class=\"notice unavailable\""));
            assert!(html.contains("not available on this server yet"));
            assert!(html.contains("<fieldset disabled>"));
        }
    }

    #[test]
    fn sign_in_hides_google_until_its_url_is_set() {
        let html = render("t", "h", SIGN_IN_BODY, SIGN_IN_SCRIPT).0;
        assert!(html.contains("id=\"google\" class=\"button secondary\" href=\"#\" hidden"));
        assert!(html.contains("google.hidden = false;"));
    }

    #[test]
    fn inputs_are_linked_to_their_error_messages() {
        let register = render("t", "h", REGISTER_BODY, REGISTER_SCRIPT).0;
        assert!(register.contains("aria-describedby=\"email-error\""));
        assert!(register.contains("aria-describedby=\"password-hint password-error\""));
        assert!(register.contains("aria-describedby=\"confirm-error\""));
        assert!(register.contains("aria-invalid"));
        let sign_in = render("t", "h", SIGN_IN_BODY, SIGN_IN_SCRIPT).0;
        assert!(sign_in.contains("aria-describedby=\"password-error\""));
    }

    #[test]
    fn pages_explain_when_javascript_is_off() {
        let html = render("t", "h", SIGN_IN_BODY, SIGN_IN_SCRIPT).0;
        assert!(html.contains("<noscript>"));
    }

    #[test]
    fn password_rules_match_server_minimum() {
        assert_eq!(MIN_PASSWORD_LEN, crate::auth::MIN_PASSWORD_LEN);
    }
}
