//! Ephemeral prompts for an ACP authentication request, separate from task history.
use agent_client_protocol::schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationAction,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};

pub struct AuthenticationPrompt {
    pub id: String,
    pub request: CreateElicitationRequest,
}

pub struct AuthenticationBroker {
    events: mpsc::UnboundedSender<AuthenticationPrompt>,
    pending: Mutex<HashMap<String, oneshot::Sender<CreateElicitationResponse>>>,
    cancelled: watch::Sender<bool>,
}

impl AuthenticationBroker {
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<AuthenticationPrompt>) {
        let (events, receiver) = mpsc::unbounded_channel();
        let (cancelled, _) = watch::channel(false);
        (
            Arc::new(Self {
                events,
                pending: Mutex::new(HashMap::new()),
                cancelled,
            }),
            receiver,
        )
    }

    pub async fn prompt(&self, request: CreateElicitationRequest) -> CreateElicitationResponse {
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), sender);
        if *self.cancelled.borrow()
            || self
                .events
                .send(AuthenticationPrompt {
                    id: id.clone(),
                    request,
                })
                .is_err()
        {
            self.pending.lock().unwrap().remove(&id);
            return CreateElicitationResponse::new(ElicitationAction::Cancel);
        }
        receiver
            .await
            .unwrap_or_else(|_| CreateElicitationResponse::new(ElicitationAction::Cancel))
    }

    pub fn respond(&self, id: &str, response: CreateElicitationResponse) -> bool {
        self.pending
            .lock()
            .unwrap()
            .remove(id)
            .is_some_and(|sender| sender.send(response).is_ok())
    }

    pub fn cancel(&self) {
        self.cancelled.send_replace(true);
        self.pending.lock().unwrap().clear();
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.cancelled.subscribe();
        while !*receiver.borrow() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancellation_releases_authentication_prompts_without_a_task() {
        let (broker, mut events) = AuthenticationBroker::new();
        let request=serde_json::from_value(serde_json::json!({"mode":"url","requestId":"authentication","elicitationId":"challenge","message":"Sign in","url":"https://example.invalid/login"})).unwrap();
        let waiting = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.prompt(request).await })
        };
        let prompt = events.recv().await.unwrap();
        assert!(!broker.respond(
            "another-flow",
            CreateElicitationResponse::new(ElicitationAction::Cancel)
        ));
        broker.cancel();
        assert!(matches!(
            waiting.await.unwrap().action,
            ElicitationAction::Cancel
        ));
        assert!(!broker.respond(
            &prompt.id,
            CreateElicitationResponse::new(ElicitationAction::Cancel)
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), broker.cancelled())
            .await
            .unwrap();
    }
}
