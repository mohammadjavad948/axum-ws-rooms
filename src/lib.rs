use std::{
    collections::HashMap,
    pin::Pin,
    sync::atomic::{AtomicU32, Ordering},
    task::{Context, Poll},
};

use std::future::Future;
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
    tx: broadcast::Sender<String>,
    inner_user: Mutex<Vec<String>>,
    user_count: AtomicU32,
}

#[derive(Clone)]
pub enum Notify {
    Join,
    Leave,
}

pub struct RoomsManager {
    inner: Mutex<HashMap<String, Room>>,
    users_room: Mutex<HashMap<String, Vec<String>>>,
    user_notify: Mutex<HashMap<String, broadcast::Sender<Notify>>>,
}

impl Room {
    /// creates new room with a given name
    /// capacity is the underlying channel capacity and its default is 100
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
            user_notify: Mutex::new(HashMap::new()),
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

    /// joins user to rooms and return reciever
    pub async fn room_reciever(
        &self,
        name: String,
        user: String,
    ) -> Result<broadcast::Receiver<String>, &'static str> {
        Ok(self.join_room(name, user).await?.subscribe())
    }

    pub async fn recieve_multi(&self, names: Vec<String>, user: String) -> MultiRoomListen {
        let rooms = self.inner.lock().await;
        let mut rooms_rx = vec![];

        for name in names {
            let room = rooms.get(&name);

            match room {
                Some(room) => {
                    let room_rx = room.join(user.clone()).await.subscribe();
                    rooms_rx.push(room_rx);
                }
                None => continue,
            }
        }

        MultiRoomListen { rooms_rx }
    }

    /// returns a MultiRoomListen with all rooms user joined in
    pub async fn recieve_joined_rooms(
        &self,
        user: String,
    ) -> Result<MultiRoomListen, &'static str> {
        let rooms = self.inner.lock().await;
        let user_room = self.users_room.lock().await;

        let mut rooms_rx = vec![];

        let names = user_room.get(&user).ok_or("no rooms for user")?;

        for name in names {
            let room = rooms.get(name);

            match room {
                Some(room) => {
                    let room_rx = room.join(user.clone()).await.subscribe();
                    rooms_rx.push(room_rx);
                }
                None => continue,
            }
        }

        Ok(MultiRoomListen { rooms_rx })
    }

    /// get user notifyer
    /// panics if you didnt call init_user before
    pub async fn notify_reciever(&self, user: String) -> broadcast::Receiver<Notify> {
        let notify = self.user_notify.lock().await;

        notify
            .get(&user)
            .expect("can not get user notifyer maybe you didnt call init_user?")
            .clone()
            .subscribe()
    }

    /// call this at first of your code to initialize user notifyer
    /// your app will panic if you try to join or leave a room befor calling this function
    pub async fn init_user(&self, user: String, capacity: Option<usize>) {
        let mut notify = self.user_notify.lock().await;

        match notify.entry(user) {
            std::collections::hash_map::Entry::Occupied(_) => {}
            std::collections::hash_map::Entry::Vacant(v) => {
                let (tx, _rx) = broadcast::channel(capacity.unwrap_or(100));
                v.insert(tx);
            }
        }
    }

    /// call this at end of your code to remove user from all rooms
    pub async fn end_user(&self, user: String) {
        let mut notify = self.user_notify.lock().await;
        let mut user_room = self.users_room.lock().await;

        match notify.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(o) => {
                o.remove();
            }
            std::collections::hash_map::Entry::Vacant(_) => {}
        }

        match user_room.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(o) => {
                let user_rooms = o.get();

                for room in user_rooms {
                    let _ = self.leave_room(room.clone(), user.clone()).await;
                }

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
        let notify = self.user_notify.lock().await;

        let sender = rooms
            .get(&name)
            .ok_or("can not get room")?
            .join(user.clone())
            .await;

        match users.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let rooms = o.get_mut();

                let has = rooms.iter().any(|x| x == &name);

                if !has {
                    rooms.push(name.clone());
                }
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(vec![name.clone()]);
            }
        };

        let _ = notify
            .get(&user)
            .expect("can not get user notifyer. maybe you didnt use init_user?")
            .send(Notify::Join);

        Ok(sender)
    }

    pub async fn leave_room(&self, name: String, user: String) -> Result<(), &'static str> {
        let rooms = self.inner.lock().await;
        let mut users = self.users_room.lock().await;
        let notify = self.user_notify.lock().await;

        rooms
            .get(&name)
            .ok_or("can not get room")?
            .leave(user.clone())
            .await;

        match users.entry(user.clone()) {
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let vecotr = o.get_mut();

                vecotr.retain(|x| *x != name);
            }
            std::collections::hash_map::Entry::Vacant(_) => {}
        }

        let _ = notify
            .get(&user)
            .expect("can not get user notifyer. maybe you didnt use init_user?")
            .send(Notify::Leave);

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
}

impl Default for RoomsManager {
    fn default() -> Self {
        Self::new()
    }
}

/// some dark magic here
pub struct MultiRoomListen {
    rooms_rx: Vec<broadcast::Receiver<String>>,
}

impl Future for MultiRoomListen {
    type Output = Result<String, broadcast::error::RecvError>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<String, broadcast::error::RecvError>> {
        for rx in self.rooms_rx.iter_mut() {
            let rx = rx.recv();

            if let Poll::Ready(val) = Box::pin(rx).as_mut().poll(cx) {
                return Poll::Ready(val);
            }
        }

        Poll::Pending
    }
}

// fix tests
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::select;

    use crate::{Room, RoomsManager};

    #[tokio::test]
    async fn can_create_room() {
        let room = Room::new("my_room".into(), None);

        assert_eq!(room.name, "my_room");
    }

    #[tokio::test]
    async fn can_send_message_to_room() {
        let room = Arc::new(Room::new("my_room".into(), None));

        let room1 = room.clone();

        tokio::spawn(async move {
            assert_eq!(room1.recieve("user1".into()).await.unwrap(), "hello");
        });

        tokio::spawn(async move {
            room.send("hello".into()).unwrap();
        });
    }

    #[tokio::test]
    async fn test_room_feature() {
        let rooms = Arc::new(RoomsManager::default());

        rooms.new_room("room1".into(), None).await;
        rooms.new_room("room2".into(), None).await;

        let room_clone1 = rooms.clone();
        let room_clone2 = rooms.clone();

        tokio::spawn(async move {
            let mut datas = vec![];

            while let Ok(data) = room_clone1
                .recieve_multi(vec!["room1".into(), "room2".into()], "user1".into())
                .await
                .await
            {
                datas.push(data);
            }

            assert_eq!(datas.len(), 2);
        });

        tokio::spawn(async move {
            room_clone2
                .send_message_to_room("room1".into(), "hello".into())
                .await
                .unwrap();

            room_clone2
                .send_message_to_room("room2".into(), "hello".into())
                .await
                .unwrap();
        });
    }

    #[tokio::test]
    async fn can_send_message_to_multiple_user() {
        let room = Arc::new(Room::new("my_room".into(), None));

        let room1 = room.clone();
        let room2 = room.clone();

        tokio::spawn(async move {
            assert_eq!(room1.recieve("user1".into()).await.unwrap(), "hello");
        });

        tokio::spawn(async move {
            assert_eq!(room2.recieve("user2".into()).await.unwrap(), "hello");
        });

        tokio::spawn(async move {
            room.send("hello".into()).unwrap();
        });
    }

    #[tokio::test]
    async fn can_recieve_from_multiple_room() {
        let room = Arc::new(Room::new("my_room1".into(), None));
        let room1 = Arc::new(Room::new("my_room2".into(), None));

        let roomc = room.clone();
        let room1c = room.clone();

        tokio::spawn(async move {
            loop {
                let data = select! {
                    Ok(msg) = room1.recieve("user1".into()) => msg,
                    Ok(msg) = room.recieve("user1".into()) => msg,
                    else => break,
                };

                assert_eq!(data, "hello");
            }
        });

        tokio::spawn(async move {
            roomc.send("hello".into()).unwrap();
            room1c.send("hello".into()).unwrap();
        });
    }

    #[tokio::test]
    async fn can_create_multiple_room() {
        let rooms = RoomsManager::default();

        rooms.new_room("room1".into(), None).await;
        rooms.new_room("room2".into(), None).await;

        assert_eq!(rooms.rooms_count().await, 2);
        assert_eq!(rooms.is_room_empty("room1".into()).await, Ok(true));
        assert_eq!(rooms.is_room_empty("room2".into()).await, Ok(true));
    }

    #[tokio::test]
    async fn can_recieve_from_room_manager() {
        let rooms = Arc::new(RoomsManager::default());

        rooms.new_room("room1".into(), None).await;
        rooms.new_room("room2".into(), None).await;

        let room_clone1 = rooms.clone();
        let room_clone2 = rooms.clone();

        tokio::spawn(async move {
            loop {
                let data = select! {
                    Ok(msg) = room_clone1.recieve("room1".into(), "user1".into()) => msg,
                    Ok(msg) = room_clone1.recieve("room2".into(), "user1".into()) => msg,
                    else => break,
                };

                assert_eq!(data, "hello");
            }
        });

        tokio::spawn(async move {
            room_clone2
                .send_message_to_room("room1".into(), "hello".into())
                .await
                .unwrap();

            room_clone2
                .send_message_to_room("room2".into(), "hello".into())
                .await
                .unwrap();
        });
    }

    #[tokio::test]
    async fn can_recieve_multiple_message() {
        let rooms = Arc::new(RoomsManager::default());

        rooms.new_room("room1".into(), None).await;
        rooms.new_room("room2".into(), None).await;

        let room_clone1 = rooms.clone();
        let room_clone2 = rooms.clone();

        tokio::spawn(async move {
            let mut d = Vec::new();

            loop {
                let data = select! {
                    Ok(msg) = room_clone1.recieve("room1".into(), "user1".into()) => msg,
                    Ok(msg) = room_clone1.recieve("room2".into(), "user1".into()) => msg,
                    else => break,
                };

                d.push(data);
            }

            assert_eq!(d.len(), 3);
        });

        tokio::spawn(async move {
            room_clone2
                .send_message_to_room("room1".into(), "hello".into())
                .await
                .unwrap();

            room_clone2
                .send_message_to_room("room1".into(), "hello".into())
                .await
                .unwrap();

            room_clone2
                .send_message_to_room("room2".into(), "hello".into())
                .await
                .unwrap();
        });
    }
}
