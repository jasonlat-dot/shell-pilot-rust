use shellpilot_server::{
    agent::{Error, Payload, Repository, ServerIdentity, Target, TaskRequest},
    model::ConnectionRequest,
    store::Store,
};
use std::sync::Arc;
fn saved(store: &Store, id: &str, user: &str) -> String {
    store
        .save(
            ConnectionRequest {
                connection_id: Some(id.into()),
                connection_name: format!("server-{id}"),
                host: "127.0.0.1".into(),
                port: 22,
                username: "tester".into(),
                user_id: Some(user.into()),
                strict_host_key_check: Some(false),
                ..Default::default()
            },
            false,
        )
        .unwrap()
        .connection_id
}
fn identity(key: &str) -> ServerIdentity {
    ServerIdentity {
        host: "127.0.0.1".into(),
        port: 22,
        username: "tester".into(),
        host_key: key.into(),
    }
}
fn target(conn: &str, terminal: &str) -> Target {
    Target {
        connection_id: conn.into(),
        terminal_session_id: terminal.into(),
    }
}
#[test]
fn project_history_and_raw_events_survive_restart_and_reconnect_without_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.sqlite");
    let (p, a, b, events) = {
        let store = Arc::new(Store::open(&path).unwrap());
        saved(&store, "A", "default");
        saved(&store, "B", "default");
        let repo = Repository::new(store);
        let p = repo.create_project("default", "工作服务器", "A").unwrap();
        let a = repo
            .create_project_chat(
                &p.id,
                "default",
                "model-a",
                &target("A", "old-pty"),
                identity("key-a"),
            )
            .unwrap();
        repo.append(
            &a.id,
            "default",
            0,
            "original",
            Payload::UserMessage {
                text: "请一直用中文，A 的原始记录🙂".into(),
            },
        )
        .unwrap();
        let b = repo
            .create_project_chat(
                &p.id,
                "default",
                "model-b",
                &target("A", "old-pty"),
                identity("key-a"),
            )
            .unwrap();
        (
            p.id,
            a.id.clone(),
            b.id,
            repo.events(&a.id, "default", 0, 200).unwrap(),
        )
    };
    let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
    let projects = repo.projects("default").unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, p);
    let before = repo.project_chats("default", Some(&p), "").unwrap();
    assert_eq!(before.items.len(), 2);
    assert!(before.items.iter().any(|c| c.session_id == a));
    assert!(before.items.iter().any(|c| c.session_id == b));
    repo.bind_project(&p, "default", "A", &identity("key-a"))
        .unwrap();
    assert_eq!(
        repo.project_chats("default", Some(&p), "")
            .unwrap()
            .items
            .len(),
        2
    );
    assert_eq!(repo.events(&a, "default", 0, 200).unwrap(), events);
    assert_eq!(
        repo.session(&a, "default")
            .unwrap()
            .scope
            .unwrap()
            .origin_terminal_id
            .as_deref(),
        Some("old-pty")
    );
    assert!(repo
        .project_chats("default", None, "")
        .unwrap()
        .items
        .is_empty());
}
#[test]
fn wrong_owner_server_key_or_edited_profile_cannot_rebind_or_add_history() {
    let store = Arc::new(Store::in_memory().unwrap());
    saved(&store, "A", "default");
    saved(&store, "B", "other");
    let repo = Repository::new(store);
    assert!(matches!(
        repo.create_project("default", "B", "B"),
        Err(Error::NotFound)
    ));
    let p = repo.create_project("default", "A", "A").unwrap();
    assert!(repo
        .bind_project(&p.id, "other", "A", &identity("a"))
        .is_err());
    let mut changed = identity("a");
    changed.host = "elsewhere".into();
    assert!(matches!(
        repo.bind_project(&p.id, "default", "A", &changed),
        Err(Error::TargetMismatch)
    ));
    repo.bind_project(&p.id, "default", "A", &identity("a"))
        .unwrap();
    assert!(matches!(
        repo.bind_project(&p.id, "default", "B", &identity("a")),
        Err(Error::TargetMismatch)
    ));
    assert!(matches!(
        repo.create_project_chat(
            &p.id,
            "default",
            "model",
            &target("A", "pty"),
            identity("changed-key")
        ),
        Err(Error::TargetMismatch)
    ));
    assert!(repo
        .project_chats("default", Some(&p.id), "")
        .unwrap()
        .items
        .is_empty());
    assert!(repo.project_chats("other", Some(&p.id), "").is_err());
    assert!(repo.create_project("default", " ", "A").is_err());
}
#[test]
fn same_saved_server_can_have_independent_projects_models_and_paginated_chats() {
    let store = Arc::new(Store::in_memory().unwrap());
    saved(&store, "A", "default");
    let repo = Repository::new(store);
    let p = repo.create_project("default", "项目一", "A").unwrap();
    let q = repo.create_project("default", "项目二", "A").unwrap();
    let other = repo
        .create_project_chat(
            &q.id,
            "default",
            "model",
            &target("A", "pty"),
            identity("a"),
        )
        .unwrap();
    let general = repo.create_general_session("general", "default").unwrap();
    let mut ids = vec![];
    for _ in 0..53 {
        ids.push(
            repo.create_project_chat(
                &p.id,
                "default",
                "model",
                &target("A", "pty"),
                identity("a"),
            )
            .unwrap()
            .id,
        );
    }
    let first = repo.project_chats("default", Some(&p.id), "").unwrap();
    assert_eq!(first.items.len(), 50);
    let second = repo
        .project_chats("default", Some(&p.id), first.next_after.as_deref().unwrap())
        .unwrap();
    assert_eq!(second.items.len(), 3);
    assert!(second.next_after.is_none());
    let all: Vec<_> = first
        .items
        .iter()
        .chain(&second.items)
        .map(|i| i.session_id.clone())
        .collect();
    for id in ids {
        assert!(all.contains(&id));
    }
    assert!(!all.contains(&other.id));
    assert_eq!(
        repo.project_chats("default", None, "").unwrap().items[0].session_id,
        general.id
    );
    assert!(repo
        .project_chats("default", Some(&p.id), &other.id)
        .is_err());
}
#[test]
fn schema_nine_scopes_and_legacy_tasks_gain_project_membership_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite");
    let (a, root, legacy, raw) = {
        let store = Arc::new(Store::open(&path).unwrap());
        saved(&store, "A", "default");
        let repo = Repository::new(store);
        let root = repo.create_general_session("model", "default").unwrap();
        let a = repo
            .server_scope(
                "model",
                "default",
                &root.id,
                &target("A", "old"),
                identity("a"),
                true,
            )
            .unwrap();
        repo.append(
            &a.id,
            "default",
            0,
            "original",
            Payload::UserMessage {
                text: "不能删除的原文".into(),
            },
        )
        .unwrap();
        let legacy = repo.create_session("old-model", "default").unwrap();
        repo.start_task(
            &legacy.id,
            "default",
            0,
            "old",
            TaskRequest {
                goal: "原任务".into(),
                constraints: vec![],
                target: Some(target("A", "legacy")),
            },
        )
        .unwrap();
        (
            a.id.clone(),
            root.id,
            legacy.id,
            repo.events(&a.id, "default", 0, 200).unwrap(),
        )
    };
    let database = rusqlite::Connection::open(&path).unwrap();
    database
        .execute_batch(
            "DROP TABLE agent_project_chats; DROP TABLE agent_projects; PRAGMA user_version=9;",
        )
        .unwrap();
    drop(database);
    for _ in 0..2 {
        let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
        let ps = repo.projects("default").unwrap();
        assert_eq!(ps.len(), 2);
        let verified = ps.iter().find(|p| p.identity.is_some()).unwrap();
        assert_eq!(
            repo.project_chats("default", Some(&verified.id), "")
                .unwrap()
                .items[0]
                .session_id,
            a
        );
        let archived = ps.iter().find(|p| p.identity.is_none()).unwrap();
        assert_eq!(
            repo.project_chats("default", Some(&archived.id), "")
                .unwrap()
                .items[0]
                .session_id,
            legacy
        );
        assert_eq!(repo.events(&a, "default", 0, 200).unwrap(), raw);
        assert!(repo.session(&legacy, "default").unwrap().scope.is_none());
        assert_eq!(
            repo.session(&root, "default").unwrap().scope.unwrap().kind,
            shellpilot_server::agent::ScopeKind::General
        );
    }
}
