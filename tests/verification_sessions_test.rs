//! Contract and tenant-isolation integration tests for identity scopes and sessions (#186).
use axum::{body::{Body,to_bytes},http::{Request,StatusCode},response::Response,Router};
use king_sparkon_eye::{config::Config,identity,persistence::{connect,migrate,tenants::{create_tenant,TenantId}}};
use serde_json::{Value,json};
use std::collections::HashMap;
use tower::ServiceExt;
use uuid::Uuid;

fn config(url:String)->Config{
 let values=HashMap::from([
 ("EYE_GOOGLE_CLIENT_ID","client-id".to_owned()),("EYE_GOOGLE_CLIENT_SECRET","client-secret".to_owned()),
 ("EYE_GOOGLE_REDIRECT_URL","https://eye.example.com/v1/auth/google/callback".to_owned()),
 ("EYE_DATABASE_URL",url),("EYE_OAUTH_ENCRYPTION_KEY","AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned()),
 ("EYE_OAUTH_CALLBACK_URLS","https://eye.example.com/auth/callback".to_owned()),
 ("EYE_OAUTH_EXCHANGE_SECRET","e".repeat(48)),("EYE_JWT_SIGNING_SECRET","j".repeat(48)),
 ("EYE_JWT_KID","verification-test-kid".to_owned())]);
 Config::from_lookup(|name|values.get(name).cloned()).expect("valid test config")
}
async fn pool()->Option<sqlx::PgPool>{
 let Ok(url)=std::env::var("EYE_TEST_DATABASE_URL") else {eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set");return None};
 let pool=connect(&url).await.expect("connect Eye test database");migrate(&pool).await.expect("migrate");Some(pool)
}
async fn owner(pool:&sqlx::PgPool)->String{
 let id=format!("usr_{}",Uuid::new_v4().simple());
 sqlx::query("INSERT INTO eye_auth_users(user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms) VALUES($1,$2,'test-hash','','Admin',ARRAY[]::text[],TRUE,1)")
 .bind(&id).bind(format!("{id}@example.test")).execute(pool).await.unwrap();id
}
async fn client_key(pool:&sqlx::PgPool,tenant:&TenantId,owner:&str)->String{
 use king_sparkon_eye::persistence::api_credentials::{create_api_client,create_api_credential,CreateApiCredentialInput};
 let client=create_api_client(pool,tenant,owner,"verification integration client",
 vec!["scopes:write".into(),"scopes:read".into(),"verification:create".into(),"verification:read".into(),"identities:write".into(),"identities:read".into()],
 600,1_800_000_000_000).await.unwrap();
 create_api_credential(pool,CreateApiCredentialInput{tenant_id:tenant,client_id:&client.client_id,actor_user_id:owner,
 environment:"test",scopes:vec!["scopes:write".into(),"scopes:read".into(),"verification:create".into(),"verification:read".into(),"identities:write".into(),"identities:read".into()],
 ttl_ms:None,now_ms:1_800_000_000_001}).await.unwrap().raw_secret
}
async fn send(app:&Router,method:&str,path:&str,key:&str,body:Option<Value>,idempotency:Option<&str>)->(StatusCode,Value){
 let mut b=Request::builder().method(method).uri(path).header("authorization",format!("Bearer {key}"));
 if body.is_some(){b=b.header("content-type","application/json");}
 if let Some(k)=idempotency{b=b.header("idempotency-key",k);}
 let req=b.body(body.map(|v|Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap();
 let response:Response=app.clone().oneshot(req).await.unwrap();let status=response.status();
 let bytes=to_bytes(response.into_body(),64*1024).await.unwrap();
 (status,if bytes.is_empty(){Value::Null}else{serde_json::from_slice(&bytes).unwrap()})
}
#[tokio::test]
async fn scopes_and_verification_sessions_are_tenant_bound_and_idempotent(){
 let Some(pool)=pool().await else{return};
 let owner_a=owner(&pool).await;let owner_b=owner(&pool).await;
 let tenant_a= create_tenant(&pool,&owner_a,"scope tenant A",1_800_000_000_000).await.unwrap().tenant_id;
 let tenant_b= create_tenant(&pool,&owner_b,"scope tenant B",1_800_000_000_000).await.unwrap().tenant_id;
 let key_a=client_key(&pool,&tenant_a,&owner_a).await;let key_b=client_key(&pool,&tenant_b,&owner_b).await;
 let app=identity::router(config(std::env::var("EYE_TEST_DATABASE_URL").unwrap()));
 let (status,identity)=send(&app,"POST","/v1/identities",&key_a,Some(json!({"externalReference":"person-001"})),None).await;
 assert_eq!(status,StatusCode::CREATED);let identity_id=identity["identityId"].as_str().unwrap();
 let (status,scope)=send(&app,"POST","/v1/identity-scopes",&key_a,Some(json!({"externalReference":"group-001","displayName":"Approved identities"})),None).await;
 assert_eq!(status,StatusCode::CREATED);let scope_id=scope["scopeId"].as_str().unwrap();
 let path=format!("/v1/identity-scopes/{scope_id}/members/{identity_id}");
 assert_eq!(send(&app,"PUT",&path,&key_a,None,None).await.0,StatusCode::NO_CONTENT);
 assert_eq!(send(&app,"PUT",&path,&key_a,None,None).await.0,StatusCode::NO_CONTENT);
 assert_eq!(send(&app,"DELETE",&path,&key_b,None,None).await.0,StatusCode::NOT_FOUND);
 let (status,session)=send(&app,"POST","/v1/verification-sessions",&key_a,Some(json!({
  "mode":"IDENTIFY","scopeId":scope_id,"purpose":"generic_check","context":{"correlation":"opaque-001"},"expiresInMs":300000
 })),Some("verify-idem-001")).await;
 assert_eq!(status,StatusCode::CREATED);assert_eq!(session["status"],"BLOCKED_EVALUATION");assert_eq!(session["reasonCode"],"EVALUATION_DATA_REQUIRED");
 assert!(session.get("similarityScore").is_none());assert!(session.get("embedding").is_none());
 let session_id=session["sessionId"].as_str().unwrap();
 let (status,replay)=send(&app,"POST","/v1/verification-sessions",&key_a,Some(json!({
  "mode":"IDENTIFY","scopeId":scope_id,"purpose":"generic_check","context":{"correlation":"opaque-001"},"expiresInMs":300000
 })),Some("verify-idem-001")).await;
 assert_eq!(status,StatusCode::OK);assert_eq!(replay["sessionId"],session_id);
 assert_eq!(send(&app,"POST","/v1/verification-sessions",&key_a,Some(json!({"mode":"MATCH","identityId":identity_id})),Some("verify-idem-001")).await.0,StatusCode::CONFLICT);
 assert_eq!(send(&app,"GET",&format!("/v1/verification-sessions/{session_id}"),&key_b,None,None).await.0,StatusCode::NOT_FOUND);
 let (status,_)=send(&app,"POST",&format!("/v1/verification-sessions/{session_id}/captures"),&key_a,None,None).await;
 assert_eq!(status,StatusCode::SERVICE_UNAVAILABLE,"#95 gate must reject capture before body parsing");
 let (status,cancelled)=send(&app,"DELETE",&format!("/v1/verification-sessions/{session_id}"),&key_a,None,None).await;
 assert_eq!(status,StatusCode::OK);assert_eq!(cancelled["status"],"CANCELLED");
}
#[tokio::test]
async fn match_session_requires_exact_active_identity_and_identify_requires_scope(){
 let Some(pool)=pool().await else{return};
 let owner=owner(&pool).await;let tenant=create_tenant(&pool,&owner,"session target tenant",1_800_000_000_000).await.unwrap().tenant_id;
 let key=client_key(&pool,&tenant,&owner).await;let app=identity::router(config(std::env::var("EYE_TEST_DATABASE_URL").unwrap()));
 assert_eq!(send(&app,"POST","/v1/verification-sessions",&key,Some(json!({"mode":"IDENTIFY"})),Some("missing-scope")).await.0,StatusCode::BAD_REQUEST);
 assert_eq!(send(&app,"POST","/v1/verification-sessions",&key,Some(json!({"mode":"MATCH","identityId":"idy_00000000000000000000000000000000"})),Some("missing-identity")).await.0,StatusCode::NOT_FOUND);
}
