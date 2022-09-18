mod pm2;

use actix_files::Files;
use actix_web::{App, Error, HttpRequest, HttpResponse, HttpServer, Result, web, rt};
use askama::Template;
use crossbeam_channel::{select, Sender};
use std::{cell::RefCell, collections::HashMap, thread, time::{Duration}};
use uuid::Uuid;

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
    let (res, mut session, _) = actix_ws::handle(&req, stream)?;

    let client_ch_s = req.app_data::<Sender<Client>>().unwrap().clone();
    let removed_client_ch_s = req.app_data::<Sender<Uuid>>().unwrap().clone();

    let (sender, receiver) = crossbeam_channel::bounded::<String>(1);

    let uuid = Uuid::new_v4();

    let client = Client {
        uuid: uuid,
        ch: sender,
    };

    client_ch_s.send(client).unwrap();

    rt::spawn(async move {
        loop {
            select! {
                recv(receiver) -> message => {
                    if session.text(message.unwrap()).await.is_err() {
                        removed_client_ch_s.send(uuid).unwrap();
                        session.close(None).await;
                        break;
                    }
                }
                default => {
                    tokio::task::yield_now().await;
                }
            }
        }
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
