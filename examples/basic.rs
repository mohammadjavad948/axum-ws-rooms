use axum_ws_rooms::RoomsManager;

type Manager = RoomsManager<i32, i32, String>;

#[tokio::main]
async fn main() {
    // init manager
    let manager = std::sync::Arc::new(Manager::new());

    // create two rooms
    manager.new_room(1, None).await;
    manager.new_room(2, None).await;

    // spawn a task that acts as a receiver
    let receiver = tokio::spawn({
        let manager = manager.clone();

        async move {
            // initialise a user and join room 1 and start receiving messages
            let mut receiver = manager.init_user(1, None).await;

            let _ = manager.join_room(1, 1).await;

            while let Ok(data) = receiver.recv().await {
                println!("received data `{}`", data);
            }
        }
    });

    // spawn a task that acts as sender
    let sender = tokio::spawn({
        let manager = manager.clone();

        async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            let _ = manager
                .send_message_to_room(&1, "test message to room 1".into())
                .await;

            let _ = manager
                .send_message_to_room(&2, "test message to room 2".into())
                .await;
        }
    });

    let _ = tokio::join!(sender, receiver);
}
