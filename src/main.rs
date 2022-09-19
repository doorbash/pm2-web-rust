mod pm2;

use actix_files::Files;
use actix_web::{
    error::ErrorInternalServerError, rt, web, App, Error, HttpRequest, HttpResponse, HttpServer,
    Result,
};
use askama::Template;
use std::{
    collections::{HashMap, VecDeque},
    process,
    time::Duration,
};
use tokio::sync::mpsc::{channel, Sender};
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
    stats_ch: Sender<String>,
    logs_ch: Sender<String>,
}

async fn js_handler(_: HttpRequest) -> Result<HttpResponse> {
    match (ScriptTemplate {
        actions_enabled: false,
        time_enabled: true,
        app_id_enabled: false,
        app_name_enabled: true,
    }.render()) {
        Ok(x) => Ok(HttpResponse::Ok().content_type("text/javascript").body(x)),
        Err(_) => Err(ErrorInternalServerError("error in js_handler")),
    }
}

async fn logs_handler(req: HttpRequest, stream: web::Payload) -> Result<HttpResponse, Error> {
    let (res, mut session, _) = actix_ws::handle(&req, stream)?;

    let clients_ch_s = match req.app_data::<Sender<Client>>() {
        Some(x) => x,
        None => return Err(ErrorInternalServerError("err")),
    }.clone();

    let removed_clients_ch_s = match req.app_data::<Sender<Uuid>>() {
        Some(x) => x,
        None => return Err(ErrorInternalServerError("err")),
    }.clone();

    let (stats_ch_s, mut stats_ch_r) = channel::<String>(1);
    let (logs_ch_s, mut logs_ch_r) = channel::<String>(200);

    let uuid = Uuid::new_v4();

    let client = Client {
        uuid: uuid,
        stats_ch: stats_ch_s,
        logs_ch: logs_ch_s,
    };

    rt::spawn(async move {
        if clients_ch_s.send(client).await.is_err() {
            return;
        }

        loop {
            tokio::select! {
                message = stats_ch_r.recv() => {
                    if let Some(x) = message {
                        if session.text(x).await.is_err() {
                            removed_clients_ch_s.send(uuid).await;
                            session.close(None).await;
                            return
                        }
                    }
                },
                message = logs_ch_r.recv() => {
                    if let Some(x) = message {
                        if session.text(x).await.is_err() {
                            removed_clients_ch_s.send(uuid).await;
                            session.close(None).await;
                            return;
                        }
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
    let (logs_ch_s, mut logs_ch_r) = channel::<String>(200);
    let (clients_ch_s, mut clients_ch_r) = channel::<Client>(100);
    let (removed_clients_ch_s, mut removed_clients_ch_r) = channel::<Uuid>(100);

    let (j1, j2) = pm2::PM2::start(stats_ch_s, logs_ch_s, Duration::from_secs(3));

    tokio::task::spawn(async move { if j1.await.is_err() { process::exit(1); } });
    tokio::task::spawn(async move { if j2.await.is_err() { process::exit(1); } });

    tokio::task::spawn(async move {
        if tokio::task::spawn(async move {
            let mut clients: HashMap<Uuid, Client> = HashMap::new();
            let mut stats = String::new();
            let mut logs: VecDeque<String> = VecDeque::with_capacity(10);
            loop {
                tokio::select! {
                    data = stats_ch_r.recv() => {
                        if let Some(x) = data {
                            stats = x;
                            for client in clients.values() {
                                tokio::select! {
                                    _ = client.stats_ch.send(stats.clone()) => {},
                                    else => ()
                                }
                            }
                        }
                    },
                    data = logs_ch_r.recv() => {
                        while logs.len() >= 200 {
                            logs.pop_front();
                        }
                        if let Some(data) = data {
                            let data_clone = data.clone();
                            logs.push_back(data);
                            for client in clients.values() {
                                tokio::select! {
                                    _ = client.logs_ch.send(data_clone.clone()) => (),
                                    else => ()
                                }
                            }
                        }
                    }
                    client = clients_ch_r.recv() => {
                        if let Some(client) = client {
                            println!("client connected: {}", client.uuid);
    
                            if !stats.is_empty() {
                                tokio::select! {
                                    _ = client.stats_ch.send(stats.clone()) => (),
                                    else => ()
                                }
                            }
    
                            for log in logs.iter().cloned() {
                                tokio::select! {
                                    _ = client.logs_ch.send(log) => (),
                                    else => ()
                                }
                            }
    
                            clients.insert(client.uuid, client);
                            println!("num connected clients: {}", clients.len());
                        }
                    }
                    uuid = removed_clients_ch_r.recv() => {
                        if let Some(uuid) = uuid {
                            println!("client disconnected: {}", uuid);
                            clients.remove(&uuid);
                            println!("num connected clients: {}", clients.len());
                        }
                    },
                }
            }
        }).await.is_err() { process::exit(0); }
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
    .workers(2)
    .run()
    .await
}
