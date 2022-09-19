mod pm2;

use actix_files::Files;
use actix_web::{App, Error, HttpRequest, HttpResponse, HttpServer, Result, web, rt};
use askama::Template;
use std::{collections::HashMap, process, time::{Duration}};
use uuid::Uuid;
use tokio::sync::mpsc::{Sender, channel};

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
    stats_ch: Sender<String>,
    logs_ch: Sender<String>
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

    let clients_ch_s = req.app_data::<Sender<Client>>().unwrap().clone();
    let removed_clients_ch_s = req.app_data::<Sender<Uuid>>().unwrap().clone();

    let (stats_ch_s, mut stats_ch_r) = channel::<String>(1);
    let (logs_ch_s, mut logs_ch_r) = channel::<String>(10);

    let uuid = Uuid::new_v4();

    let client = Client {
        uuid: uuid,
        stats_ch: stats_ch_s,
        logs_ch: logs_ch_s,
    };

    rt::spawn(async move {
        clients_ch_s.send(client).await;
        loop {
            tokio::select! {
                message = stats_ch_r.recv() => {
                    if session.text(message.unwrap()).await.is_err() {
                        removed_clients_ch_s.send(uuid).await;
                        session.close(None).await;
                        return
                    }
                },
                message = logs_ch_r.recv() => {
                    if session.text(message.unwrap()).await.is_err() {
                        removed_clients_ch_s.send(uuid).await;
                        session.close(None).await;
                        return;
                    }
                }
            }
        }
    });

    Ok(res)
}

#[actix_web::main]
async fn main() -> Result<(), std::io::Error> {
    // pretty_env_logger::init();

    ctrlc::set_handler(move || {
        process::exit(0);
    })
    .expect("Error setting Ctrl-C handler");

    let (stats_ch_s, mut stats_ch_r) = channel::<String>(1);
    let (logs_ch_s, mut logs_ch_r) = channel::<String>(10);
    let (clients_ch_s, mut clients_ch_r) = channel::<Client>(10);
    let (removed_clients_ch_s, mut removed_clients_ch_r) = channel::<Uuid>(10);

    pm2::PM2::start(stats_ch_s, logs_ch_s, Duration::from_secs(3));

    tokio::task::spawn(async move {
        let mut clients: HashMap<Uuid, Client> = HashMap::new();
        loop {
            tokio::select! {
                data = stats_ch_r.recv() => {
                    let data = data.unwrap();
                    for client in clients.values() {
                        tokio::select! {
                            _ = client.stats_ch.send(data.clone()) => {},
                            else => ()
                        }
                    }
                },
               data = logs_ch_r.recv() => {
                    let data = data.unwrap();
                    for client in clients.values() {
                        tokio::select! {
                            _ = client.logs_ch.send(data.clone()) => (),
                            else => ()
                        }
                    }
                },
                uuid = removed_clients_ch_r.recv() => {
                    let uuid = uuid.unwrap();
                    println!("client disconnected: {}", uuid);
                    clients.remove(&uuid);
                },
                client = clients_ch_r.recv() => {
                    let client = client.unwrap();
                    println!("client connected: {}", client.uuid);
                    clients.insert(client.uuid, client);
                }
            }
        }
    });

    HttpServer::new(move || {
        App::new()
            // .wrap(Logger::default())
            .app_data(clients_ch_s.clone())
            .app_data(removed_clients_ch_s.clone())
            .route("/script.js", web::get().to(js_handler))
            .service(web::resource("/logs").route(web::get().to(logs_handler)))
            .service(Files::new("/", "./static/").index_file("index.html"))
    })
    .bind(("0.0.0.0", 6060))?
    .workers(10)
    .run().await
}
