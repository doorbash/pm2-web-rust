mod pm2;

use askama::Template;
use crossbeam_channel::{Sender, select};
use futures_util::{
    StreamExt,
};
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, thread, time::{Duration}};
use uuid::Uuid;
use warp::{Filter, ws::{Message, WebSocket, Ws}};
use futures_util::{SinkExt};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Template)]
#[template(path = "script.js", escape = "none")]
struct ScriptTemplate {
    actions_enabled: bool,
    time_enabled: bool,
    app_id_enabled: bool,
    app_name_enabled: bool,
}

struct Client {
    uuid: Uuid,
    ch: Sender<String>,
}

async fn logs_handler(
    ws: WebSocket,
    client_ch_s: crossbeam_channel::Sender<Client>,
    removed_client_ch_s: crossbeam_channel::Sender<Uuid>,
) {
    let (mut tx, _) = ws.split();

    let (sender, receiver) = crossbeam_channel::bounded::<String>(1);

    let uuid = Uuid::new_v4();

    let client = Client {
        uuid: uuid,
        ch: sender,
    };

    client_ch_s.send(client).unwrap();

    tokio::task::spawn(async move {
        loop {
            select! {
                recv(receiver) -> message => {
                    if tx.send(Message::text(message.unwrap())).await.is_err() {
                        removed_client_ch_s.send(uuid).unwrap();
                        break;
                    }
                }
                default => {
                    tokio::task::yield_now().await;
                }
            }
        }
    });
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let (data_ch_s, data_ch_r) = crossbeam_channel::bounded::<String>(0);
    let (clients_ch_s, clients_ch_r) = crossbeam_channel::bounded::<Client>(0);
    let (removed_clients_ch_s, removed_clients_ch_r) = crossbeam_channel::bounded::<Uuid>(0);

    pm2::PM2::start(data_ch_s, Duration::from_secs(3));
    thread::spawn(move || {
        let clients: RefCell<HashMap<Uuid, Sender<String>>> = RefCell::new(HashMap::new());
        loop {
            select! {
                recv(data_ch_r) -> data => {
                    let data = data.unwrap();
                    for client in clients.borrow().values() {
                        select! {
                            send(client, data.clone()) -> _ => (),
                            default => ()
                        }
                    }
                }
                recv(removed_clients_ch_r) -> uuid => {
                    let uuid = uuid.unwrap();
                    println!("client disconnected: {}", uuid);
                    clients.borrow_mut().remove(&uuid);
                    println!("num connected clients: {}", clients.borrow().len());
                }
                recv(clients_ch_r) -> client => {
                    let client = client.unwrap();
                    println!("client connected: {}", client.uuid);
                    clients.borrow_mut().insert(client.uuid, client.ch);
                    println!("num connected clients: {}", clients.borrow().len());
                }
            }
        }
    });
    // handler.join().unwrap();

    let c_ch_s = warp::any().map(move || clients_ch_s.clone());
    let rc_ch_s = warp::any().map(move || removed_clients_ch_s.clone());

    let index = warp::get()
        .and(warp::path::end())
        .and(warp::fs::file("./static/index.html"));
    let styles = warp::get()
        .and(warp::path("styles.css"))
        .and(warp::fs::file("./static/styles.css"));
    let script = warp::get()
        .and(warp::path("script.js"))
        .map(|| ScriptTemplate {
            actions_enabled: true,
            time_enabled: true,
            app_id_enabled: false,
            app_name_enabled: true,
        });
    let logs = warp::path("logs")
        .and(warp::ws())
        .and(c_ch_s)
        .and(rc_ch_s)
        .map(|ws: Ws, c_ch_s, rc_ch_s| {
            ws.on_upgrade(move |websocket| logs_handler(websocket, c_ch_s, rc_ch_s))
        });

    let routes = index.or(styles).or(script).or(logs);

    warp::serve(routes).run(([0, 0, 0, 0], 6969)).await;
}
