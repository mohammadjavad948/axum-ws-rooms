## About 

this package can manage room for axum websoket.

it is flexible and you can do what you want with rooms.

## How Does It Work

for a single room: it creates a channel for room and sends data through channel.


## Example

```rs

struct State {
  rooms: RoomsManager,
}


async fn main(){
  let rooms = axum_ws_rooms::RoomsManager::new();

  // create two rooms
  rooms.new_room("global".into(), None).await;
  rooms.new_room("room".into(), None).await;

  let state = Arc::new(State { rooms });

  // build our application with a single route
  let app = Router::new()
      .route("/", get(websocket_handler))
      .layer(Extension(state));

  // run it with axum on localhost:5000
  let addr = SocketAddr::from(([0, 0, 0, 0], 5000));
  tracing::debug!("listening on {}", addr);
  axum::Server::bind(&addr)
      .serve(app.into_make_service())
      .await
      .unwrap();
}

pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    Extension(state): Extension<Arc<State>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| websocket(socket, state))
}

async fn websocket(stream: WebSocket, state: Arc<State>) {
    // By splitting we can send and receive at the same time.
    let (mut sender, mut receiver) = stream.split();
    let mut user = UserInfo {
        user_id: 0,
        session_id: 0,
    };

    // authenticate user if it fails close the socket if its ok then break the loop;
    while let Some(Ok(Message::Text(token))) = receiver.next().await {
        // do auth stuff here
        let info = check_auth_header(&token).await;

        match info {
            Ok(user_info) => {
                user = user_info;

                break;
            }
            Err(_) => {
                sender
                    .send(Message::Text("auth failed closing socket".to_string()))
                    .await
                    .unwrap();

                sender.close().await.unwrap();

                return;
            }
        }
    }

    //init for authenticated user
    state
        .rooms
        .init_user(user.session_id.to_string(), None)
        .await;

    // join user to global room
    state
        .rooms
        .join_room("global".into(), user.session_id.to_string())
        .await
        .unwrap();

    // get receiver for user that get message from all rooms
    let mut user_receiver = state
        .rooms
        .get_user_receiver(user.session_id.to_string())
        .await
        .unwrap();

    // spawn a task to get message from rooms and send it to user
    let mut send_task = tokio::spawn(async move {
        while let Ok(data) = user_receiver.recv().await {
          sender.send(Message::Text(data)).await.unwrap();
        }
    });

    let rec_state = state.clone();

    // spawn a task to get message from user and handle things
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(data))) = receiver.next().await {
            if data.starts_with("join") {
                // we can join rooms and reciever gets message from that room
                rec_state
                    .rooms
                    .join_room("room".into(), user.session_id.to_string())
                    .await
                    .unwrap();
            }

            if data.starts_with("send") {
                // we can send message to a specific room 
                rec_state
                    .rooms
                    .send_message_to_room("room".into(), data)
                    .await
                    .unwrap();
            } else {
                rec_state
                    .rooms
                    .send_message_to_room("global".into(), data)
                    .await
                    .unwrap();
            }
        }
    });

    // If any one of the tasks exit, abort the other.
    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };

    // after connection closed you have to call this so all things gets
    // removed safely
    state.rooms.end_user(user.session_id.to_string()).await;

    println!("connection closed");
}

```
