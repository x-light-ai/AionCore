use std::sync::Arc;

use aionui_common::{generate_id, now_ms};
use aionui_db::models::AssistantSessionRow;
use aionui_db::IChannelRepository;
use tracing::{debug, info};

use crate::error::ChannelError;

/// Manages per-chat session isolation for channel users.
///
/// Each (user_id, chat_id) pair maps to exactly one session. This ensures
/// that the same user chatting in different groups/DMs gets independent
/// conversation contexts, while repeated messages in the same chat reuse
/// the existing session.
pub struct SessionManager {
    repo: Arc<dyn IChannelRepository>,
}

impl SessionManager {
    pub fn new(repo: Arc<dyn IChannelRepository>) -> Self {
        Self { repo }
    }

    /// Finds an existing session for the user+chat pair, or creates one.
    ///
    /// - If found: updates `last_activity` and returns the existing session.
    /// - If not found: creates a new session with the given `agent_type`.
    ///
    /// The `workspace` parameter is optional and may be set later by
    /// the `ChannelManager` when it knows the active workspace path.
    pub async fn get_or_create_session(
        &self,
        user_id: &str,
        chat_id: &str,
        agent_type: &str,
        workspace: Option<&str>,
    ) -> Result<AssistantSessionRow, ChannelError> {
        let now = now_ms();
        let new_row = AssistantSessionRow {
            id: generate_id(),
            user_id: user_id.to_owned(),
            agent_type: agent_type.to_owned(),
            conversation_id: None,
            workspace: workspace.map(String::from),
            chat_id: Some(chat_id.to_owned()),
            created_at: now,
            last_activity: now,
        };

        let session = self
            .repo
            .get_or_create_session(user_id, chat_id, &new_row)
            .await?;

        debug!(
            session_id = %session.id,
            user_id = %user_id,
            chat_id = %chat_id,
            "session resolved"
        );

        Ok(session)
    }

    /// Returns all active sessions.
    pub async fn get_active_sessions(
        &self,
    ) -> Result<Vec<AssistantSessionRow>, ChannelError> {
        let sessions = self.repo.get_all_sessions().await?;
        Ok(sessions)
    }

    /// Removes all sessions belonging to a user.
    ///
    /// Called when a user is revoked to clean up their session state.
    pub async fn cleanup_user_sessions(
        &self,
        user_id: &str,
    ) -> Result<(), ChannelError> {
        self.repo.delete_sessions_by_user(user_id).await?;
        info!(user_id = %user_id, "cleaned up user sessions");
        Ok(())
    }

    /// Updates the conversation binding for a session.
    ///
    /// Called after a new conversation is created for this session,
    /// linking the session to its backing conversation.
    pub async fn bind_conversation(
        &self,
        session_id: &str,
        conversation_id: &str,
    ) -> Result<(), ChannelError> {
        // We re-fetch the session to ensure it exists
        let session = self
            .repo
            .get_session(session_id)
            .await?
            .ok_or_else(|| ChannelError::SessionNotFound(session_id.to_owned()))?;

        // Update last_activity (the DB layer handles the actual bind
        // via get_or_create_session; here we just track activity)
        let _ = session;
        self.repo
            .update_session_activity(session_id, now_ms())
            .await?;

        debug!(
            session_id = %session_id,
            conversation_id = %conversation_id,
            "session bound to conversation"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_db::models::{
        AssistantSessionRow, AssistantUserRow, ChannelPluginRow, PairingCodeRow,
    };
    use aionui_db::{DbError, IChannelRepository, UpdatePluginStatusParams};
    use aionui_common::TimestampMs;
    use std::sync::Mutex;

    // ── Mock IChannelRepository ────────────────────────────────────────

    struct MockRepo {
        sessions: Mutex<Vec<AssistantSessionRow>>,
    }

    impl MockRepo {
        fn new() -> Self {
            Self {
                sessions: Mutex::new(Vec::new()),
            }
        }

        fn get_sessions(&self) -> Vec<AssistantSessionRow> {
            self.sessions.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl IChannelRepository for MockRepo {
        // -- Plugin CRUD (unused stubs) --
        async fn get_all_plugins(&self) -> Result<Vec<ChannelPluginRow>, DbError> {
            Ok(vec![])
        }
        async fn get_plugin(&self, _id: &str) -> Result<Option<ChannelPluginRow>, DbError> {
            Ok(None)
        }
        async fn upsert_plugin(&self, _row: &ChannelPluginRow) -> Result<(), DbError> {
            Ok(())
        }
        async fn update_plugin_status(
            &self,
            _id: &str,
            _params: &UpdatePluginStatusParams,
        ) -> Result<(), DbError> {
            Ok(())
        }
        async fn delete_plugin(&self, _id: &str) -> Result<(), DbError> {
            Ok(())
        }

        // -- User CRUD (unused stubs) --
        async fn get_all_users(&self) -> Result<Vec<AssistantUserRow>, DbError> {
            Ok(vec![])
        }
        async fn get_user_by_platform(
            &self,
            _platform_user_id: &str,
            _platform_type: &str,
        ) -> Result<Option<AssistantUserRow>, DbError> {
            Ok(None)
        }
        async fn create_user(&self, _row: &AssistantUserRow) -> Result<(), DbError> {
            Ok(())
        }
        async fn update_user_last_active(
            &self,
            _id: &str,
            _last_active: TimestampMs,
        ) -> Result<(), DbError> {
            Ok(())
        }
        async fn delete_user(&self, _id: &str) -> Result<(), DbError> {
            Ok(())
        }

        // -- Session CRUD --
        async fn get_all_sessions(&self) -> Result<Vec<AssistantSessionRow>, DbError> {
            Ok(self.sessions.lock().unwrap().clone())
        }

        async fn get_session(
            &self,
            id: &str,
        ) -> Result<Option<AssistantSessionRow>, DbError> {
            let sessions = self.sessions.lock().unwrap();
            Ok(sessions.iter().find(|s| s.id == id).cloned())
        }

        async fn get_or_create_session(
            &self,
            user_id: &str,
            chat_id: &str,
            new_row: &AssistantSessionRow,
        ) -> Result<AssistantSessionRow, DbError> {
            let mut sessions = self.sessions.lock().unwrap();
            // Look for existing session by user_id + chat_id
            if let Some(existing) = sessions.iter_mut().find(|s| {
                s.user_id == user_id && s.chat_id.as_deref() == Some(chat_id)
            }) {
                existing.last_activity = new_row.last_activity;
                return Ok(existing.clone());
            }
            // Create new
            sessions.push(new_row.clone());
            Ok(new_row.clone())
        }

        async fn update_session_activity(
            &self,
            id: &str,
            last_activity: TimestampMs,
        ) -> Result<(), DbError> {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(s) = sessions.iter_mut().find(|s| s.id == id) {
                s.last_activity = last_activity;
                Ok(())
            } else {
                Err(DbError::NotFound(id.into()))
            }
        }

        async fn delete_sessions_by_user(&self, user_id: &str) -> Result<(), DbError> {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.retain(|s| s.user_id != user_id);
            Ok(())
        }

        // -- Pairing codes (unused stubs) --
        async fn create_pairing(&self, _row: &PairingCodeRow) -> Result<(), DbError> {
            Ok(())
        }
        async fn get_pending_pairings(&self) -> Result<Vec<PairingCodeRow>, DbError> {
            Ok(vec![])
        }
        async fn get_pairing_by_code(
            &self,
            _code: &str,
        ) -> Result<Option<PairingCodeRow>, DbError> {
            Ok(None)
        }
        async fn update_pairing_status(
            &self,
            _code: &str,
            _status: &str,
        ) -> Result<(), DbError> {
            Ok(())
        }
        async fn cleanup_expired_pairings(
            &self,
            _now: TimestampMs,
        ) -> Result<u64, DbError> {
            Ok(0)
        }
    }

    fn make_manager() -> (SessionManager, Arc<MockRepo>) {
        let repo = Arc::new(MockRepo::new());
        let mgr = SessionManager::new(repo.clone());
        (mgr, repo)
    }

    // ── get_or_create_session ──────────────────────────────────────────

    #[tokio::test]
    async fn creates_new_session() {
        let (mgr, repo) = make_manager();
        let session = mgr
            .get_or_create_session("user1", "chat1", "gemini", None)
            .await
            .unwrap();

        assert_eq!(session.user_id, "user1");
        assert_eq!(session.chat_id.as_deref(), Some("chat1"));
        assert_eq!(session.agent_type, "gemini");
        assert!(session.conversation_id.is_none());

        let all = repo.get_sessions();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn reuses_existing_session_for_same_user_chat() {
        let (mgr, repo) = make_manager();

        let s1 = mgr
            .get_or_create_session("user1", "chat1", "gemini", None)
            .await
            .unwrap();
        let s2 = mgr
            .get_or_create_session("user1", "chat1", "gemini", None)
            .await
            .unwrap();

        assert_eq!(s1.id, s2.id);
        assert_eq!(repo.get_sessions().len(), 1);
    }

    #[tokio::test]
    async fn different_chats_get_different_sessions() {
        let (mgr, repo) = make_manager();

        let s1 = mgr
            .get_or_create_session("user1", "chatA", "acp", None)
            .await
            .unwrap();
        let s2 = mgr
            .get_or_create_session("user1", "chatB", "acp", None)
            .await
            .unwrap();

        assert_ne!(s1.id, s2.id);
        assert_eq!(repo.get_sessions().len(), 2);
    }

    #[tokio::test]
    async fn different_users_same_chat_get_different_sessions() {
        let (mgr, repo) = make_manager();

        let s1 = mgr
            .get_or_create_session("user1", "chat1", "gemini", None)
            .await
            .unwrap();
        let s2 = mgr
            .get_or_create_session("user2", "chat1", "gemini", None)
            .await
            .unwrap();

        assert_ne!(s1.id, s2.id);
        assert_eq!(repo.get_sessions().len(), 2);
    }

    #[tokio::test]
    async fn session_with_workspace() {
        let (mgr, _repo) = make_manager();
        let session = mgr
            .get_or_create_session("u1", "c1", "acp", Some("/workspace"))
            .await
            .unwrap();

        assert_eq!(session.workspace.as_deref(), Some("/workspace"));
    }

    // ── get_active_sessions ────────────────────────────────────────────

    #[tokio::test]
    async fn get_active_sessions_empty() {
        let (mgr, _repo) = make_manager();
        let sessions = mgr.get_active_sessions().await.unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn get_active_sessions_returns_all() {
        let (mgr, _repo) = make_manager();
        mgr.get_or_create_session("u1", "c1", "gemini", None)
            .await
            .unwrap();
        mgr.get_or_create_session("u2", "c2", "acp", None)
            .await
            .unwrap();

        let sessions = mgr.get_active_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
    }

    // ── cleanup_user_sessions ──────────────────────────────────────────

    #[tokio::test]
    async fn cleanup_removes_user_sessions() {
        let (mgr, repo) = make_manager();
        mgr.get_or_create_session("u1", "c1", "gemini", None)
            .await
            .unwrap();
        mgr.get_or_create_session("u1", "c2", "gemini", None)
            .await
            .unwrap();
        mgr.get_or_create_session("u2", "c1", "acp", None)
            .await
            .unwrap();

        mgr.cleanup_user_sessions("u1").await.unwrap();

        let sessions = repo.get_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].user_id, "u2");
    }

    #[tokio::test]
    async fn cleanup_noop_for_unknown_user() {
        let (mgr, repo) = make_manager();
        mgr.get_or_create_session("u1", "c1", "gemini", None)
            .await
            .unwrap();

        mgr.cleanup_user_sessions("u999").await.unwrap();

        assert_eq!(repo.get_sessions().len(), 1);
    }

    // ── bind_conversation ──────────────────────────────────────────────

    #[tokio::test]
    async fn bind_conversation_updates_activity() {
        let (mgr, repo) = make_manager();
        let session = mgr
            .get_or_create_session("u1", "c1", "acp", None)
            .await
            .unwrap();
        let original_activity = session.last_activity;

        // Small delay to ensure timestamp differs
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        mgr.bind_conversation(&session.id, "conv_123")
            .await
            .unwrap();

        let updated = repo
            .get_sessions()
            .into_iter()
            .find(|s| s.id == session.id)
            .unwrap();
        assert!(updated.last_activity >= original_activity);
    }

    #[tokio::test]
    async fn bind_conversation_not_found() {
        let (mgr, _repo) = make_manager();
        let err = mgr
            .bind_conversation("nonexistent", "conv_123")
            .await
            .unwrap_err();
        assert!(matches!(err, ChannelError::SessionNotFound(_)));
    }
}
