use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use king_sparkon_eye::{api, config::Config, seo::PRODUCTION_ORIGIN};
use tower::ServiceExt;

fn app() -> axum::Router {
    let config = Config::from_lookup(|_: &str| None::<String>).expect("defaults are valid");
    api::router(config)
}

async fn request(
    method: Method,
    path: &str,
) -> (StatusCode, Option<String>, Option<String>, Vec<u8>) {
    let response = app()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let robots = response
        .headers()
        .get("x-robots-tag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, content_type, robots, body)
}

#[tokio::test]
async fn robots_txt_allows_public_content_and_excludes_private_route_classes() {
    let (status, content_type, _, body) = request(Method::GET, "/robots.txt").await;
    let robots = String::from_utf8(body).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/plain; charset=utf-8"));
    assert!(robots.contains("User-agent: *\n"));
    assert!(robots.contains("Allow: /\n"));
    for path in [
        "/auth/",
        "/v1/",
        "/health/",
        "/console/",
        "/account/",
        "/sessions/",
        "/tenants/",
        "/verification/",
    ] {
        assert!(
            robots.contains(&format!("Disallow: {path}\n")),
            "missing disallow for {path}"
        );
    }
    assert!(robots.contains(&format!("Sitemap: {PRODUCTION_ORIGIN}/sitemap.xml")));
    assert!(!robots.contains("Disallow: /assets/"));
}

#[tokio::test]
async fn sitemap_is_valid_xml_and_contains_only_the_canonical_public_homepage() {
    let (status, content_type, _, body) = request(Method::GET, "/sitemap.xml").await;
    let sitemap = String::from_utf8(body).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        content_type.as_deref(),
        Some("application/xml; charset=utf-8")
    );
    assert!(sitemap.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(sitemap.contains("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">"));
    assert!(sitemap.contains(&format!("<loc>{PRODUCTION_ORIGIN}/</loc>")));
    assert_eq!(
        sitemap.matches("<url>").count(),
        1,
        "only the implemented public homepage is indexable"
    );
    assert!(!sitemap.contains("/auth/"));
    assert!(!sitemap.contains("/v1/"));
    assert!(sitemap.ends_with("</urlset>\n"));
}

#[tokio::test]
async fn homepage_has_canonical_social_and_structured_metadata() {
    let (status, _, robots, body) = request(Method::GET, "/").await;
    let page = String::from_utf8(body).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(robots, None, "the public homepage must remain indexable");
    let canonical = format!("rel=\"canonical\" href=\"{PRODUCTION_ORIGIN}/");
    let og_url = format!("property=\"og:url\" content=\"{PRODUCTION_ORIGIN}/");
    let og_image = format!("property=\"og:image\" content=\"{PRODUCTION_ORIGIN}/assets/hero.jpg");
    for expected in [
        "<title>King Sparkon Eye | Face-Based Identity Verification Platform</title>",
        "name=\"description\"",
        "name=\"robots\" content=\"index,follow\"",
        canonical.as_str(),
        og_url.as_str(),
        og_image.as_str(),
        "name=\"twitter:card\" content=\"summary_large_image\"",
        "application/ld+json",
        "\"@type\": \"WebSite\"",
    ] {
        assert!(
            page.contains(expected),
            "homepage missing SEO metadata: {expected}"
        );
    }
    assert!(page.contains("Face similarity is probabilistic"));
    assert!(page.contains("not proof of legal identity, liveness"));
    assert!(!page.contains("event-scoped attendee identification"));
    assert!(!page.contains("Spring Boot"));
}

#[tokio::test]
async fn account_pages_are_explicitly_non_indexable() {
    let (status, _, robots, body) = request(Method::GET, "/auth/sign-in").await;
    let page = String::from_utf8(body).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(robots.as_deref(), Some("noindex, nofollow, noarchive"));
    assert!(page.contains("<meta name=\"robots\" content=\"noindex,nofollow,noarchive\">"));
}

#[tokio::test]
async fn public_share_image_remains_accessible_to_preview_crawlers() {
    let (status, content_type, robots, body) = request(Method::GET, "/assets/hero.jpg").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/jpeg"));
    assert_eq!(robots, None);
    assert!(body.starts_with(&[0xFF, 0xD8, 0xFF]));
}

#[tokio::test]
async fn branded_favicon_is_served_as_svg() {
    let (status, content_type, robots, body) = request(Method::GET, "/assets/favicon.svg").await;
    let svg = String::from_utf8(body).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/svg+xml"));
    assert_eq!(robots, None);
    assert!(svg.starts_with("<svg "));
    assert!(svg.contains("viewBox=\"0 0 64 64\""));
}

#[tokio::test]
async fn unknown_routes_return_a_real_not_found_status() {
    let (status, _, _, body) = request(Method::GET, "/not-a-public-page").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.is_empty());
}

#[tokio::test]
async fn health_and_api_responses_are_non_indexable() {
    let (health_status, _, health_robots, _) = request(Method::GET, "/health/live").await;
    assert_eq!(health_status, StatusCode::OK);
    assert_eq!(
        health_robots.as_deref(),
        Some("noindex, nofollow, noarchive")
    );

    let (api_status, _, api_robots, _) = request(Method::POST, "/v1/enrollment/photo-assessments").await;
    assert_eq!(api_status, StatusCode::UNAUTHORIZED);
    assert_eq!(api_robots.as_deref(), Some("noindex, nofollow, noarchive"));
}
