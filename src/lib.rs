use std::{
    collections::HashMap,
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
pub struct Room {
    pub name: String,
    tx: broadcast::Sender<String>,
    inner_user: Mutex<Vec<String>>,
    user_count: AtomicU32,
}

pub struct RoomsManager {
    inner: Mutex<HashMap<String, Room>>,
    users_room: Mutex<HashMap<String, Vec<UserTask>>>,
    user_reciever: Mutex<HashMap<String, broadcast::Sender<String>>>,
}

struct UserTask {
    room_name: String,
    task: JoinHandle<()>,
}

impl Room {
    /// creates new room with a given name
    /// capacity is the underlying channel capacity and its default is 100
    pub fn new(name: String, capacity: Option<usize>) -> Room {
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
    pub async fn join(&self, user: String) -> broadcast::Sender<String> {
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
    pub async fn recieve(&self, user: String) -> broadcast::Receiver<String> {
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
    pub fn get_sender(&self) -> broadcast::Sender<String> {
        self.tx.clone()
    }

    ///send message to room
    pub fn send(&self, data: String) -> Result<usize, broadcast::error::SendError<String>> {
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

impl RoomsManager {
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

    /// send a message to a room
    /// it will fail if there are no users in the room or
    /// if room does not exists
    pub async fn send_message_to_room(
        &self,
        name: String,
        data: String,
    ) -> Result<usize, &'static str> {
        let rooms = self.inner.lock().await;

        rooms
            .get(&name)
            .ok_or("cant get room")?
            .send(data)
            .map_err(|_| "cant send data")
    }

    /// call this at first of your code to initialize user notifyer
    /// your app will panic if you try to join or leave a room befor calling this function
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
    ) -> Result<broadcast::Sender<String>, &'static str> {
        let rooms = self.inner.lock().await;
        let mut users = self.users_room.lock().await;
        let user_reciever = self.user_reciever.lock().await;

        let sender = rooms
            .get(&name)
            .ok_or("can not get room")?
            .join(user.clone())
            .await;

        let user_reciever = user_reciever
            .get(&user)
            .expect("can not get user reciever. maybe you didnt use init_user?")
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

    pub async fn leave_room(&self, name: String, user: String) -> Result<(), &'static str> {
        let rooms = self.inner.lock().await;
        let mut users = self.users_room.lock().await;

        rooms
            .get(&name)
            .ok_or("can not get room")?
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

    pub async fn is_room_empty(&self, name: String) -> Result<bool, &'static str> {
        let rooms = self.inner.lock().await;

        Ok(rooms.get(&name).ok_or("can not get room")?.is_empty())
    }

    pub async fn rooms_count(&self) -> usize {
        let rooms = self.inner.lock().await;

        rooms.len()
    }

    pub async fn get_user_receiver(
        &self,
        name: String,
    ) -> Result<broadcast::Receiver<String>, &'static str> {
        let rx = self.user_reciever.lock().await;

        let rx = rx
            .get(&name)
            .ok_or("can not get user reciever maybe you didnt call user_init?")?
            .subscribe();

        Ok(rx)
    }
}

impl Default for RoomsManager {
    fn default() -> Self {
        Self::new()
    }
}
