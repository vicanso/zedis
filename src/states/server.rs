// Copyright 2025 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::connection::RedisServer;
use crate::connection::get_connection_manager;
use crate::connection::{get_servers, save_servers};
use crate::error::Error;
use gpui::AppContext;
use gpui::Context;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, Default)]
pub struct ZedisServerState {
    pub server: String,
    pub dbsize: Option<u64>,
    pub servers: Option<Vec<RedisServer>>,
    pub selected_key: Option<String>,
}

impl ZedisServerState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            ..Default::default()
        }
    }
    pub fn update_or_insrt_server(&mut self, cx: &mut Context<Self>, server: RedisServer) {
        let mut servers = self.servers.clone().unwrap_or_default();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                if let Some(existing_server) = servers.iter_mut().find(|s| s.name == server.name) {
                    *existing_server = server;
                } else {
                    servers.push(server);
                }
                save_servers(&servers)?;

                Ok(servers)
            });
            let result: Result<Vec<RedisServer>> = task.await;
            match result {
                Ok(servers) => handle.update(cx, |this, cx| {
                    this.servers = Some(servers);
                    cx.notify();
                }),
                Err(e) => {
                    // TODO
                    println!("error: {e:?}");
                    Ok(())
                }
            }
        })
        .detach();
    }
    pub fn fetch_servers(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let servers = get_servers()?;
                Ok(servers)
            });
            let result: Result<Vec<RedisServer>> = task.await;
            handle.update(cx, move |this, cx| {
                match result {
                    Ok(servers) => {
                        this.servers = Some(servers);
                    }
                    Err(e) => {
                        println!("error: {e:?}");
                    }
                };
                cx.notify();
            })
        })
        .detach();
    }
    pub fn select_server(&mut self, server: &str, cx: &mut Context<Self>) {
        if self.server != server {
            self.server = server.to_string();
            self.dbsize = None;
            cx.notify();
            if self.server.is_empty() {
                return;
            }
            let server_clone = server.to_string();
            cx.spawn(async move |handle, cx| {
                let counting_server = server_clone.clone();
                let task = cx.background_spawn(async move {
                    let client = get_connection_manager().get_client(&server_clone)?;
                    client.dbsize()
                });
                let result = task.await;
                handle.update(cx, move |this, cx| {
                    if this.server != counting_server {
                        return;
                    }
                    match result {
                        Ok(dbsize) => {
                            this.dbsize = Some(dbsize);
                        }
                        Err(e) => {
                            // TODO 出错的处理
                            println!("error: {e:?}");
                            this.dbsize = None;
                        }
                    };
                    cx.notify();
                })
            })
            .detach();
        }
    }
}
