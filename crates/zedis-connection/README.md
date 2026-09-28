# zedis-connection

The Redis layer of [Zedis](https://github.com/vicanso/zedis): pooled clients, cluster/sentinel handling, server configuration, SSH tunnels, and every Redis operation the app sends — one typed module per feature or key type, each taking a `ServerDb` rather than a connection and answering a struct the UI can draw. Also here: the version floors for Redis and Valkey, the feature probe, the danger classifier and the read-only allowlist the web bridge enforces, the audit redaction, and the shared connection-domain error type. GUI-free.

Part of the Zedis workspace — see the [repository root](https://github.com/vicanso/zedis) for build and contribution docs.
