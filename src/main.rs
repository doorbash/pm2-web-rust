mod pm2;

use actix_files::Files;
use actix_web::{App, Error, HttpRequest, HttpResponse, HttpServer, Result, middleware::Logger, web, rt};
use askama::Template;
use crossbeam_channel::{select, Sender};
use std::{cell::RefCell, collections::HashMap, thread, time::{Duration, Instant}};
use uuid::Uuid;
use futures_util::{StreamExt, future::{self, Either}};
use tokio::{pin, time::interval};
use actix_ws::Message;

/// Should be half (or less) of the acceptable client timeout.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// How long before lack of client response causes a timeout.
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

async fn js_handler(_: HttpRequest) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().content_type("text/javascript").body(
        ScriptTemplate {
            actions_enabled: true,
            time_enabled: true,
            app_id_enabled: false,
            app_name_enabled: true,
        }
        .render()
        .unwrap(),
    ))
}

async fn logs_handler(req: HttpRequest, stream: web::Payload) -> Result<HttpResponse, Error> {
    let (res, mut session, mut msg_stream) = actix_ws::handle(&req, stream)?;

    let client_ch_s = req.app_data::<Sender<Client>>().unwrap().clone();
    let removed_client_ch_s = req.app_data::<Sender<Uuid>>().unwrap().clone();

    let (sender, receiver) = crossbeam_channel::bounded::<String>(0);

    let uuid = Uuid::new_v4();

    let client = Client {
        uuid: uuid,
        ch: sender,
    };

    client_ch_s.send(client).unwrap();

    let mut s = session.clone();

    rt::spawn(async move {
        log::info!("connected");
    
        let mut last_heartbeat = Instant::now();
        let mut interval = interval(HEARTBEAT_INTERVAL);
    
        let reason = loop {
            // create "next client timeout check" future
            let tick = interval.tick();
            // required for select()
            pin!(tick);
    
            // waits for either `msg_stream` to receive a message from the client or the heartbeat
            // interval timer to tick, yielding the value of whichever one is ready first
            match future::select(msg_stream.next(), tick).await {
                // received message from WebSocket client
                Either::Left((Some(Ok(msg)), _)) => {
                    log::debug!("msg: {msg:?}");
    
                    match msg {
                        Message::Text(text) => {
                            session.text(text).await.unwrap();
                        }
    
                        Message::Binary(bin) => {
                            session.binary(bin).await.unwrap();
                        }
    
                        Message::Close(reason) => {
                            break reason;
                        }
    
                        Message::Ping(bytes) => {
                            // log::info!("ping!");
                            last_heartbeat = Instant::now();
                            let _ = session.pong(&bytes).await;
                        }
    
                        Message::Pong(_) => {
                            // log::info!("pong!");
                            last_heartbeat = Instant::now();
                        }
    
                        Message::Continuation(_) => {
                            log::warn!("no support for continuation frames");
                        }
    
                        // no-op; ignore
                        Message::Nop => {}
                    };
                }
    
                // client WebSocket stream error
                Either::Left((Some(Err(err)), _)) => {
                    log::error!("{}", err);
                    break None;
                }
    
                // client WebSocket stream ended
                Either::Left((None, _)) => break None,
    
                // heartbeat interval ticked
                Either::Right((_inst, _)) => {
                    // if no heartbeat ping/pong received recently, close the connection
                    if Instant::now().duration_since(last_heartbeat) > CLIENT_TIMEOUT {
                        log::info!(
                            "client has not sent heartbeat in over {CLIENT_TIMEOUT:?}; disconnecting"
                        );
    
                        break None;
                    }

                    select! {
                        recv(receiver) -> data => {
                            let data = data.unwrap();
                            // println!("new data: {}", data);
                            s.text(data).await;
                        }
                        recv(crossbeam_channel::after(Duration::from_secs(3))) -> _ => {}
                    }
    
                    // send heartbeat ping
                    let _ = session.ping(b"").await;
                }
            }
        };

        removed_client_ch_s.send(uuid).unwrap();
    
        // attempt to close connection gracefully
        let _ = session.close(reason).await;
    
        log::info!("disconnected");
    });

    Ok(res)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // pretty_env_logger::init();

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
                }
                recv(clients_ch_r) -> client => {
                    let client = client.unwrap();
                    println!("client connected: {}", client.uuid);
                    clients.borrow_mut().insert(client.uuid, client.ch);
                }
            }
        }
    });
    // handler.join().unwrap();

    HttpServer::new(move || {
        App::new()
            // .wrap(Logger::default())
            .app_data(clients_ch_s.clone())
            .app_data(removed_clients_ch_s.clone())
            .route("/script.js", web::get().to(js_handler))
            // .route("/logs", web::get().to(logs_handler))
            .service(web::resource("/logs").route(web::get().to(logs_handler)))
            .service(Files::new("/", "./static/").index_file("index.html"))
    })
    .bind(("0.0.0.0", 6060))?
    .workers(10)
    .run()
    .await
}
