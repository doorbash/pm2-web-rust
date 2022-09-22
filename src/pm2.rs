use core::time;
use serde::Serialize;
use serde_json::Value;
use std::{io::{BufRead, BufReader}, process::{Command, Stdio}, str, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::{pin, sync::mpsc::{Sender, UnboundedSender}, task::JoinHandle, time::sleep};

pub struct PM2 {}

#[derive(Debug, Serialize)]
struct Stats {
    name: String,
    id: i64,
    pid: i64,
    uptime: i64,
    status: String,
    restart: i64,
    user: String,
    cpu: f64,
    mem: i64,
}

#[derive(Debug, Serialize)]
struct LogsData {
    app: String,
    id: String,
    message: String,
    time: String,
    #[serde(rename = "type")]
    _type: String,
}

#[derive(Serialize)]
struct Message<T> {
    #[serde(rename = "Type")]
    _type: String,
    #[serde(rename = "Data")]
    data: T,
    #[serde(rename = "Time")]
    time: u128,
}

impl PM2 {
    pub fn start(
        stats_chan: Sender<String>,
        logs_chan: UnboundedSender<String>,
        interval: time::Duration,
    ) -> (JoinHandle<()>, JoinHandle<()>) {

        macro_rules! unwrap_or_sleep {
            ($res:expr) => {
                match $res {
                    Ok(val) => val,
                    Err(_) => {
                        tokio::time::sleep(interval).await;
                        continue;
                    }
                }
            };
        }

        return (
            tokio::task::spawn(async move {
                loop {
                    let mut list: Vec<Stats> = vec![];

                    let output = unwrap_or_sleep!(Command::new("pm2").arg("jlist").output());

                    let s = unwrap_or_sleep!(str::from_utf8(&output.stdout));

                    if let Value::Array(arr) = unwrap_or_sleep!(serde_json::from_str(s)) {
                        for obj in arr {
                            let mut stats = Stats {
                                name: String::from(""),
                                id: 0,
                                pid: 0,
                                uptime: 0,
                                status: String::from(""),
                                restart: 0,
                                user: String::from(""),
                                cpu: 0f64,
                                mem: 0,
                            };
                            if let Value::String(name) = obj["name"].clone() {
                                stats.name = name
                            }
                            if let Value::Number(id) = obj["pm_id"].clone() {
                                if let Some(x) = id.as_i64() {
                                    stats.id = x;
                                }
                            }
                            if let Value::Number(pid) = obj["pid"].clone() {
                                if let Some(x) = pid.as_i64() {
                                    stats.pid = x;
                                }
                            }
                            if let Value::Object(pm2_env) = &obj["pm2_env"] {
                                if let Value::Number(uptime) = pm2_env["pm_uptime"].clone() {
                                    if let Some(x) = uptime.as_i64() {
                                        stats.uptime = x;
                                    }
                                }
                                if let Value::String(status) = pm2_env["status"].clone() {
                                    stats.status = status;
                                }
                                if let Value::Number(restart) = pm2_env["restart_time"].clone() {
                                    if let Some(x) = restart.as_i64() {
                                        stats.restart = x;
                                    }
                                }
                                if let Value::String(user) = pm2_env["username"].clone() {
                                    stats.user = user;
                                }
                            }
                            if let Value::Object(monit) = &obj["monit"] {
                                if let Value::Number(cpu) = monit["cpu"].clone() {
                                    if let Some(x) = cpu.as_f64() {
                                        stats.cpu = x;
                                    }
                                }
                                if let Value::Number(mem) = monit["memory"].clone() {
                                    if let Some(x) = mem.as_i64() {
                                        stats.mem = x;
                                    }
                                }
                            }
                            list.push(stats);
                        }
                    }
                    let data = Message {
                        _type: String::from("stats"),
                        data: list,
                        time: unwrap_or_sleep!(SystemTime::now().duration_since(UNIX_EPOCH)).as_millis(),
                    };
                    let j = unwrap_or_sleep!(serde_json::to_string(&data));
                    let sleep = sleep(Duration::from_secs(1));
                    pin!(sleep);
                    tokio::select! {
                        _ = stats_chan.send(j) => (),
                        _ = &mut sleep => ()
                    }
                    tokio::time::sleep(interval).await;
                }
            }),
            tokio::task::spawn_blocking(move || loop {
                if let Ok(child) = Command::new("pm2")
                    .arg("logs")
                    .arg("--format")
                    .arg("--timestamp")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    if let Some(stdout) = child.stdout {
                        let mut br = BufReader::new(stdout);
                        let mut line = String::new();
                        loop {
                            if let Err(err) = br.read_line(&mut line) {
                                println!("error while reading br line {}", err);
                                break;
                            }

                            if None == line.find("timestamp=") {
                                break;
                            }

                            let idx1 = match line.find(' ') {
                                Some(x) => x,
                                None => break,
                            };

                            if !line[idx1+1..].starts_with("app=") {
                                break;
                            }

                            let idx2 =
                                idx1 + match line[idx1 + 1..].find(' ') {
                                    Some(x) => x,
                                    None => break,
                                } + 1;

                            if !line[idx2+1..].starts_with("id=") {
                                break;
                            }

                            let idx3 =
                                idx2 + match line[idx2 + 1..].find(' ') {
                                    Some(x) => x,
                                    None => break,
                                } + 1;

                            if !line[idx3+1..].starts_with("type=") {
                                break;
                            }

                            let idx4 =
                                idx3 + match line[idx3 + 1..].find(' ') {
                                    Some(x) => x,
                                    None => break,
                                } + 1;

                            if !line[idx4+1..].starts_with("message=") {
                                break;
                            }

                            let data = Message {
                                _type: String::from("log"),
                                data: LogsData {
                                    app: line[idx1 + 5..idx2].to_string(),
                                    id: line[idx2 + 4..idx3].to_string(),
                                    message: line[idx4 + 9..].to_string(),
                                    time: format!("{} {}", &line[10..20], &line[21..idx1]),
                                    _type: line[idx3 + 6..idx4].to_string(),
                                },
                                time: match SystemTime::now().duration_since(UNIX_EPOCH) {
                                    Ok(x) => x,
                                    Err(_) => break
                                }.as_millis(),
                            };
                            if let Ok(x) = serde_json::to_string(&data) {
                                logs_chan.send(x);
                            }
                            line.clear();
                        }
                    }
                }
            }),
        );
    }
}
