use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

/// each room has a name and id and it contains `broadcast::sender<String>` which can be accessed
/// by `get_sender` method and you can send message to a roome by calling `send` on room.
/// each room counts how many user it has and there is a method to check if its empty
/// if you want to join a room you can call `join` method and recieve a `broadcast::Sender<String>`
/// which you can subscribe to it and listen for incoming messages.
/// remember to leave the room after the user disconnects.
pub struct Room {
    pub id: String,
    pub name: String,
    pub tx: broadcast::Sender<String>,
    pub inner_user: Mutex<Vec<String>>,
    pub user_count: AtomicU32,
}

impl Room {
    pub fn new(name: String, capacity: Option<usize>) -> Room {
        let (tx, _rx) = broadcast::channel(capacity.unwrap_or(100));

        Room {
            id: Uuid::new_v4().to_string(),
            name,
            tx,
            inner_user: Mutex::new(vec![]),
            user_count: AtomicU32::new(0),
        }
    }

    pub async fn join(&self, user: String) -> broadcast::Sender<String> {
        let mut inner = self.inner_user.lock().await;

        if !inner.contains(&user) {
            inner.push(user);

            self.user_count.fetch_add(1, Ordering::SeqCst);
        }

        self.tx.clone()
    }

    pub async fn leave(&self, user: String) {
        let mut inner = self.inner_user.lock().await;

        if let Some(pos) = inner.iter().position(|x| *x == user) {
            inner.swap_remove(pos);

            self.user_count.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub async fn contains_user(&self, user: &String) -> bool {
        let inner = self.inner_user.lock().await;

        inner.contains(user)
    }

    pub fn is_empty(&self) -> bool {
        self.user_count.load(Ordering::SeqCst) == 0
    }

    pub fn get_sender(&self) -> broadcast::Sender<String> {
        self.tx.clone()
    }

    pub fn send(&self, data: String) -> Result<usize, broadcast::error::SendError<String>> {
        self.tx.send(data)
    }
}
