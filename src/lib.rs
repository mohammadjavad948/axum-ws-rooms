use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::atomic::{AtomicU32, Ordering},
};

use tokio::{
    sync::{broadcast, RwLock},
    task::JoinHandle,
};

/// each room has a name and it contains `broadcast::sender<String>` which can be accessed
/// by `get_sender` method and you can send message to a roome by calling `send` on room.
/// each room counts how many user it has and there is a method to check if its empty
/// each room track its joined users and stores spawned tasks handlers
struct Room<K, U, T> {
    name: K,
    tx: broadcast::Sender<T>,
    inner_user: RwLock<HashMap<U, UserTask>>,
    user_count: AtomicU32,
}

/// struct that contains task handler that forwards messages
struct UserTask {
    task: JoinHandle<()>,
}

/// use in combination with `Arc` to share it between threads
/// internally it uses `RwLock` so it can handle concurrent requests without a problem
/// when a user connects to ws endpoint you have to call `init_user` and it gives you a guard that
/// when dropped will remove user from all rooms
/// # Generics
/// `K` is type used to identify each room
///
/// `U` is type used to identify each user
///
/// `T` is message type that is sent between rooms and users
/// # Examples
/// examples are available in examples directory
pub struct RoomsManager<K, U, T> {
    inner: RwLock<HashMap<K, Room<K, U, T>>>,
    user_reciever: RwLock<HashMap<U, broadcast::Sender<T>>>,
}

#[derive(Debug)]
pub enum RoomError {
    /// room does not exists
    RoomNotFound,
    /// can not send message to room
    MessageSendFail,
    /// you have not called init_user
    NotInitiated,
}

impl Error for RoomError {}

impl fmt::Display for RoomError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RoomError::RoomNotFound => {
                write!(f, "target room not found")
            }
            RoomError::NotInitiated => {
                write!(f, "user is not initiated")
            }
            RoomError::MessageSendFail => {
                write!(f, "failed to send message to the room")
            }
        }
    }
}

pub struct UserReceiverGuard<'a, K, U, T>
where
    T: Clone + Send + 'static,
    K: Eq + std::hash::Hash + Clone,
    U: Eq + std::hash::Hash + Clone,
{
    receiver: broadcast::Receiver<T>,
    user: U,
    manager: &'a RoomsManager<K, U, T>,
}

impl<K, U, T> Room<K, U, T>
where
    T: Clone + Send + 'static,
    K: Eq + std::hash::Hash,
    U: Eq + std::hash::Hash,
{
    /// creates new room with a given name
    /// capacity is the underlying channel capacity and its default is 100
    fn new(name: K, capacity: Option<usize>) -> Room<K, U, T> {
        let (tx, _rx) = broadcast::channel(capacity.unwrap_or(100));

        Room {
            name,
            tx,
            inner_user: RwLock::new(HashMap::new()),
            user_count: AtomicU32::new(0),
        }
    }

    /// join the rooms with a unique user
    /// if user has joined before, it does nothing
    async fn join(&self, user: U, user_sender: broadcast::Sender<T>) {
        let mut inner = self.inner_user.write().await;

        match inner.entry(user) {
            std::collections::hash_map::Entry::Occupied(_) => {}
            std::collections::hash_map::Entry::Vacant(data) => {
                let mut room_rec = self.get_sender().subscribe();

                let task = tokio::spawn(async move {
                    while let Ok(data) = room_rec.recv().await {
                        let _ = user_sender.send(data);
                    }
                });

                data.insert(UserTask { task });

                self.user_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// leave the room with user
    /// if user has left before it wont do anything
    async fn leave(&self, user: U) {
        let mut inner = self.inner_user.write().await;

        match inner.entry(user) {
            std::collections::hash_map::Entry::Vacant(_) => {}
            std::collections::hash_map::Entry::Occupied(data) => {
                let data = data.remove();

                data.task.abort();

                self.user_count.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    fn blocking_leave(&self, user: U) {
        let mut inner = self.inner_user.blocking_write();

        match inner.entry(user) {
            std::collections::hash_map::Entry::Vacant(_) => {}
            std::collections::hash_map::Entry::Occupied(data) => {
                let data = data.remove();

                data.task.abort();

                self.user_count.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    async fn clear_tasks(&self) {
        let mut inner = self.inner_user.write().await;

        inner.values().for_each(|value| {
            value.task.abort();
        });

        inner.clear();

        self.user_count.store(0, Ordering::SeqCst);
    }

    /// check if user is in the room
    async fn contains_user(&self, user: &U) -> bool {
        let inner = self.inner_user.read().await;

        inner.contains_key(user)
    }

    /// checks if room is empty
    fn is_empty(&self) -> bool {
        self.user_count.load(Ordering::SeqCst) == 0
    }

    /// get sender without joining room
    fn get_sender(&self) -> broadcast::Sender<T> {
        self.tx.clone()
    }

    ///send message to room
    fn send(&self, data: T) -> Result<usize, broadcast::error::SendError<T>> {
        self.tx.send(data)
    }

    /// get user count of room
    async fn user_count(&self) -> u32 {
        self.user_count.load(Ordering::SeqCst)
    }
}

impl<K, U, T> RoomsManager<K, U, T>
where
    T: Clone + Send + 'static,
    K: Eq + std::hash::Hash + Clone,
    U: Eq + std::hash::Hash + Clone,
{
    pub fn new() -> Self {
        RoomsManager {
            inner: RwLock::new(HashMap::new()),
            user_reciever: RwLock::new(HashMap::new()),
        }
    }

    pub async fn new_room(&self, name: K, capacity: Option<usize>) {
        let mut rooms = self.inner.write().await;

        rooms.insert(name.clone(), Room::new(name, capacity));
    }

    pub async fn room_exists(&self, name: &K) -> bool {
        let rooms = self.inner.read().await;

        rooms.get(name).is_some()
    }

    pub async fn join_or_create(&self, user: U, room: K) -> Result<(), RoomError> {
        match self.room_exists(&room).await {
            true => self.join_room(room, user).await,
            false => {
                self.new_room(room.clone(), None).await;

                self.join_room(room, user).await
            }
        }
    }

    /// send a message to a room
    /// it will fail if there are no users in the room or
    /// if room does not exists
    pub async fn send_message_to_room(&self, name: &K, data: T) -> Result<usize, RoomError> {
        let rooms = self.inner.read().await;

        rooms
            .get(name)
            .ok_or(RoomError::RoomNotFound)?
            .send(data)
            .map_err(|_| RoomError::MessageSendFail)
    }

    /// call this at first of your code to initialize user notifier
    pub async fn init_user(
        &self,
        user: U,
        capacity: Option<usize>,
    ) -> UserReceiverGuard<'_, K, U, T> {
        let mut user_reciever = self.user_reciever.write().await;

        match user_reciever.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(channel) => UserReceiverGuard {
                user,
                receiver: channel.get().subscribe(),
                manager: self,
            },
            std::collections::hash_map::Entry::Vacant(v) => {
                let (tx, rx) = broadcast::channel(capacity.unwrap_or(100));
                v.insert(tx);

                UserReceiverGuard {
                    user,
                    receiver: rx,
                    manager: self,
                }
            }
        }
    }

    /// call this at end of your code to remove user from all rooms
    pub fn end_user(&self, user: U) {
        let rooms = self.inner.blocking_write();
        let mut user_reciever = self.user_reciever.blocking_write();

        for (_key, room) in rooms.iter() {
            room.blocking_leave(user.clone());
        }

        match user_reciever.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(o) => {
                o.remove();
            }
            std::collections::hash_map::Entry::Vacant(_) => {}
        }
    }

    /// join user to room
    pub async fn join_room(&self, name: K, user: U) -> Result<(), RoomError> {
        let rooms = self.inner.read().await;
        let user_reciever = self.user_reciever.read().await;

        let user_reciever = user_reciever
            .get(&user)
            .ok_or(RoomError::NotInitiated)?
            .clone();

        rooms
            .get(&name)
            .ok_or(RoomError::RoomNotFound)?
            .join(user.clone(), user_reciever)
            .await;

        Ok(())
    }

    pub async fn remove_room(&self, room: K) {
        let mut rooms = self.inner.write().await;

        match rooms.entry(room.clone()) {
            std::collections::hash_map::Entry::Vacant(_) => {}
            std::collections::hash_map::Entry::Occupied(el) => {
                let room = el.remove();

                room.clear_tasks().await;
            }
        }
    }

    pub async fn leave_room(&self, name: K, user: U) -> Result<(), RoomError> {
        let rooms = self.inner.read().await;

        rooms
            .get(&name)
            .ok_or(RoomError::RoomNotFound)?
            .leave(user.clone())
            .await;

        Ok(())
    }

    pub async fn is_room_empty(&self, name: K) -> Result<bool, RoomError> {
        let rooms = self.inner.read().await;

        Ok(rooms.get(&name).ok_or(RoomError::RoomNotFound)?.is_empty())
    }

    pub async fn rooms_count(&self) -> usize {
        let rooms = self.inner.read().await;

        rooms.len()
    }
}

impl<K, U, T> Default for RoomsManager<K, U, T>
where
    T: Clone + Send + 'static,
    K: Eq + std::hash::Hash + Clone,
    U: Eq + std::hash::Hash + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, U, T> std::ops::Deref for UserReceiverGuard<'_, K, U, T>
where
    T: Clone + Send + 'static,
    K: Eq + std::hash::Hash + Clone,
    U: Eq + std::hash::Hash + Clone,
{
    type Target = broadcast::Receiver<T>;

    fn deref(&self) -> &Self::Target {
        &self.receiver
    }
}

impl<K, U, T> std::ops::DerefMut for UserReceiverGuard<'_, K, U, T>
where
    T: Clone + Send + 'static,
    K: Eq + std::hash::Hash + Clone,
    U: Eq + std::hash::Hash + Clone,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.receiver
    }
}

impl<K, U, T> Drop for UserReceiverGuard<'_, K, U, T>
where
    T: Clone + Send + 'static,
    K: Eq + std::hash::Hash + Clone,
    U: Eq + std::hash::Hash + Clone,
{
    fn drop(&mut self) {
        self.manager.end_user(self.user.clone());
    }
}
