//! Technical SEO for the public site.
//!
//! Keep the production origin explicit and verified against the repository's configured
//! homepage. Only routes that exist, are public, and have crawlable content belong in the sitemap.

use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use crate::api::AppState;

/// Canonical origin configured in the GitHub repository homepage setting.
pub const PRODUCTION_ORIGIN: &str = "https://king-sparkon-eye.vercel.app";

/// Routes that search engines and other crawlers may use to discover public content.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/robots.txt", get(robots_txt))
        .route("/sitemap.xml", get(sitemap_xml))
}

async fn robots_txt() -> impl IntoResponse {
    let body = format!(
        "User-agent: *\n\
         Allow: /\n\
         Disallow: /auth/\n\
         Disallow: /v1/\n\
         Disallow: /health/\n\
         Disallow: /console/\n\
         Disallow: /account/\n\
         Disallow: /sessions/\n\
         Disallow: /tenants/\n\
         Disallow: /verification/\n\
         \n\
         Sitemap: {PRODUCTION_ORIGIN}/sitemap.xml\n"
    );
    ([(CONTENT_TYPE, "text/plain; charset=utf-8")], body)
}

async fn sitemap_xml() -> impl IntoResponse {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n\
           <url><loc>{PRODUCTION_ORIGIN}/</loc></url>\n\
         </urlset>\n"
    );
    ([(CONTENT_TYPE, "application/xml; charset=utf-8")], body)
}
