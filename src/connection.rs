// Copyright 2026 Tree xie.
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

// Thin forwarding shim: the connection layer lives in the
// `zedis-connection` crate; call sites keep `crate::connection::…`.
pub use zedis_connection::*;

#[cfg(test)]
mod tests {
    /// The name Redis sees (`CLIENT SETNAME`) is built from
    /// `zedis-connection`'s `CARGO_PKG_VERSION`, which only equals the app
    /// version because every member inherits `version` from
    /// `[workspace.package]`. Give that crate a version of its own and Redis
    /// starts seeing *it* — exactly the `zedis:v0.1.0` regression that shipped
    /// when the connection layer was first extracted. This asserts across the
    /// crate boundary: the constant here is the *app*'s version.
    #[test]
    fn client_name_matches_app_version() {
        assert_eq!(
            zedis_connection::client_name(),
            format!("zedis:v{}", env!("CARGO_PKG_VERSION")),
            "zedis-connection must inherit `version` from [workspace.package]"
        );
    }
}
