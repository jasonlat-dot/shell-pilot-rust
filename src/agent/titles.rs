//! Explicit user titles do not rewrite messages, task goals or model context.
use super::{chat, model, repository, Error, Repository, Result};
use rusqlite::{params, TransactionBehavior};
impl Repository {
    pub fn rename_chat(&self, agent: &str, user: &str, id: &str, title: &str) -> Result<()> {
        model::identifier(agent)?;
        model::identifier(user)?;
        model::identifier(id)?;
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 80 || title.chars().any(char::is_control) {
            return Err(Error::Invalid(
                "对话标题需为 1–80 个字符，不能包含换行或控制字符".into(),
            ));
        }
        self.db(|db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if repository::read_session(&tx, id, user)?.agent_id != agent {
                return Err(Error::NotFound);
            }
            tx.execute("INSERT INTO agent_chat_titles(session_id,title,updated_at) VALUES(?1,?2,?3)
                ON CONFLICT(session_id) DO UPDATE SET title=excluded.title,updated_at=excluded.updated_at",
                params![id,title,chrono::Utc::now().timestamp_millis()])?;
            tx.commit()?;
            Ok(())
        })
    }
}
impl chat::Manager {
    pub async fn rename_chat(
        &self,
        agent: String,
        user: String,
        id: String,
        title: String,
    ) -> chat::Result<()> {
        if self.stop.is_cancelled() {
            return Err(chat::Error::Stopped);
        }
        let repo = Repository::new(self.store.clone());
        tokio::task::spawn_blocking(move || {
            repo.rename_chat(&agent, &user, &id, &title)
                .map_err(|error| match error {
                    Error::Invalid(_) => chat::Error::Invalid,
                    other => other.into(),
                })
        })
        .await
        .map_err(|_| chat::Error::Storage)?
    }
}
#[cfg(test)]
mod tests {
    use super::super::Payload;
    use super::*;
    use crate::store::Store;
    use std::sync::Arc;
    #[test]
    fn custom_title_survives_upgrade_restart_and_never_rewrites_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let id;
        {
            let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
            let chat = repo.create_general_session("model", "user").unwrap();
            id = chat.id.clone();
            repo.append(
                &id,
                "user",
                0,
                "message",
                Payload::UserMessage {
                    text: "原始第一条消息".into(),
                },
            )
            .unwrap();
            repo.db(|db| {
                db.execute_batch("DROP TABLE agent_chat_titles; PRAGMA user_version=18")?;
                Ok(())
            })
            .unwrap();
        }
        {
            let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
            assert_eq!(
                repo.project_chats("user", None, "").unwrap().items[0].title,
                "原始第一条消息"
            );
            let before = repo.session(&id, "user").unwrap();
            let events = repo.events(&id, "user", 0, 200).unwrap();
            repo.rename_chat("model", "user", &id, "  Docker 排查记录  ")
                .unwrap();
            repo.rename_chat("model", "user", &id, "Docker 运维记录")
                .unwrap();
            assert_eq!(repo.session(&id, "user").unwrap(), before);
            assert_eq!(
                serde_json::to_value(repo.events(&id, "user", 0, 200).unwrap()).unwrap(),
                serde_json::to_value(events).unwrap()
            );
            repo.db(|db| {
                assert_eq!(
                    db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
                    20
                );
                Ok(())
            })
            .unwrap();
        }
        let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
        let listing = repo.project_chats("user", None, "").unwrap();
        assert_eq!(listing.items[0].session_id, id);
        assert_eq!(listing.items[0].title, "Docker 运维记录");
        repo.db(|db| {
            db.execute_batch("PRAGMA foreign_keys=ON")?;
            Ok(())
        })
        .unwrap();
        repo.delete_chat("model", "user", &id).unwrap();
        repo.db(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM agent_chat_titles", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn renaming_is_owned_and_validates_unicode_without_creating_orphan_titles() {
        let repo = Repository::new(Arc::new(Store::in_memory().unwrap()));
        let chat = repo.create_general_session("model", "user").unwrap();
        for title in [" ", "bad\nname", "bad\tname", &"汉".repeat(81)] {
            assert!(matches!(
                repo.rename_chat("model", "user", &chat.id, title),
                Err(Error::Invalid(_))
            ));
        }
        for (agent, user, id) in [
            ("wrong", "user", chat.id.as_str()),
            ("model", "wrong", chat.id.as_str()),
            ("model", "user", "missing"),
        ] {
            assert!(matches!(
                repo.rename_chat(agent, user, id, "新标题"),
                Err(Error::NotFound)
            ));
        }
        repo.rename_chat("model", "user", &chat.id, &"😀".repeat(80))
            .unwrap();
        repo.db(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM agent_chat_titles", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .unwrap();
    }
}
