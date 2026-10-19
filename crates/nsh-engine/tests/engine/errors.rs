//! Errors are reported with app source locations, via the source map.

use std::{
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use nsh_core::{ApproxTextEngine, MountBatch, Size};
use nsh_engine::{AppError, AppErrorKind, Bundle, Engine, EngineConfig, MountSink};

struct Sink {
    errors: Arc<Mutex<mpsc::Sender<String>>>,
}

impl MountSink for Sink {
    fn submit(&self, _batch: MountBatch) {}

    fn report_error(&self, message: &str) {
        let _ = self.errors.lock().unwrap().send(message.to_owned());
    }
}

/// Hears the errors with their parts.
struct ReportSink(Mutex<mpsc::Sender<AppError>>);

impl MountSink for ReportSink {
    fn submit(&self, _batch: MountBatch) {}

    fn report(&self, error: &AppError) {
        let _ = self.0.lock().unwrap().send(error.clone());
    }
}

fn config(bundle: Bundle, limits: nsh_engine::Limits) -> EngineConfig {
    EngineConfig {
        app: nsh_engine::App::Js(bundle),
        viewport: Size::new(100.0, 100.0),
        trace: false,
        network: None,
        assets: None,
        environment: Default::default(),
        header_heights: (44.0, 96.0),
        limits,
        ui_sets: Vec::new(),
        runtime: None,
    }
}

/// The errors `code` (JS source) reports, until none came for a while.
fn reports(code: &str, limits: nsh_engine::Limits) -> Vec<AppError> {
    let (tx, rx) = mpsc::channel();
    let bundle = Bundle::source(code.to_owned());
    let _engine = Engine::start(
        config(bundle, limits),
        || ApproxTextEngine,
        ReportSink(Mutex::new(tx)),
    )
    .unwrap();
    let mut errors = Vec::new();
    while let Ok(error) = rx.recv_timeout(Duration::from_millis(1500)) {
        errors.push(error);
    }
    errors
}

#[test]
fn uncaught_errors_point_at_app_sources() {
    let dir = std::env::temp_dir().join(format!("nsh-errors-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("App.tsx");
    // The throw is on line 5, column 9.
    std::fs::write(
        &entry,
        "import { mount, View } from \"nsh\";\n\
         const message: string = \"boom\";\n\
         function fail(): void {\n\
         \x20 // line 4\n\
         \x20 throw new Error(message);\n\
         }\n\
         setTimeout(fail, 0);\n\
         mount(() => <View />);\n",
    )
    .unwrap();
    let bundle = nsh_bundler::bundle(&nsh_bundler::BundleOptions {
        entry,
        minify: false,
        split: false,
        ui_sets: vec![nsh_ui_mobile::registry()],
    })
    .unwrap();
    let bytecode = nsh_js::compile_bytecode(&bundle.code, false).unwrap();
    // SAFETY: compiled just now by this runtime.
    let bundle = unsafe { Bundle::bytecode(bytecode) }
        .with_source_map(bundle.source_map.expect("source map"));

    let (tx, rx) = mpsc::channel();
    let sink = Sink {
        errors: Arc::new(Mutex::new(tx)),
    };
    let config = EngineConfig {
        app: nsh_engine::App::Js(bundle),
        viewport: Size::new(100.0, 100.0),
        trace: false,
        network: None,
        assets: None,
        environment: Default::default(),
        header_heights: (44.0, 96.0),
        limits: nsh_engine::Limits::default(),
        ui_sets: Vec::new(),
        runtime: None,
    };
    let _engine = Engine::start(config, || ApproxTextEngine, sink).unwrap();
    let error = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("error reported");
    assert!(error.contains("Error: boom"), "{error}");
    assert!(error.contains("App.tsx:5:"), "not symbolicated: {error}");
    assert!(!error.contains("bundle.js"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn errors_say_what_went_wrong_with_message_and_stack_apart() {
    let errors = reports(
        "function fail() { throw new Error('later'); }\nsetTimeout(fail, 0);",
        nsh_engine::Limits::default(),
    );
    let [error] = errors.as_slice() else {
        panic!("{errors:?}")
    };
    assert_eq!(error.kind, AppErrorKind::Uncaught);
    assert!(!error.is_fatal() && !error.is_stopped());
    assert_eq!(error.message, "Error: later");
    let stack = error.stack.as_deref().expect("a stack");
    assert!(stack.trim_start().starts_with("at fail"), "{stack}");
    assert_eq!(error.package, None);

    let errors = reports("throw new Error('at load');", nsh_engine::Limits::default());
    let [error] = errors.as_slice() else {
        panic!("{errors:?}")
    };
    assert_eq!(error.kind, AppErrorKind::Load);
    assert!(error.is_fatal() && !error.is_stopped());
    assert_eq!(error.message, "Error: at load");
}

#[test]
fn a_stopped_vm_is_reported_as_fatal() {
    let limits = nsh_engine::Limits {
        memory_bytes: 16 << 20,
        ..nsh_engine::Limits::default()
    };
    let errors = reports(
        "const kept = []; setTimeout(() => { for (;;) kept.push(new Array(100000).fill(1)); }, 0);",
        limits,
    );
    let stopped: Vec<_> = errors.iter().filter(|e| e.is_stopped()).collect();
    assert_eq!(stopped.len(), 1, "{errors:?}");
    assert!(stopped[0].is_fatal());
}
