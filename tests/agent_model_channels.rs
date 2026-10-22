use serde_json::json;
use shellpilot_server::{agent::{provider::*,Repository,chat::{Manager,Request}},store::Store};
use std::sync::{Arc,Mutex};
use tokio_util::sync::CancellationToken;

fn config()->ModelConfig {serde_json::from_value(json!({"provider":"openAiCompatible","baseUrl":"http://127.0.0.1/v1/","completionsPath":"chat/completions","model":"one","outputTokenParameter":"maxTokens","capabilities":{"developerRole":false,"temperature":false,"reasoningEffort":false},"requireApiKey":false})).unwrap()}
fn update(config:ModelConfig)->ModelConfigUpdate {ModelConfigUpdate{config,api_key:KeyChange::Keep}}
fn ids(values:&[&str])->Vec<String>{values.iter().map(|v|(*v).into()).collect()}

#[test]
fn copying_channels_preserves_per_model_configs_order_state_and_independent_identities() {
    let repo=ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    let mut configured=config();configured.capabilities.reasoning_effort=true;configured.reasoning_effort=Some(ReasoningEffort::High);
    let original=repo.save_channel("original","u",0,update(configured),"My channel",ids(&["one","two"])).unwrap();
    let second=repo.get(&original.models[1].id,"u").unwrap();let mut settings=second.config;settings.reasoning_effort=Some(ReasoningEffort::Low);
    repo.save(&second.id,"u",second.revision,update(settings)).unwrap();
    let original=repo.channels("u").unwrap().remove(0);
    let original=repo.set_channel_enabled("original","u",original.profile.revision,false).unwrap();
    let before=serde_json::to_value(&original).unwrap();
    let copied=repo.copy_channel("original","u",original.profile.revision,"copy").unwrap();
    assert_eq!(copied.profile.display_name,"My channel（副本）");assert!(!copied.profile.enabled);assert_eq!(copied.profile.revision,1);
    assert_eq!(copied.models.iter().map(|m|m.model.clone()).collect::<Vec<_>>(),ids(&["one","two"]));
    for (source,target) in original.models.iter().zip(&copied.models) {
        assert_ne!(source.id,target.id);
        assert_eq!(serde_json::to_value(repo.get(&source.id,"u").unwrap().config).unwrap(),serde_json::to_value(repo.get(&target.id,"u").unwrap().config).unwrap());
    }
    assert_eq!(serde_json::to_value(repo.channels("u").unwrap().remove(0)).unwrap(),before);
    let next=repo.copy_channel("original","u",original.profile.revision,"copy-two").unwrap();assert_eq!(next.profile.display_name,"My channel（副本 2）");
    let enabled=repo.set_channel_enabled("copy","u",copied.profile.revision,true).unwrap();
    repo.delete_channel("original","u",original.profile.revision).unwrap();
    assert!(repo.runtime(&enabled.models[1].id,"u").is_ok());
}

#[test]
fn copying_legacy_channels_checks_ownership_revision_destination_capacity_and_name_length() {
    let repo=ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    let legacy=repo.save_named("legacy","u",0,update(config()),Some(&"模".repeat(64))).unwrap();
    assert!(repo.copy_channel("legacy","other",legacy.revision,"copy").is_err());
    assert!(matches!(repo.copy_channel("legacy","u",legacy.revision+1,"copy"),Err(ModelError::Stale)));
    assert!(repo.copy_channel("legacy","u",legacy.revision,"legacy").is_err());
    let copied=repo.copy_channel("legacy","u",legacy.revision,"copy").unwrap();assert_eq!(copied.profile.display_name.chars().count(),64);
    assert_eq!(copied.models.len(),1);assert!(copied.profile.enabled);
    assert!(repo.copy_channel("legacy","u",legacy.revision,"copy").is_err());
    assert_eq!(repo.channels("u").unwrap().len(),2);
    let channel=repo.save_channel("multi","u",0,update(config()),"Multi",ids(&["one","two"])).unwrap();
    assert!(repo.copy_channel(&channel.models[1].id,"u",1,"member-copy").is_err());
    for index in 3..32 {repo.save_named(&format!("profile-{index}"),"u",0,update(config()),None).unwrap();}
    assert!(repo.copy_channel("legacy","u",legacy.revision,"over-capacity").is_err());assert_eq!(repo.channels("u").unwrap().len(),32);
}

#[test]
fn credential_failure_rolls_back_the_entire_channel_copy() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("copy.sqlite");let repo=ModelRepository::new(Arc::new(Store::open(&path).unwrap()));
    let original=repo.save_channel("original","u",0,update(config()),"Original",ids(&["one","two"])).unwrap();
    let db=rusqlite::Connection::open(&path).unwrap();db.execute("UPDATE agent_model_profiles SET api_key=X'000102' WHERE user_id='u' AND id=?1",[&original.models[1].id]).unwrap();
    assert!(matches!(repo.copy_channel("original","u",original.profile.revision,"copy"),Err(ModelError::Credentials)));
    assert_eq!(repo.channels("u").unwrap().len(),1);assert!(repo.get("copy","u").is_err());
    assert_eq!(db.query_row("SELECT COUNT(*) FROM agent_model_profiles WHERE user_id='u'",[],|r|r.get::<_,u64>(0)).unwrap(),2);
}

#[test]
fn copy_draft_is_committed_with_edits_and_invalid_drafts_leave_no_copy() {
    let repo=ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    let source=repo.save_channel("source","u",0,update(config()),"Source",ids(&["one","two"])).unwrap();
    let source=repo.set_channel_enabled("source","u",source.profile.revision,false).unwrap();
    for (id,name,models) in [("bad-name"," ",ids(&["one"])),("bad-models","Copy",ids(&["same","same"])),("empty-models","Copy",vec![])] {
        let draft=ChannelCopyDraft{display_name:name.into(),changes:Some(ChannelCopyChanges{update:update(config()),model_ids:models})};
        assert!(repo.copy_channel_draft("source","u",source.profile.revision,id,Some(draft)).is_err());
        assert!(repo.get(id,"u").is_err());assert_eq!(repo.channels("u").unwrap().len(),1);
    }
    let mut edited=config();edited.timeout_seconds=240;
    let draft=ChannelCopyDraft{display_name:"Edited copy".into(),changes:Some(ChannelCopyChanges{update:update(edited),model_ids:ids(&["three","four"])})};
    let copied=repo.copy_channel_draft("source","u",source.profile.revision,"copy",Some(draft)).unwrap();
    assert_eq!(copied.profile.display_name,"Edited copy");assert!(!copied.profile.enabled);
    assert_eq!(copied.models.iter().map(|m|m.model.clone()).collect::<Vec<_>>(),ids(&["three","four"]));
    for model in &copied.models {assert_eq!(repo.get(&model.id,"u").unwrap().config.timeout_seconds,240);}
    assert_eq!(repo.get("source","u").unwrap().config.timeout_seconds,source.profile.config.timeout_seconds);
    assert_eq!(repo.channels("u").unwrap().len(),2);
    let renamed=repo.copy_channel_draft("source","u",source.profile.revision,"renamed",Some(ChannelCopyDraft{display_name:"Custom name".into(),changes:None})).unwrap();
    assert_eq!(renamed.profile.display_name,"Custom name");assert_eq!(serde_json::to_value(&renamed.profile.config).unwrap(),serde_json::to_value(&source.profile.config).unwrap());
}

#[tokio::test]
async fn copied_credentials_work_for_every_model_and_stay_independent_after_source_changes() {
    use axum::{extract::State,http::HeaderMap,routing::post,Json,Router};
    type Calls=Arc<Mutex<Vec<(String,String)>>>;
    async fn reply(State(calls):State<Calls>,headers:HeaderMap,Json(body):Json<serde_json::Value>)->Json<serde_json::Value>{
        calls.lock().unwrap().push((headers["authorization"].to_str().unwrap().to_owned(),body["model"].as_str().unwrap().to_owned()));
        Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}))
    }
    let calls:Calls=Arc::default();let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config=config();config.base_url=format!("http://{}/v1/",listener.local_addr().unwrap());config.require_api_key=true;
    let app=Router::new().route("/v1/chat/completions",post(reply)).with_state(calls.clone());
    let task=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
    let repo=ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    let original=repo.save_channel("source","u",0,ModelConfigUpdate{config:config.clone(),api_key:KeyChange::Set{value:SecretKey::new("copy-test-secret").unwrap()}},"Source",ids(&["one","two"])).unwrap();
    let original=repo.save_channel("source","u",original.profile.revision,update(config.clone()),"Source",ids(&["two","three"])).unwrap();
    assert_ne!(original.models[0].id,"source","copy from the active member, even if the original anchor was retired");
    let copied=repo.copy_channel("source","u",original.profile.revision,"copy").unwrap();
    assert!(copied.profile.api_key_configured);assert!(!serde_json::to_string(&copied).unwrap().contains("copy-test-secret"));
    let rotated=repo.save_channel("source","u",original.profile.revision,ModelConfigUpdate{config:config.clone(),api_key:KeyChange::Set{value:SecretKey::new("rotated-test-secret").unwrap()}},"Source",ids(&["two","three"])).unwrap();
    let request=ChatRequest{messages:vec![ChatMessage::User{content:"test".into()}],tools:vec![]};
    for model in &copied.models {ModelClient::new(repo.runtime(&model.id,"u").unwrap()).unwrap().complete(&request,&CancellationToken::new()).await.unwrap();}
    ModelClient::new(repo.runtime(&rotated.models[0].id,"u").unwrap()).unwrap().complete(&request,&CancellationToken::new()).await.unwrap();
    repo.delete_channel("source","u",rotated.profile.revision).unwrap();
    ModelClient::new(repo.runtime(&copied.models[1].id,"u").unwrap()).unwrap().complete(&request,&CancellationToken::new()).await.unwrap();
    assert_eq!(*calls.lock().unwrap(),vec![("Bearer copy-test-secret".into(),"two".into()),("Bearer copy-test-secret".into(),"three".into()),("Bearer rotated-test-secret".into(),"two".into()),("Bearer copy-test-secret".into(),"three".into())]);
    config.require_api_key=false;
    let optional=repo.save_named("optional","u",0,ModelConfigUpdate{config:config.clone(),api_key:KeyChange::Set{value:SecretKey::new("optional-test-secret").unwrap()}},Some("Optional")).unwrap();
    let copied=repo.copy_channel("optional","u",optional.revision,"optional-copy").unwrap();assert!(copied.profile.api_key_configured);assert!(!copied.profile.config.require_api_key);
    config.require_api_key=true;
    let enabled=repo.save_channel("optional-copy","u",copied.profile.revision,update(config),"Optional",ids(&["one"])).unwrap();
    ModelClient::new(repo.runtime(&enabled.models[0].id,"u").unwrap()).unwrap().complete(&request,&CancellationToken::new()).await.unwrap();
    assert_eq!(calls.lock().unwrap().last().unwrap().0,"Bearer optional-test-secret");
    for (id,key,expected) in [("draft-kept",KeyChange::Keep,"Bearer copy-test-secret"),("draft-new",KeyChange::Set{value:SecretKey::new("draft-new-test-secret").unwrap()},"Bearer draft-new-test-secret")] {
        let draft=ChannelCopyDraft{display_name:"Saved draft".into(),changes:Some(ChannelCopyChanges{update:ModelConfigUpdate{config:repo.get("copy","u").unwrap().config,api_key:key},model_ids:ids(&["four"])})};
        // Use the original copied channel: it remains independent after source deletion.
        let created=repo.copy_channel_draft("copy","u",1,id,Some(draft)).unwrap();
        ModelClient::new(repo.runtime(&created.models[0].id,"u").unwrap()).unwrap().complete(&request,&CancellationToken::new()).await.unwrap();
        assert_eq!(calls.lock().unwrap().last().unwrap(),&(expected.into(),"four".into()));
    }
    task.abort();
}

#[tokio::test]
async fn channel_enabled_state_hides_every_member_and_blocks_new_sessions_without_losing_history() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("enabled.sqlite");
    let store=Arc::new(Store::open(&path).unwrap());let repo=ModelRepository::new(store.clone());
    let channel=repo.save_channel("channel","u",0,update(config()),"Enabled channel",ids(&["one","two"])).unwrap();
    repo.save_channel("other","u",0,update(config()),"Other",ids(&["one"])).unwrap();
    let manager=Manager::new(store.clone(),CancellationToken::new());
    let session=manager.create_general_session(channel.models[1].id.clone(),"u".into()).await.unwrap();
    assert_eq!(manager.list("u".into()).await.unwrap().len(),3);
    let disabled=repo.set_channel_enabled("channel","u",channel.profile.revision,false).unwrap();
    assert!(!disabled.profile.enabled);assert!(!disabled.profile.ready);assert_eq!(disabled.models.len(),2);
    for member in &channel.models {
        assert!(!repo.get(&member.id,"u").unwrap().enabled);assert!(matches!(repo.runtime(&member.id,"u"),Err(ModelError::Disabled)));
        let error=manager.create_general_session(member.id.clone(),"u".into()).await.err().unwrap();assert_eq!(error.code(),"MODEL_DISABLED");assert_eq!(error.status(),409);
    }
    assert_eq!(manager.list("u".into()).await.unwrap()[0].agent_id,"other");
    assert_eq!(repo.channel_report("u").unwrap().items.len(),2);assert_eq!(Repository::new(store.clone()).session(&session.id,"u").unwrap().agent_id,channel.models[1].id);
    drop(manager);drop(repo);drop(store);
    let store=Arc::new(Store::open(&path).unwrap());let repo=ModelRepository::new(store.clone());assert!(!repo.channels("u").unwrap()[0].profile.enabled);
    let enabled=repo.set_channel_enabled("channel","u",disabled.profile.revision,true).unwrap();assert!(enabled.profile.enabled);assert!(enabled.profile.ready);assert_eq!(enabled.models[1].id,channel.models[1].id);
    for member in enabled.models {assert!(repo.runtime(&member.id,"u").is_ok());}
    assert!(Repository::new(store).session(&session.id,"u").is_ok());
}

#[test]
fn channel_toggle_is_scoped_revision_checked_and_does_not_rewrite_secrets_or_models() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("toggle.sqlite");let store=Arc::new(Store::open(&path).unwrap());let repo=ModelRepository::new(store);
    let mut configured=config();configured.require_api_key=true;
    let channel=repo.save_channel("channel","u",0,ModelConfigUpdate{config:configured.clone(),api_key:KeyChange::Set{value:SecretKey::new("toggle-secret").unwrap()}},"Channel",ids(&["one","two"])).unwrap();
    let db=rusqlite::Connection::open(&path).unwrap();
    let payloads=||{let mut query=db.prepare("SELECT id,config_json,api_key,revision FROM agent_model_profiles ORDER BY id").unwrap();query.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Vec<u8>>(2)?,row.get::<_,u64>(3)?))).unwrap().collect::<std::result::Result<Vec<_>,_>>().unwrap()};
    let before=payloads();
    assert!(repo.set_channel_enabled("channel","other",channel.profile.revision,false).is_err());assert!(repo.set_channel_enabled(&channel.models[1].id,"u",channel.profile.revision,false).is_err());
    let disabled=repo.set_channel_enabled("channel","u",channel.profile.revision,false).unwrap();assert_eq!(payloads(),before);assert!(disabled.profile.api_key_configured);
    assert!(matches!(repo.set_channel_enabled("channel","u",channel.profile.revision,true),Err(ModelError::Stale)));
    assert!(matches!(repo.save_channel("channel","u",channel.profile.revision,update(configured.clone()),"stale",ids(&["one"])),Err(ModelError::Stale)));
    let edited=repo.save_channel("channel","u",disabled.profile.revision,update(configured),"Edited",ids(&["two","one","three"])).unwrap();assert!(!edited.profile.enabled);assert!(edited.profile.api_key_configured);assert_eq!(edited.models[0].id,channel.models[1].id);
    let enabled=repo.set_channel_enabled("channel","u",edited.profile.revision,true).unwrap();assert!(enabled.profile.ready);
    repo.delete_channel("channel","u",enabled.profile.revision).unwrap();let recreated=repo.save_channel("channel","u",0,update(config()),"Recreated",ids(&["one"])).unwrap();assert!(recreated.profile.enabled);
}

#[test]
fn legacy_profiles_default_enabled_and_keep_disabled_state_when_converted_to_channels() {
    let store=Arc::new(Store::in_memory().unwrap());let repo=ModelRepository::new(store);
    let legacy=repo.save_named("legacy","u",0,update(config()),Some("Legacy")).unwrap();assert!(legacy.enabled);
    let disabled=repo.set_channel_enabled("legacy","u",legacy.revision,false).unwrap();assert!(!disabled.profile.enabled);
    let converted=repo.save_channel("legacy","u",disabled.profile.revision,update(config()),"Converted",ids(&["one","two"])).unwrap();assert!(!converted.profile.enabled);assert!(!repo.get(&converted.models[1].id,"u").unwrap().ready);
    let enabled=repo.set_channel_enabled("legacy","u",converted.profile.revision,true).unwrap();assert!(enabled.profile.ready);
    assert_eq!(repo.set_channel_enabled("legacy","u",enabled.profile.revision,true).unwrap().profile.revision,enabled.profile.revision);
    repo.save_named("single","u",0,update(config()),None).unwrap();let disabled=repo.set_channel_enabled("single","u",1,false).unwrap();repo.delete("single","u",disabled.profile.revision).unwrap();assert!(repo.save_named("single","u",0,update(config()),None).unwrap().enabled);
}

#[test]
fn upgrade_creates_channel_state_table_without_disabling_existing_profiles() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("upgrade.sqlite");
    let repo=ModelRepository::new(Arc::new(Store::open(&path).unwrap()));repo.save_channel("existing","u",0,update(config()),"Existing",ids(&["one","two"])).unwrap();drop(repo);
    let db=rusqlite::Connection::open(&path).unwrap();db.execute_batch("DROP TABLE agent_model_channel_states; PRAGMA user_version=24;").unwrap();drop(db);
    let repo=ModelRepository::new(Arc::new(Store::open(&path).unwrap()));let channel=repo.channels("u").unwrap().remove(0);assert!(channel.profile.enabled);assert_eq!(channel.models.len(),2);for member in channel.models {assert!(repo.runtime(&member.id,"u").is_ok());}
    let db=rusqlite::Connection::open(path).unwrap();assert_eq!(db.query_row("PRAGMA user_version",[],|r|r.get::<_,u64>(0)).unwrap(),26);
}

#[test]
fn unreadable_configurations_are_isolated_preserved_and_reported_without_private_data() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("isolated.sqlite");
    let store=Arc::new(Store::open(&path).unwrap());let repo=ModelRepository::new(store);
    repo.save_named("good","u",0,update(config()),Some("Healthy")).unwrap();
    repo.save_named("bad","u",0,update(config()),Some("Unreadable")).unwrap();
    let db=rusqlite::Connection::open(&path).unwrap();
    let original:String=db.query_row("SELECT config_json FROM agent_model_profiles WHERE id='bad'",[],|r|r.get(0)).unwrap();
    let base:serde_json::Value=serde_json::from_str(&original).unwrap();
    let mut unknown=base.clone();unknown["supplier"]=json!("future-private-supplier");
    let mut extra=base.clone();extra["private-future-field"]=json!("private-test-secret");
    let mut invalid=base.clone();invalid["baseUrl"]=json!("private-test-secret");
    let mut effort=base.clone();effort["reasoningEffort"]=json!("future-effort");
    for damaged in ["{invalid private-test-secret".to_owned(),unknown.to_string(),extra.to_string(),invalid.to_string(),effort.to_string()] {
        db.execute("UPDATE agent_model_profiles SET config_json=?1 WHERE id='bad'",[&damaged]).unwrap();
        let report=repo.channel_report("u").unwrap();
        assert_eq!(report.items.len(),1);assert_eq!(report.items[0].profile.id,"good");
        assert_eq!(report.issues.len(),1);assert_eq!(report.issues[0].id,"bad");
        assert_eq!(report.issues[0].display_name,"Unreadable");
        let json=serde_json::to_string(&report).unwrap();
        assert!(!json.contains("private-test-secret"));assert!(!json.contains("future-private-supplier"));
        assert_eq!(repo.list("u").unwrap().len(),1);
        assert_eq!(repo.channels("u").unwrap().len(),1);
        assert!(matches!(repo.get("bad","u"),Err(ModelError::StoredConfig)));
        assert!(matches!(repo.runtime("bad","u"),Err(ModelError::StoredConfig)));
        assert!(repo.channel_report("other").unwrap().issues.is_empty());
        let after:(String,u64)=db.query_row("SELECT config_json,revision FROM agent_model_profiles WHERE id='bad'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(after,(damaged,1));
    }
    db.execute("UPDATE agent_model_profiles SET config_json=?1 WHERE id='bad'",[original]).unwrap();
    let report=repo.channel_report("u").unwrap();assert!(report.issues.is_empty());assert_eq!(report.items.len(),2);
    db.execute("DROP TABLE agent_model_profiles",[]).unwrap();
    assert!(matches!(repo.list("u"),Err(ModelError::Database)));
    assert!(matches!(repo.channel_report("u"),Err(ModelError::Database)));
}

#[test]
fn unreadable_nonfirst_member_does_not_offer_a_partial_channel_for_editing() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("member.sqlite");
    let store=Arc::new(Store::open(&path).unwrap());let repo=ModelRepository::new(store);
    let channel=repo.save_channel("channel","u",0,update(config()),"Mixed channel",ids(&["one","two"])).unwrap();
    repo.save_named("good","u",0,update(config()),Some("Healthy")).unwrap();
    let db=rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE agent_model_profiles SET config_json='{}' WHERE id=?1",[&channel.models[1].id]).unwrap();
    let report=repo.channel_report("u").unwrap();
    assert_eq!(report.items.len(),1);assert_eq!(report.items[0].profile.id,"good");
    assert_eq!(report.issues.len(),1);assert_eq!(report.issues[0].id,"channel");
    assert_eq!(report.issues[0].display_name,"Mixed channel");
    assert_eq!(repo.list("u").unwrap().len(),2);
    let members:usize=db.query_row("SELECT COUNT(*) FROM agent_model_channel_members WHERE active=1",[],|r|r.get(0)).unwrap();
    assert_eq!(members,2);
}

#[test]
fn channels_keep_creation_order_across_edits_legacy_conversion_and_reopening() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("order.sqlite");
    let store=Arc::new(Store::open(&path).unwrap());let repo=ModelRepository::new(store);
    let first=repo.save_named("z-first","u",0,update(config()),Some("Z first")).unwrap();
    let second=repo.save_channel("a-second","u",0,update(config()),"A second",ids(&["one","two"])).unwrap();
    repo.save_channel("other","other",0,update(config()),"Other user",ids(&["one"])).unwrap();
    let third=repo.save_named("m-third","u",0,update(config()),Some("M third")).unwrap();
    let order=|repo:&ModelRepository|repo.channels("u").unwrap().into_iter().map(|channel|channel.profile.id).collect::<Vec<_>>();
    assert_eq!(order(&repo),ids(&["z-first","a-second","m-third"]));
    repo.save_channel("z-first","u",first.revision,update(config()),"ZZ renamed",ids(&["one","two"])).unwrap();
    // Removing the initial model must not make the channel as new as its remaining member.
    repo.save_channel("a-second","u",second.profile.revision,update(config()),"0 renamed",ids(&["two"])).unwrap();
    repo.save_named("m-third","u",third.revision,update(config()),Some("A renamed")).unwrap();
    repo.save_channel("0-last","u",0,update(config()),"0 last",ids(&["one"])).unwrap();
    assert_eq!(order(&repo),ids(&["z-first","a-second","m-third","0-last"]));
    drop(repo);
    let repo=ModelRepository::new(Arc::new(Store::open(&path).unwrap()));
    assert_eq!(order(&repo),ids(&["z-first","a-second","m-third","0-last"]));
}

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


#[test]
fn google_channel_uses_canonical_model_identity_and_preserves_members_and_shared_keys(){
    let directory=tempfile::tempdir().unwrap();let path=directory.path().join("google-channels.sqlite");
    let repo=ModelRepository::new(Arc::new(Store::open(&path).unwrap()));let mut config=config();
    config.provider=ProviderKind::GoogleGemini;config.completions_path="models".into();config.require_api_key=true;
    let channel=repo.save_channel("google","u",0,ModelConfigUpdate{config:config.clone(),api_key:KeyChange::Set{value:SecretKey::new("google-test-shared").unwrap()}},"Google",ids(&["models/one","two"])).unwrap();
    assert_eq!(channel.models[0].id,"google");assert_eq!(channel.models[0].model,"one");let second=channel.models[1].id.clone();
    for member in &channel.models {let runtime=repo.runtime(&member.id,"u").unwrap();assert_eq!(runtime.config.model,member.model);assert!(repo.get(&member.id,"u").unwrap().api_key_configured);}
    assert!(repo.save_channel("google","u",channel.profile.revision,update(config.clone()),"bad",ids(&["one","models/one"])).is_err());
    assert_eq!(repo.channels("u").unwrap()[0].profile.revision,channel.profile.revision);
    let edited=repo.save_channel("google","u",channel.profile.revision,update(config),"Google",ids(&["models/two","one"])).unwrap();
    assert_eq!(edited.models[0].id,second);assert_eq!(edited.models[1].id,"google");drop(repo);
    let repo=ModelRepository::new(Arc::new(Store::open(&path).unwrap()));assert_eq!(repo.channels("u").unwrap()[0].models[0].id,second);
    assert_eq!(repo.runtime(&second,"u").unwrap().config.provider,ProviderKind::GoogleGemini);
}
