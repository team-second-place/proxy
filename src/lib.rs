use std::sync::Arc;

use axum::{routing::IntoMakeService, Router};
use dashmap::DashMap;
use messages::{MicrocontrollerId, UserId};
use tokio::sync::RwLock;

mod one_to_many;
mod routes;

type WebsocketReceiver =
    futures::channel::mpsc::Receiver<Result<axum::extract::ws::Message, axum::Error>>;
type WebsocketSender = futures::channel::mpsc::Sender<axum::extract::ws::Message>;

type MicrocontrollerSender = WebsocketSender;
type UserSender = WebsocketSender;

#[derive(Debug, Default)]
pub struct InnerAppState {
    pub authentications: RwLock<one_to_many::OneToMany<MicrocontrollerId, UserId>>,

    pub microcontrollers: DashMap<MicrocontrollerId, MicrocontrollerSender>,
    pub users: DashMap<UserId, UserSender>,
}

type AppState = Arc<InnerAppState>;

pub type App = IntoMakeService<Router>;

pub fn create_app(state: AppState) -> App {
    routes::create_router()
        .with_state(state)
        .into_make_service()
}

pub fn create_default_state() -> AppState {
    Arc::new(InnerAppState::default())
}

#[cfg(test)]
mod integration_tests {
    // TODO: write an integration test mimicking real life flow (user sends a command, system does something, system sends data back)

    use std::{
        net::{Ipv4Addr, SocketAddr},
        time::Duration,
    };

    use futures::{SinkExt, StreamExt};

    use messages::*;
    use tokio::time::timeout;

    use super::{create_app, create_default_state, routes::*};

    #[tokio::test]
    #[ntest::timeout(2000)]
    async fn system_is_told_when_no_users_are_connected() {
        let state = create_default_state();
        let server = axum::Server::bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .serve(create_app(state));
        let addr = server.local_addr();
        tokio::spawn(server);

        // Giving this a quick glance, this test does not represent the behavior we want
        todo!();

        let (mut microcontroller_socket, _microcontroller_response) =
            tokio_tungstenite::connect_async(format!("ws://{addr}{MICROCONTROLLER}"))
                .await
                .unwrap();

        let first_microcontroller_message_received =
            timeout(Duration::from_secs(1), microcontroller_socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        let first_microcontroller_message_received = match first_microcontroller_message_received {
            tungstenite::Message::Binary(msg) => msg,
            other => panic!("expected a binary message but got {other:?}"),
        };
        let first_microcontroller_message_received =
            decode_to_microcontroller(&first_microcontroller_message_received).unwrap();
        match first_microcontroller_message_received {
            ToMicrocontroller::UsersAreOffline => {}
            other => panic!("expected UsersAreOffline but got {other:?}"),
        };
    }

    #[tokio::test]
    #[ntest::timeout(2000)]
    async fn user_receives_data_from_microcontroller() {
        let state = create_default_state();
        let server = axum::Server::bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .serve(create_app(state));
        let addr = server.local_addr();
        tokio::spawn(server);

        // Giving this a quick glance, this test does not represent the behavior we want
        todo!();

        let (mut microcontroller_socket, _microcontroller_response) =
            tokio_tungstenite::connect_async(format!("ws://{addr}{MICROCONTROLLER}"))
                .await
                .unwrap();
        let (mut user_socket, _user_response) =
            tokio_tungstenite::connect_async(format!("ws://{addr}{USER}"))
                .await
                .unwrap();

        let first_microcontroller_message_data = Data::CurrentLight(CurrentLight { on: false });
        let first_microcontroller_message =
            FromMicrocontroller::BroadcastData(first_microcontroller_message_data.clone());
        let first_microcontroller_message_encoded =
            encode_from_microcontroller(&first_microcontroller_message).unwrap();
        microcontroller_socket
            .send(tungstenite::Message::Binary(
                first_microcontroller_message_encoded,
            ))
            .await
            .unwrap();

        let first_user_message_received = timeout(Duration::from_secs(1), user_socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let first_user_message_received = match first_user_message_received {
            tungstenite::Message::Binary(msg) => msg,
            other => panic!("expected a binary message but got {other:?}"),
        };
        let first_user_message_received = decode_to_user(&first_user_message_received).unwrap();
        let first_user_message_received_data = match first_user_message_received {
            ToUser::Data(data) => data,
            other => panic!("expected data but got {other:?}"),
        };

        assert_eq!(
            first_user_message_received_data,
            first_microcontroller_message_data
        );
    }
}
