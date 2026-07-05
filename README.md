# Az.Local.Dev

A local, self-contained emulator for Azure resources — currently **Azure Service Bus** and **Azure Storage
(Blob)** — with a system-tray app and a browser dashboard for managing everything, so you can develop and
test against Azure SDKs (and Azure Functions) without a real Azure subscription.

Speaks the real wire protocols, so unmodified apps using the official Azure SDKs (.NET, Java, JS, Python, Go)
can connect to it exactly as they would to real Azure, just by pointing a connection string at `localhost`:
real **AMQP 1.0** for Service Bus, and the real **Blob REST API** (path-style, Azurite-compatible) for Storage.

## Features

- **System tray app** (`emu-engine.exe`) — runs quietly in the background; left-click the tray icon to open
  the dashboard, right-click for a Quit/Open menu.
- **Web dashboard** at [http://127.0.0.1:7777](http://127.0.0.1:7777) — create, rename, start/stop, and
  delete resource groups and resources; browse queues and peek/complete/abandon/dead-letter/resubmit
  messages; global search across resource groups and resources.
- **Azure Service Bus emulation** — queues with active/scheduled/deferred/dead-lettered message states,
  AMQP 1.0 (plain + TLS) endpoints starting at port `5672`, one emulated namespace per resource.
- **Azure Storage (Blob) emulation** — containers and block blobs (upload/download/delete/list, metadata),
  path-style HTTP endpoints starting at port `10000`, one emulated account per resource.
- **Auto-persistence** — every resource group is saved as its own JSON file under
  `%APPDATA%/EmuEngine/groups/{group-id}.json`, kept in sync on every create/rename/delete, and restored
  automatically on the next launch. Queue/message data is stored separately under
  `%APPDATA%/EmuEngine/data/service-bus/{instance-id}.json`.

## Project layout

This is a Cargo workspace:

| Crate | Path | Purpose |
| --- | --- | --- |
| `emu-servicebus-core` | `emu/module/servicebus/core` | Domain model: broker, entities, message states — no I/O. |
| `emu-servicebus-amqp` | `emu/module/servicebus/amqp` | AMQP 1.0 server adapter (`fe2o3-amqp`) over the core broker. |
| `emu-servicebus-engine` | `emu/module/servicebus/engine` | Wires core + AMQP into a runnable Service Bus emulator instance, plus its REST API. |
| `emu-storage-blob-core` | `emu/module/storage/blob/core` | Domain model: containers, blobs — no I/O. |
| `emu-storage-blob-server` | `emu/module/storage/blob/server` | Blob REST API wire protocol adapter over the core store. |
| `emu-storage-blob-engine` | `emu/module/storage/blob/engine` | Wires core + HTTP server into a runnable Storage (Blob) emulator instance, plus its REST API. |
| `emu-registry` | `emu/services/engine` | Generic `EmulatorEngine`/`EngineRegistry` traits shared by every resource kind. |
| `emu-web` | `emu/ui/web` | Dashboard REST API, static asset serving, and per-group persistence. |
| `emu-gui` | `emu/ui/gui` | The tray application binary (`emu-engine`), wiring everything together. |

## Building & running

```powershell
cargo build -p emu-gui
./target/debug/emu-engine.exe
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

### Blob trigger/binding, or as `AzureWebJobsStorage`

```json
{
  "IsEncrypted": false,
  "Values": {
    "FUNCTIONS_WORKER_RUNTIME": "dotnet-isolated",
    "AzureWebJobsStorage": "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey=emulator;BlobEndpoint=http://127.0.0.1:10000/devstoreaccount1;"
  }
}
```

This works as a drop-in `AzureWebJobsStorage` replacement (same convention as Azurite's
`UseDevelopmentStorage=true`) for apps using only Blob input/output bindings and the host's own internal
bookkeeping. It does **not** yet cover Queue- or Table-backed features (Durable Functions task hubs,
blob-trigger polling/retry tracking) - those storage services aren't emulated yet.

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

Both use the same self-signed dev certificate (persisted under `%APPDATA%/EmuEngine/certs`, generated on
first use). The emulator tries to auto-trust it in your Windows user certificate store; if that fails, the
TLS handshake itself will fail cert validation until you trust it manually:

```powershell
certutil -user -addstore Root "$env:APPDATA\EmuEngine\certs\dev-cert.pem"
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
