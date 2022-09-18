use core::time;
use crossbeam_channel::{after, select, Sender};
use serde::Serialize;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::ops::Index;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{str, thread};

pub struct PM2 {}

#[derive(Debug, Serialize)]
pub struct Stats {
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
    Type: String,
}

#[derive(Serialize)]
struct Message<T> {
    Type: String,
    Data: T,
    Time: u128,
}

impl PM2 {
    pub fn start(stats_chan: Sender<String>, logs_chan: Sender<String>, interval: time::Duration) {
        thread::spawn(move || {
            loop {
                let mut list: Vec<Stats> = vec![];
                let output: Output = Command::new("pm2")
                    .arg("jlist")
                    .output()
                    .expect("failed to execute process");

                if let Value::Array(arr) =
                    serde_json::from_str(str::from_utf8(&output.stdout).unwrap()).unwrap()
                {
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
                            // println!("name = {}", name);
                            stats.name = name
                        }
                        if let Value::Number(id) = obj["pm_id"].clone() {
                            // println!("id = {}", id);
                            stats.id = id.as_i64().unwrap();
                        }
                        if let Value::Number(pid) = obj["pid"].clone() {
                            // println!("pid = {}", pid);
                            stats.pid = pid.as_i64().unwrap();
                        }
                        if let Value::Object(pm2_env) = &obj["pm2_env"] {
                            if let Value::Number(uptime) = pm2_env["pm_uptime"].clone() {
                                // println!("uptime = {}", uptime);
                                stats.uptime = uptime.as_i64().unwrap()
                            }
                            if let Value::String(status) = pm2_env["status"].clone() {
                                // println!("status = {}", status);
                                stats.status = status;
                            }
                            if let Value::Number(restart) = pm2_env["restart_time"].clone() {
                                // println!("restart = {}", restart);
                                stats.restart = restart.as_i64().unwrap();
                            }
                            if let Value::String(user) = pm2_env["username"].clone() {
                                // println!("user = {}", user);
                                stats.user = user;
                            }
                        }
                        if let Value::Object(monit) = &obj["monit"] {
                            if let Value::Number(cpu) = monit["cpu"].clone() {
                                // println!("cpu = {}", cpu);
                                stats.cpu = cpu.as_f64().unwrap();
                            }
                            if let Value::Number(mem) = monit["memory"].clone() {
                                // println!("mem = {}", mem);
                                stats.mem = mem.as_i64().unwrap();
                            }
                        }
                        // println!("{:?}", stats);
                        list.push(stats);
                    }
                }
                // println!("trying to send...");
                let data = Message {
                    Type: String::from("stats"),
                    Data: list,
                    Time: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                };
                select! {
                    send(stats_chan, serde_json::to_string(&data).unwrap()) -> _ => {}
                    recv(after(time::Duration::from_secs(3))) -> _ => {}
                    // default => {}
                }
                thread::sleep(interval);
            }
        });

        thread::spawn(move || {
            let child = Command::new("pm2")
                .arg("logs")
                .arg("--format")
                .arg("--timestamp")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut br = BufReader::new(child.stdout.unwrap());
            loop {
                let mut line = String::new();
                br.read_line(&mut line).unwrap();

                if None == line.find("timestamp=") {
                    continue;
                }

                let idx1 = line.find(' ').unwrap();
                let idx2 = idx1 + line[idx1+1..].find(' ').unwrap() + 1;
                let idx3 = idx2 + line[idx2+1..].find(' ').unwrap() + 1;
                let idx4 = idx3 + line[idx3+1..].find(' ').unwrap() + 1;

                let data = Message {
                    Type: String::from("log"),
                    Data: LogsData {
                        app: line[idx1+5..idx2].to_string(),
                        id: line[idx2+4..idx3].to_string(),
                        message: line[idx4+9..].to_string(),
                        time: format!("{} {}", &line[10..20], &line[21..idx1]),
                        Type: line[idx3+6..idx4].to_string(),
                    },
                    Time: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                };
                logs_chan
                    .send(serde_json::to_string(&data).unwrap())
                    .unwrap()
            }
        });
    }
}
