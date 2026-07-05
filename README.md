# Az.Local.Dev

A local, self-contained emulator for Azure resources — currently **Azure Service Bus**, **Azure Storage**
(Blob + Queue + Table), and **Application Insights** — with a system-tray app and a browser dashboard for
managing everything, so you can develop and test against Azure SDKs (and Azure Functions) without a real
Azure subscription.

Speaks the real wire protocols, so unmodified apps using the official Azure SDKs (.NET, Java, JS, Python, Go)
can connect to it exactly as they would to real Azure, just by pointing a connection string at `localhost`:
real **AMQP 1.0** for Service Bus, and the real **Blob/Queue/Table REST APIs** (path-style,
Azurite-compatible) for Storage.

## Features

- **System tray app** (`AzLocalDev.exe`) — runs quietly in the background; left-click the tray icon to open
  the dashboard, right-click for a Quit/Open menu.
- **Web dashboard** at [http://127.0.0.1:7777](http://127.0.0.1:7777) — create, rename, start/stop, and
  delete resource groups and resources; browse queues and peek/complete/abandon/dead-letter/resubmit
  messages; browse Storage containers/blobs, queues/messages, and tables/entities; global search across
  resource groups and resources.
- **Azure Service Bus emulation** — queues with active/scheduled/deferred/dead-lettered message states,
  AMQP 1.0 (plain + TLS) endpoints starting at port `5672`, one emulated namespace per resource.
- **Azure Storage emulation** — a single "Storage" resource covers all three core data services, exactly
  like a real Storage account or Azurite instance: **Blob** (containers/block blobs - upload/download/
  delete/list, metadata), **Queue** (create/delete queues, put/get/peek/delete/update messages), and
  **Table** (create/delete tables, insert/upsert/update/merge/delete entities, query by PartitionKey).
  Path-style HTTP endpoints on 3 sequential ports starting at `10000` (Blob/Queue/Table), matching
  Azurite's own port convention.
- **Application Insights emulation** — captures telemetry via both the classic Breeze ingestion protocol
  (`APPLICATIONINSIGHTS_CONNECTION_STRING`) and OTLP/HTTP (JSON), with an Aspire-dashboard-style Traces/
  Structured Logs/Metrics viewing experience.
- **Auto-persistence** — every resource group is saved as its own JSON file under
  `%APPDATA%/AzLocalDev/groups/{group-id}.json`, kept in sync on every create/rename/delete, and restored
  automatically on the next launch. Service Bus queue/message data and Storage Blob container/blob data are
  each persisted separately under `%APPDATA%/AzLocalDev/data/{service-bus,storage-blob}/{instance-id}.json`.
  Storage Queue/Table contents and Application Insights telemetry are intentionally **not** persisted - see
  the relevant crates' doc comments for why.

## Project layout

This is a Cargo workspace:

| Crate | Path | Purpose |
| --- | --- | --- |
| `emu-servicebus-core` | `emu/module/servicebus/core` | Domain model: broker, entities, message states — no I/O. |
| `emu-servicebus-amqp` | `emu/module/servicebus/amqp` | AMQP 1.0 server adapter (`fe2o3-amqp`) over the core broker. |
| `emu-servicebus-engine` | `emu/module/servicebus/engine` | Wires core + AMQP into a runnable Service Bus emulator instance, plus its REST API. |
| `emu-storage-blob-core` | `emu/module/storage/blob/core` | Domain model: containers, blobs — no I/O. |
| `emu-storage-blob-server` | `emu/module/storage/blob/server` | Blob REST API wire protocol adapter over the core store. |
| `emu-storage-queue-core` | `emu/module/storage/queue/core` | Domain model: queues, messages (visibility timeout, pop receipts) — no I/O. |
| `emu-storage-queue-server` | `emu/module/storage/queue/server` | Queue REST API wire protocol adapter over the core store. |
| `emu-storage-table-core` | `emu/module/storage/table/core` | Domain model: tables, entities — no I/O. |
| `emu-storage-table-server` | `emu/module/storage/table/server` | Table REST API (OData JSON) wire protocol adapter over the core store. |
| `emu-storage-blob-engine` | `emu/module/storage/blob/engine` | The unified `StorageEngine`: wires the Blob/Queue/Table cores + their HTTP servers into one runnable Storage account instance (3 ports), plus its REST API. Crate name kept for compatibility even though it now covers all three services. |
| `emu-registry` | `emu/services/engine` | Generic `EmulatorEngine`/`EngineRegistry` traits shared by every resource kind. |
| `emu-web` | `emu/ui/web` | Dashboard REST API, static asset serving, and per-group persistence. |
| `emu-gui` | `emu/ui/gui` | The tray application binary (`AzLocalDev`), wiring everything together. |

## Building & running

```powershell
cargo build -p emu-gui
./target/debug/AzLocalDev.exe
```

The dashboard opens automatically on first launch; afterwards, use the tray icon to reopen it or quit.

## Using with Azure Functions

Create a **Service Bus** and/or **Storage (Blob)** resource in the dashboard, then open its details (the info
icon on its row) to copy its connection string. Both resource kinds accept any credentials - there's no real
auth to configure.

### Service Bus trigger/binding

```json
{
  "IsEncrypted": false,
  "Values": {
    "FUNCTIONS_WORKER_RUNTIME": "dotnet-isolated",
    "ServiceBusConnection": "Endpoint=sb://localhost:5672;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=emulator;UseDevelopmentEmulator=true"
  }
}
```

Use `ServiceBusConnection` (or whatever name you pick) as the `connection` property on your trigger/binding.

### Blob/Queue/Table trigger/binding, or as `AzureWebJobsStorage`

```json
{
  "IsEncrypted": false,
  "Values": {
    "FUNCTIONS_WORKER_RUNTIME": "dotnet-isolated",
    "AzureWebJobsStorage": "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey=emulator;BlobEndpoint=http://127.0.0.1:10000/devstoreaccount1;QueueEndpoint=http://127.0.0.1:10001/devstoreaccount1;TableEndpoint=http://127.0.0.1:10002/devstoreaccount1;"
  }
}
```

This works as a drop-in `AzureWebJobsStorage` replacement (same convention as Azurite's
`UseDevelopmentStorage=true`) - a single "Storage" resource in the dashboard emulates all three core data
services (Blob/Queue/Table) on 3 sequential ports, so this covers Blob input/output bindings, queue
triggers/bindings, Table bindings, and the Functions host's own internal bookkeeping (which needs a real
Queue endpoint for its singleton-lease/timer-schedule storage - a Blob-only connection string used to make
the host log `azure.functions.webjobs.storage: Unhealthy - Unable to create client for AzureWebJobsStorage`
even for apps that never touched a queue directly). Durable Functions task hubs (which need both Queue and
Table) should work for common flows, though this isn't a full implementation of every Table Storage query
capability - see `emu-storage-table-core`'s doc comment for the one query shape ($filter) it supports.

### Managed Identity-style connections

Real Managed Identity requires a live Microsoft Entra ID tenant on both ends, so it can't be emulated
locally - this is also why Azurite never supported it. If your app constructs clients from a
`TokenCredential` (e.g. `DefaultAzureCredential`) instead of a connection string, it still works against
this emulator: neither the Service Bus AMQPS listener nor the Blob HTTPS listener validate the token
they're handed, so whatever credential your machine resolves locally (`az login`, Visual Studio, VS Code)
is accepted as-is.

The dashboard's details modal shows the right endpoint to use - `fullyQualifiedNamespace` for Service Bus,
`blobServiceUri` for Blob. **Both must use HTTPS/AMQPS, not the plain connection-string port** - Azure
Core's bearer-token auth policy refuses outright to send a token over an unencrypted connection, so each
emulated resource runs a second, dedicated TLS listener just for this:

- Service Bus: AMQPS on port `5671` (fixed, one loopback address per instance - e.g. `127.0.0.2`).
- Storage (Blob): HTTPS on `<http port> + 10000` (e.g. HTTP `10000` → HTTPS `20000`).

Both use the same self-signed dev certificate (persisted under `%APPDATA%/AzLocalDev/certs`, generated on
first use). The emulator tries to auto-trust it in your Windows user certificate store; if that fails, the
TLS handshake itself will fail cert validation until you trust it manually:

```powershell
certutil -user -addstore Root "$env:APPDATA\AzLocalDev\certs\dev-cert.pem"
```

For identity-based `AzureWebJobsStorage`, set `AzureWebJobsStorage__blobServiceUri` to the `https://` value
shown in the details modal (not the plain `http://` one), and leave `AzureWebJobsStorage__credential`
unset locally so the host falls back to your logged-in developer identity. The `connection` group prefix
(`ServiceBus` below) can be any name you like - it's just what your trigger/binding's `connection` property
must match.

```json
{
  "IsEncrypted": false,
  "Values": {
    "FUNCTIONS_WORKER_RUNTIME": "dotnet-isolated",
    "AzureWebJobsStorage__blobServiceUri": "https://127.0.0.1:20000/devstoreaccount1",
    "ServiceBus__fullyQualifiedNamespace": "127.0.0.1"
  }
}
```

Note both values are the `https://`/AMQPS ones (port `20000`, not `10000`; no `sb://` scheme needed for
`fullyQualifiedNamespace`) - `AzureWebJobsStorage__credential`/`ServiceBus__credential` are deliberately
left unset here since that's what tells the host to use your local developer identity instead of expecting
real Managed Identity (only set them to `managedidentity` when actually deployed to Azure).

## Status

This is a development/test emulator, not a clone of the full Azure control plane — see
[docs/DESIGN.md](docs/DESIGN.md) for the detailed design, goals, and non-goals, and
[docs/SESSION-NOTES.md](docs/SESSION-NOTES.md) for a chronological log of how it got built.
