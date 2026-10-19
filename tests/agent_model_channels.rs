use serde_json::json;
use shellpilot_server::{agent::{provider::*,Repository,chat::{Manager,Request}},store::Store};
use std::sync::{Arc,Mutex};
use tokio_util::sync::CancellationToken;

fn config()->ModelConfig {serde_json::from_value(json!({"provider":"openAiCompatible","baseUrl":"http://127.0.0.1/v1/","completionsPath":"chat/completions","model":"one","outputTokenParameter":"maxTokens","capabilities":{"developerRole":false,"temperature":false,"reasoningEffort":false},"requireApiKey":false})).unwrap()}
fn update(config:ModelConfig)->ModelConfigUpdate {ModelConfigUpdate{config,api_key:KeyChange::Keep}}
fn ids(values:&[&str])->Vec<String>{values.iter().map(|v|(*v).into()).collect()}

#[test]
fn legacy_profile_upgrades_without_changing_model_identity_and_members_survive_reorder_reopen_and_removal() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("channels.sqlite");
    let store=Arc::new(Store::open(&path).unwrap());let repo=ModelRepository::new(store.clone());
    let legacy=repo.save_named("legacy","user",0,update(config()),Some("old")).unwrap();
    let session=Repository::new(store.clone()).create_general_session_with_policy("legacy","user",None).unwrap();
    assert_eq!(repo.channels("user").unwrap()[0].models[0].id,"legacy");
    let channel=repo.save_channel("legacy","user",legacy.revision,update(config()),"My proxy",ids(&["one","two"])).unwrap();
    assert_eq!(channel.models[0].id,"legacy");let second=channel.models[1].id.clone();
    let channel=repo.save_channel("legacy","user",channel.profile.revision,update(config()),"Renamed",ids(&["two","one"])).unwrap();
    assert_eq!(channel.models[0].id,second);assert_eq!(channel.models[1].id,"legacy");
    let channel=repo.save_channel("legacy","user",channel.profile.revision,update(config()),"Renamed",ids(&["two"])).unwrap();
    assert_eq!(repo.list("user").unwrap().len(),1);assert!(!repo.get("legacy","user").unwrap().ready);assert!(repo.runtime("legacy","user").is_err());
    assert_eq!(Repository::new(store).session(&session.id,"user").unwrap().agent_id,"legacy");
    drop(repo);
    let repo=ModelRepository::new(Arc::new(Store::open(&path).unwrap()));
    let channel=repo.save_channel("legacy","user",channel.profile.revision,update(config()),"Renamed",ids(&["one","two"])).unwrap();
    assert_eq!(channel.models[0].id,"legacy");assert_eq!(channel.models[1].id,second);
    assert_eq!(repo.list("user").unwrap()[0].channel_name.as_deref(),Some("Renamed"));
}

#[test]
fn channel_save_is_atomic_revision_checked_and_user_scoped() {
    let repo=ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    let channel=repo.save_channel("channel","a",0,update(config()),"a",ids(&["one","two"])).unwrap();
    for invalid in [ids(&[]),ids(&["one","one"]),ids(&["one",""]),vec!["x".repeat(257)]] {
        assert!(repo.save_channel("channel","a",channel.profile.revision,update(config()),"bad",invalid).is_err());
        assert_eq!(repo.channels("a").unwrap()[0].profile.revision,channel.profile.revision);
        assert_eq!(repo.list("a").unwrap().len(),2);
    }
    assert!(repo.save_channel("channel","a",0,update(config()),"bad",ids(&["one"])).is_err());
    assert!(repo.get(&channel.models[1].id,"other").is_err());assert!(repo.runtime(&channel.models[1].id,"other").is_err());
    let other=repo.save_channel("channel","b",0,update(config()),"b",ids(&["other"])).unwrap();
    repo.delete_channel("channel","b",other.profile.revision).unwrap();
    assert_eq!(repo.channels("a").unwrap().len(),1);
    assert!(repo.save_channel(&channel.models[1].id,"a",1,update(config()),"hijack",ids(&["x"])).is_err());
}

#[test]
fn per_model_reasoning_is_independent_and_invalidates_stale_channel_edits() {
    let repo=ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    let channel=repo.save_channel("channel","u",0,update(config()),"proxy",ids(&["one","two"])).unwrap();
    let second=repo.get(&channel.models[1].id,"u").unwrap();let mut config=second.config.clone();
    config.reasoning_effort=Some(ReasoningEffort::High);config.capabilities.reasoning_effort=true;
    repo.save(&second.id,"u",second.revision,update(config)).unwrap();
    assert!(repo.get("channel","u").unwrap().config.reasoning_effort.is_none());
    assert!(repo.save_channel("channel","u",channel.profile.revision,update(second.config.clone()),"proxy",ids(&["one"])).is_err());
    let current=repo.get(&second.id,"u").unwrap();let mut changed=current.config.clone();changed.base_url="https://unintended.example/v1/".into();
    assert!(repo.save(&second.id,"u",current.revision,update(changed)).is_err());
    assert!(repo.delete(&second.id,"u",current.revision).is_err());
    let latest=repo.channels("u").unwrap().remove(0);repo.delete_channel("channel","u",latest.profile.revision).unwrap();
    assert!(repo.list("u").unwrap().is_empty());
}

#[tokio::test]
async fn selected_model_and_shared_credentials_reach_http_and_same_session_keeps_history() {
    use axum::{extract::State,http::HeaderMap,routing::post,Json,Router,response::{Response,IntoResponse},body::Body};
    type Calls=Arc<Mutex<Vec<(String,serde_json::Value)>>>;
    async fn reply(State(calls):State<Calls>,headers:HeaderMap,Json(body):Json<serde_json::Value>)->Response {
        let stream=body["stream"]==true;
        calls.lock().unwrap().push((headers.get("authorization").map(|v|v.to_str().unwrap().to_owned()).unwrap_or_default(),body));
        if stream {
            Response::builder().header("content-type","text/event-stream").body(Body::from(format!("data: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"index":0,"delta":{"content":"kept"},"finish_reason":"stop"}]})))).unwrap()
        } else {Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"kept"},"finish_reason":"stop"}]})).into_response()}
    }
    let calls:Calls=Arc::default();let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config=config();config.base_url=format!("http://{}/v1/",listener.local_addr().unwrap());config.require_api_key=true;
    let app=Router::new().route("/v1/chat/completions",post(reply)).with_state(calls.clone());
    let task=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
    let store=Arc::new(Store::in_memory().unwrap());let repo=ModelRepository::new(store.clone());
    let channel=repo.save_channel("channel","u",0,ModelConfigUpdate{config:config.clone(),api_key:KeyChange::Set{value:SecretKey::new("shared-test-key").unwrap()}},"Proxy",ids(&["one","two"])).unwrap();
    let serialized=serde_json::to_string(&channel).unwrap();assert!(!serialized.contains("shared-test-key"));
    let request=ChatRequest{messages:vec![ChatMessage::User{content:"test".into()}],tools:vec![]};
    for model in &channel.models {ModelClient::new(repo.runtime(&model.id,"u").unwrap()).unwrap().complete(&request,&CancellationToken::new()).await.unwrap();}
    let next=repo.save_channel("channel","u",channel.profile.revision,ModelConfigUpdate{config:config.clone(),api_key:KeyChange::Set{value:SecretKey::new("rotated-test-key").unwrap()}},"Proxy",ids(&["one","two"])).unwrap();
    for model in &next.models {ModelClient::new(repo.runtime(&model.id,"u").unwrap()).unwrap().complete(&request,&CancellationToken::new()).await.unwrap();}
    {
        let calls=calls.lock().unwrap();assert_eq!(calls[0].0,"Bearer shared-test-key");assert_eq!(calls[1].0,"Bearer shared-test-key");
        assert_eq!(calls[2].0,"Bearer rotated-test-key");assert_eq!(calls[3].0,"Bearer rotated-test-key");assert_eq!(calls[0].1["model"],"one");assert_eq!(calls[1].1["model"],"two");
    }
    let manager=Manager::new(store.clone(),CancellationToken::new());let session=Repository::new(store.clone()).create_general_session_with_policy("channel","u",None).unwrap();
    let mut request=Request{agent_id:"channel".into(),user_id:"u".into(),session_id:session.id.clone(),request_id:"one".into(),terminal_session_id:String::new(),message:"remember 12233".into(),attachments:vec![]};
    manager.chat_desktop(request.clone(),None,false,&CancellationToken::new(),|_|async{Ok(())}).await.unwrap();
    let history=manager.history("channel".into(),"u".into(),session.id.clone(),0,20).await.unwrap();
    manager.change_model("u".into(),session.id.clone(),"channel".into(),next.models[1].id.clone(),history.session.revision).await.unwrap();
    request.agent_id=next.models[1].id.clone();request.request_id="two".into();request.message="which port?".into();
    manager.chat_desktop(request,None,false,&CancellationToken::new(),|_|async{Ok(())}).await.unwrap();
    let history=manager.history(next.models[1].id.clone(),"u".into(),session.id.clone(),0,20).await.unwrap();assert_eq!(history.turns.len(),2);
    assert!(calls.lock().unwrap().last().unwrap().1.to_string().contains("12233"));
    let current=repo.channels("u").unwrap().remove(0);
    let disabled=repo.save_channel("channel","u",current.profile.revision,ModelConfigUpdate{config,api_key:KeyChange::Clear},"Proxy",ids(&["one","two"])).unwrap();
    assert!(!disabled.profile.ready);for model in disabled.models {assert!(repo.runtime(&model.id,"u").is_err());}
    task.abort();
}
