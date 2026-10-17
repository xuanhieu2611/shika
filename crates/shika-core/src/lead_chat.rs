//! Core boundary for the app's logical Lead identity. Old actors cannot save
//! ids, receive new input or publish events after Stop/replacement.
use crate::lead_chat_store::ChatStore;
use crate::{ChatEvent, ChatOrigin, ChatRecord, Core, Error, Result};

impl Core {
    /// Synchronous quit hook: GPUI may retain the window state until process
    /// exit, so RAII alone cannot guarantee that pipe children are reaped.
    pub fn stop_lead_chats(&self) {
        let chats = self
            .chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, chat)| chat)
            .collect::<Vec<_>>();
        for chat in chats {
            chat.stop();
        }
    }

    pub fn lead_chat_events(&self, id: &str) -> Vec<ChatEvent> {
        self.chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|chat| chat.drain())
            .unwrap_or_default()
    }
    pub fn lead_chat_send(
        &self,
        id: &str,
        text: String,
        origin: ChatOrigin,
        events: Vec<(String, Option<String>)>,
    ) -> Result<()> {
        self.chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .ok_or(Error::UnknownSession)?
            .send(text, origin, events)
            .map_err(Error::LeadChat)
    }
    pub fn lead_chat_interrupt(&self, id: &str) {
        if let Some(chat) = self.chats.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
            chat.interrupt();
        }
    }
    pub fn lead_chat_answer(
        &self,
        id: &str,
        request_id: String,
        answer: Option<serde_json::Value>,
    ) {
        if let Some(chat) = self.chats.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
            chat.answer(request_id, answer);
        }
    }
    pub fn lead_chat_retry_save(&self, id: &str) {
        if let Some(chat) = self.chats.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
            chat.retry_save();
        }
    }
    /// Blocking disk read. Loading history does not start the CLI or replay input.
    pub fn lead_chat_page(&self, project: &str, before: Option<i64>) -> Result<Vec<ChatRecord>> {
        let saved = self.projects.get(project)?.lead;
        ChatStore::open(&self.data_dir, project)
            .and_then(|store| {
                if saved
                    .as_ref()
                    .and_then(|s| s.chat_history.as_ref())
                    .is_some_and(|id| id != &store.lifetime)
                {
                    return Err("Could not read this conversation: history identity changed".into());
                }
                store.page(before, 60)
            })
            .map_err(Error::LeadChat)
    }
    /// Commit a protocol-confirmed id only under the exact live logical identity.
    pub fn confirm_lead_chat(&self, id: &str, conversation: &str) -> Result<()> {
        let _guard = self.operations.lock().unwrap_or_else(|e| e.into_inner());
        let lead = self
            .sessions
            .get(id)
            .filter(|s| s.lead && s.preset_id == "claude")
            .ok_or(Error::UnknownSession)?;
        if !self
            .chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
        {
            return Err(Error::UnknownSession);
        }
        let saved = self
            .projects
            .get(&lead.project_id)?
            .lead
            .ok_or(Error::NoSavedLead)?;
        if saved.cli != "claude"
            || saved
                .session_id
                .as_ref()
                .is_some_and(|saved| saved != conversation)
        {
            return Err(Error::LeadChat(
                "Claude Code returned a different conversation".into(),
            ));
        }
        self.projects
            .set_lead_session(&lead.project_id, Some(conversation.into()))
    }
}
