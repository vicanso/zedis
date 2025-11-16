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
use crate::connection::get_servers;
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
    pub fn select_server(&mut self, server: String, cx: &mut Context<Self>) {
        if self.server != server {
            let server_clone = server.clone();
            self.server = server;
            self.dbsize = None;
            cx.notify();
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
