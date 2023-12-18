use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::atomic::{AtomicU32, Ordering},
};

use tokio::{
    sync::{broadcast, Mutex},
    task::JoinHandle,
};

/// each room has a name and id and it contains `broadcast::sender<String>` which can be accessed
/// by `get_sender` method and you can send message to a roome by calling `send` on room.
/// each room counts how many user it has and there is a method to check if its empty
/// if you want to join a room you can call `join` method and recieve a `broadcast::Sender<String>`
/// which you can subscribe to it and listen for incoming messages.
/// remember to leave the room after the user disconnects.
pub struct Room<T> {
    pub name: String,
    tx: broadcast::Sender<T>,
    inner_user: Mutex<Vec<String>>,
    user_count: AtomicU32,
}

/// this struct is used for managing multiple rooms at once
/// ## how it works
/// everything is managed by a mutex
/// there is a hashmap of rooms
/// a hashmap of user reciever
/// and a hashmap of user task
/// when a user joins a room a task will be created and all of that rooms
/// message will be forwarded to user reciever
/// and then user can listen to its own user reciever and recieve message from all joind rooms
pub struct RoomsManager<T> {
    inner: Mutex<HashMap<String, Room<T>>>,
    users_room: Mutex<HashMap<String, Vec<UserTask>>>,
    user_reciever: Mutex<HashMap<String, broadcast::Sender<T>>>,
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

struct UserTask {
    room_name: String,
    task: JoinHandle<()>,
}

impl<T> Room<T>
where
    T: Clone + Send + 'static,
{
    /// creates new room with a given name
    /// capacity is the underlying channel capacity and its default is 100
    pub fn new(name: String, capacity: Option<usize>) -> Room<T> {
        let (tx, _rx) = broadcast::channel(capacity.unwrap_or(100));

        Room {
            name,
            tx,
            inner_user: Mutex::new(vec![]),
            user_count: AtomicU32::new(0),
        }
    }

    /// join the rooms with a unique user
    /// if user has joined before, it just returns the sender
    pub async fn join(&self, user: String) -> broadcast::Sender<T> {
        let mut inner = self.inner_user.lock().await;

        if !inner.contains(&user) {
            inner.push(user);

            self.user_count.fetch_add(1, Ordering::SeqCst);
        }

        self.tx.clone()
    }

    /// leave the room with user
    /// if user has left before it wont do anything
    pub async fn leave(&self, user: String) {
        let mut inner = self.inner_user.lock().await;

        if let Some(pos) = inner.iter().position(|x| *x == user) {
            inner.swap_remove(pos);

            self.user_count.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// this method will join the user and return a reciever
    pub async fn recieve(&self, user: String) -> broadcast::Receiver<T> {
        self.join(user).await.subscribe()
    }

    /// check if user is in the room
    pub async fn contains_user(&self, user: &String) -> bool {
        let inner = self.inner_user.lock().await;

        inner.contains(user)
    }

    /// checks if room is empty
    pub fn is_empty(&self) -> bool {
        self.user_count.load(Ordering::SeqCst) == 0
    }

    /// get sender without joining room
    pub fn get_sender(&self) -> broadcast::Sender<T> {
        self.tx.clone()
    }

    ///send message to room
    pub fn send(&self, data: T) -> Result<usize, broadcast::error::SendError<T>> {
        self.tx.send(data)
    }

    /// this method locks on user and give it to you
    /// pls drop it when you dont need it
    pub async fn users(&self) -> tokio::sync::MutexGuard<Vec<String>> {
        self.inner_user.lock().await
    }

    /// get user count of room
    pub async fn user_count(&self) -> u32 {
        self.user_count.load(Ordering::SeqCst)
    }
}

impl<T> RoomsManager<T>
where
    T: Clone + Send + 'static,
{
    pub fn new() -> Self {
        RoomsManager {
            inner: Mutex::new(HashMap::new()),
            users_room: Mutex::new(HashMap::new()),
            user_reciever: Mutex::new(HashMap::new()),
        }
    }

    pub async fn new_room(&self, name: String, capacity: Option<usize>) {
        let mut rooms = self.inner.lock().await;

        rooms.insert(name.clone(), Room::new(name, capacity));
    }

    pub async fn room_exists(&self, name: &str) -> bool {
        let rooms = self.inner.lock().await;

        match rooms.get(name) {
            Some(_) => true,
            None => false,
        }
    }

    pub async fn join_or_create(
        &self,
        user: String,
        room: String,
    ) -> Result<broadcast::Sender<T>, RoomError> {
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
    pub async fn send_message_to_room(&self, name: String, data: T) -> Result<usize, RoomError> {
        let rooms = self.inner.lock().await;

        rooms
            .get(&name)
            .ok_or(RoomError::RoomNotFound)?
            .send(data)
            .map_err(|_| RoomError::MessageSendFail)
    }

    /// call this at first of your code to initialize user notifyer
    pub async fn init_user(&self, user: String, capacity: Option<usize>) {
        let mut user_reciever = self.user_reciever.lock().await;

        match user_reciever.entry(user) {
            std::collections::hash_map::Entry::Occupied(_) => {}
            std::collections::hash_map::Entry::Vacant(v) => {
                let (tx, _rx) = broadcast::channel(capacity.unwrap_or(100));
                v.insert(tx);
            }
        }
    }

    /// call this at end of your code to remove user from all rooms
    pub async fn end_user(&self, user: String) {
        let rooms = self.inner.lock().await;
        let mut user_room = self.users_room.lock().await;
        let mut user_reciever = self.user_reciever.lock().await;

        match user_room.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(o) => {
                let user_rooms = o.get();

                for task in user_rooms {
                    let room = rooms.get(&task.room_name);

                    if let Some(room) = room {
                        room.leave(user.clone()).await;
                    }

                    task.task.abort();
                }

                o.remove();
            }
            std::collections::hash_map::Entry::Vacant(_) => {}
        }

        match user_reciever.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(o) => {
                o.remove();
            }
            std::collections::hash_map::Entry::Vacant(_) => {}
        }
    }

    /// join user to room
    pub async fn join_room(
        &self,
        name: String,
        user: String,
    ) -> Result<broadcast::Sender<T>, RoomError> {
        let rooms = self.inner.lock().await;
        let mut users = self.users_room.lock().await;
        let user_reciever = self.user_reciever.lock().await;

        let sender = rooms
            .get(&name)
            .ok_or(RoomError::RoomNotFound)?
            .join(user.clone())
            .await;

        let user_reciever = user_reciever
            .get(&user)
            .ok_or(RoomError::NotInitiated)?
            .clone();

        let mut task_recv = sender.subscribe();

        let task = tokio::spawn(async move {
            while let Ok(data) = task_recv.recv().await {
                let _ = user_reciever.send(data);
            }
        });

        match users.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let rooms = o.get_mut();

                let has = rooms.iter().any(|x| x.room_name == name);

                if !has {
                    rooms.push(UserTask {
                        room_name: name,
                        task,
                    });
                }
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(vec![UserTask {
                    room_name: name,
                    task,
                }]);
            }
        };

        Ok(sender)
    }

    pub async fn remove_room(&self, room: String) {
        let mut rooms = self.inner.lock().await;
        let mut users = self.users_room.lock().await;

        match rooms.entry(room.clone()) {
            std::collections::hash_map::Entry::Vacant(_) => {}
            std::collections::hash_map::Entry::Occupied(el) => {
                for user in el.get().users().await.iter() {
                    if let std::collections::hash_map::Entry::Occupied(mut user_task) =
                        users.entry(user.into())
                    {
                        let vecotr = user_task.get_mut();

                        vecotr.retain(|task| {
                            if task.room_name == room {
                                task.task.abort();
                            }

                            task.room_name != room
                        });
                    }
                }

                el.remove();
            }
        }
    }

    pub async fn leave_room(&self, name: String, user: String) -> Result<(), RoomError> {
        let rooms = self.inner.lock().await;
        let mut users = self.users_room.lock().await;

        rooms
            .get(&name)
            .ok_or(RoomError::RoomNotFound)?
            .leave(user.clone())
            .await;

        match users.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let vecotr = o.get_mut();

                vecotr.retain(|task| {
                    if task.room_name == name {
                        task.task.abort();
                    }

                    task.room_name != name
                });
            }
            std::collections::hash_map::Entry::Vacant(_) => {}
        }

        Ok(())
    }

    pub async fn is_room_empty(&self, name: String) -> Result<bool, RoomError> {
        let rooms = self.inner.lock().await;

        Ok(rooms.get(&name).ok_or(RoomError::RoomNotFound)?.is_empty())
    }

    pub async fn rooms_count(&self) -> usize {
        let rooms = self.inner.lock().await;

        rooms.len()
    }

    pub async fn get_user_receiver(
        &self,
        name: String,
    ) -> Result<broadcast::Receiver<T>, RoomError> {
        let rx = self.user_reciever.lock().await;

        let rx = rx.get(&name).ok_or(RoomError::NotInitiated)?.subscribe();

        Ok(rx)
    }
}

impl<T> Default for RoomsManager<T>
where
    T: Clone + Send + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
