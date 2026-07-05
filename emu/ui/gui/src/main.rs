// This is a tray/background app with no console UI - without this, Rust's default "console"
// subsystem on Windows would pop up a visible console window every time `AzLocalDev.exe` runs.
#![windows_subsystem = "windows"]

use std::sync::Arc;
use std::time::{Duration, Instant};

use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};

use emu_registry::{EmulatorEngine, EngineRegistry};
use emu_appinsights_engine::{AppInsightsEngine, AppInsightsRegistry};
use emu_servicebus_engine::{ServiceBusEngine, ServiceBusRegistry};
use emu_storage_blob_engine::{StorageEngine, StorageRegistry};
use emu_web::AppState;

mod dev_cert_prompt;
mod icon;

const APP_NAME: &str = "AzLocalDev";
const DASHBOARD_ADDR: &str = "127.0.0.1:7777";
const SERVICE_BUS_AMQP_PORT: u16 = 5672;
const STORAGE_BLOB_BASE_PORT: u16 = 10000;
const APP_INSIGHTS_BASE_PORT: u16 = 9500;

fn tray_icon() -> tray_icon::Icon {
    let (rgba, size) = icon::render_rgba(32);
    tray_icon::Icon::from_rgba(rgba, size, size).expect("valid icon buffer")
}

fn dashboard_url() -> String {
    format!("http://{DASHBOARD_ADDR}/")
}

/// Opens the dashboard in the user's default browser. Since it's just a normal web page,
/// nothing stops the user from opening it in as many tabs/windows as they like.
fn open_dashboard() {
    if let Err(err) = open::that(dashboard_url()) {
        tracing::warn!(?err, "failed to open the dashboard in a browser");
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Loads (generating on first run) the shared dev TLS certificate used by every
    // Managed-Identity-capable listener (Service Bus AMQPS, Storage Blob HTTPS), and asks
    // the user once whether to trust it, instead of each engine silently attempting (and
    // possibly silently failing) this on its own later. Each engine's own `start()` still
    // calls `load_or_generate()` itself - that just re-reads these same persisted files.
    match emu_dev_cert::load_or_generate() {
        Ok(dev_cert) => dev_cert_prompt::ensure_trusted(&dev_cert),
        Err(err) => tracing::warn!(?err, "failed to prepare dev TLS certificate"),
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let registry = EngineRegistry::new();
    let sb_registry = ServiceBusRegistry::new();
    let blob_registry = StorageRegistry::new();
    let ai_registry = AppInsightsRegistry::new();

    // Shared with the "service-bus" kind factory below so both it (when creating a brand
    // new instance) and the post-restore bump logic further down (when instances were
    // recreated from a persisted resource group instead) agree on the next free port/seq.
    let next_port = Arc::new(std::sync::atomic::AtomicU16::new(SERVICE_BUS_AMQP_PORT + 1));
    let next_seq = Arc::new(std::sync::atomic::AtomicU8::new(2));
    // Same idea, for the "storage" kind's base (Blob) HTTP port - each instance reserves 3
    // consecutive ports (Blob/Queue/Table).
    let next_blob_port = Arc::new(std::sync::atomic::AtomicU16::new(STORAGE_BLOB_BASE_PORT));
    // Same idea, for the "app-insights" kind's ingestion HTTP port.
    let next_ai_port = Arc::new(std::sync::atomic::AtomicU16::new(APP_INSIGHTS_BASE_PORT));

    registry.register_kind("service-bus", "Service Bus", {
        // Each instance gets its own `127.0.0.{seq}` loopback address for its AMQPS listener
        // (see `ServiceBusEngine::new`) - that's what lets a Managed-Identity-style
        // `fullyQualifiedNamespace` address a *specific* instance with zero code changes and
        // no custom port, since real Azure SDK clients always dial the default AMQPS port
        // (5671) for a bare hostname. Every 127.0.0.x address is loopback, so no hosts file or
        // DNS setup is needed.
        let next_port = next_port.clone();
        let next_seq = next_seq.clone();
        let sb_registry = sb_registry.clone();
        move |id, name, config| {
            let port = config
                .as_ref()
                .and_then(|c| c.get("port"))
                .and_then(|p| p.as_u64())
                .map(|p| p as u16)
                .unwrap_or_else(|| next_port.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
            let seq = config
                .as_ref()
                .and_then(|c| c.get("seq"))
                .and_then(|p| p.as_u64())
                .map(|p| p as u8)
                .unwrap_or_else(|| next_seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
            let engine = Arc::new(ServiceBusEngine::new(id, name, port, seq));
            sb_registry.insert(engine.clone());
            engine as Arc<dyn EmulatorEngine>
        }
    });

    registry.register_kind("storage", "Storage (Blob + Queue + Table)", {
        let next_blob_port = next_blob_port.clone();
        let blob_registry = blob_registry.clone();
        move |id, name, config| {
            let port = config
                .as_ref()
                .and_then(|c| c.get("port"))
                .and_then(|p| p.as_u64())
                .map(|p| p as u16)
                // Each instance reserves 3 consecutive ports (Blob/Queue/Table, matching
                // Azurite's convention), so the counter must advance by 3 per instance, not 1.
                .unwrap_or_else(|| next_blob_port.fetch_add(3, std::sync::atomic::Ordering::SeqCst));
            let engine = Arc::new(StorageEngine::new(id, name, port));
            blob_registry.insert(engine.clone());
            engine as Arc<dyn EmulatorEngine>
        }
    });

    registry.register_kind("app-insights", "Application Insights", {
        let next_ai_port = next_ai_port.clone();
        let ai_registry = ai_registry.clone();
        move |id, name, config| {
            let port = config
                .as_ref()
                .and_then(|c| c.get("port"))
                .and_then(|p| p.as_u64())
                .map(|p| p as u16)
                .unwrap_or_else(|| next_ai_port.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
            let instrumentation_key = config
                .as_ref()
                .and_then(|c| c.get("instrumentation_key"))
                .and_then(|k| k.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let engine = Arc::new(AppInsightsEngine::new(id, name, port, instrumentation_key));
            ai_registry.insert(engine.clone());
            engine as Arc<dyn EmulatorEngine>
        }
    });

    // Every resource group is persisted as its own `%APPDATA%/AzLocalDev/groups/{id}.json`
    // file (see `emu_web::persist_group`/`load_all_groups`), rewritten on every rename/
    // create/delete - so this restores exactly what was there last time, names and all,
    // instead of always starting over with a single hardcoded "Service Bus" instance.
    let restored = runtime.block_on(emu_web::load_all_groups(&registry));
    if restored {
        // Bump past whatever ports/instance-seqs were just restored, so a resource created
        // afterward in this run can't collide with one of them.
        for engine in sb_registry.all() {
            next_port.fetch_max(engine.amqp_port() + 1, std::sync::atomic::Ordering::SeqCst);
            let seq = engine.amqps_host().octets()[3];
            next_seq.fetch_max(seq.saturating_add(1), std::sync::atomic::Ordering::SeqCst);
        }
        for engine in blob_registry.all() {
            next_blob_port.fetch_max(engine.port() + 3, std::sync::atomic::Ordering::SeqCst);
        }
        for engine in ai_registry.all() {
            next_ai_port.fetch_max(engine.port() + 1, std::sync::atomic::Ordering::SeqCst);
        }
    } else {
        // First-ever run (nothing persisted yet): create the same default this app has
        // always started with, and persist it immediately so it shows up on disk right away
        // rather than only after the user's first edit.
        let service_bus = Arc::new(ServiceBusEngine::new(
            "service-bus-1",
            "Service Bus",
            SERVICE_BUS_AMQP_PORT,
            1,
        ));
        let default_group = registry.create_group("Default", None);
        registry.register(service_bus.clone(), &default_group.id);
        sb_registry.insert(service_bus.clone());
        runtime.block_on(async {
            if let Err(err) = service_bus.start().await {
                tracing::error!(?err, "failed to autostart Service Bus emulator");
            }
        });
        emu_web::persist_group(&registry, &default_group.id);
    }

    let state = AppState {
        registry: registry.clone(),
    };

    let addr = DASHBOARD_ADDR.parse()?;
    let router = emu_web::with_static_fallback(
        emu_web::dashboard_router(state)
            .nest("/api/service-bus", emu_servicebus_engine::router(sb_registry))
            .nest("/api/storage-blob", emu_storage_blob_engine::router(blob_registry))
            .nest("/api/app-insights", emu_appinsights_engine::router(ai_registry)),
    );
    runtime.spawn(async move {
        if let Err(err) = emu_web::serve(addr, router).await {
            tracing::error!(?err, "dashboard web server crashed");
        }
    });

    // A Win32 message loop needs to be running for the tray icon to work - `tao`'s event
    // loop provides that, even though we never create a visible window.
    let event_loop = EventLoop::new();

    let tray_menu = Menu::new();
    let open_item = MenuItem::new("Open Dashboard", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    tray_menu.append(&open_item)?;
    tray_menu.append(&PredefinedMenuItem::separator())?;
    tray_menu.append(&quit_item)?;

    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip(APP_NAME)
        .with_icon(tray_icon())
        .build()?;

    // Open the dashboard once on startup so the user immediately sees something.
    open_dashboard();

    let quit_id = quit_item.id().clone();
    let open_id = open_item.id().clone();

    event_loop.run(move |_event, _target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(150));

        if let Ok(menu_event) = MenuEvent::receiver().try_recv() {
            if menu_event.id == quit_id {
                runtime.block_on(registry.stop_all());
                *control_flow = ControlFlow::Exit;
            } else if menu_event.id == open_id {
                open_dashboard();
            }
        }

        if let Ok(TrayIconEvent::Click { button, button_state, .. }) = TrayIconEvent::receiver().try_recv() {
            // Only react to a left-click release; a right-click should just show the native
            // context menu (Windows already does this automatically) and not also open the
            // dashboard behind it.
            if button == tray_icon::MouseButton::Left && button_state == tray_icon::MouseButtonState::Up {
                open_dashboard();
            }
        }
    });
}
