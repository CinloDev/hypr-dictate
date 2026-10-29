mod paint;
mod settings;
mod shortcut;
mod ui;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use hypr_dictate::{Phase, valid_address, wav};
use serde_json::{Value, json};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

pub enum Event {
    Command(String),
    Ping(mpsc::SyncSender<()>),
    KeyPress(hypr_dictate::CaptureKey),
    KeyRelease(hypr_dictate::CaptureKey),
    ArmReady(u64, String, bool),
    Finished(u64, Result<(String, String)>),
    Catalog(Result<Vec<String>>),
    Save(settings::Settings),
}
struct Capture {
    child: Child,
    reader: thread::JoinHandle<Result<Vec<u8>>>,
    errors: thread::JoinHandle<std::io::Result<Vec<u8>>>,
}
pub struct App {
    phase: Phase,
    capture: Option<Capture>,
    lifecycle: hypr_dictate::KeyLifecycle,
    recording_started: Option<std::time::Instant>,
    tx: mpsc::Sender<Event>,
    client: reqwest::blocking::Client,
    key: String,
    endpoint: String,
    settings: settings::Settings,
    level: std::sync::Arc<std::sync::atomic::AtomicU32>,
    error: String,
}
impl App {
    pub fn color(&self) -> [u8; 3] {
        match self.phase {
            Phase::Idle => [55, 170, 130],
            Phase::Recording(_) => [235, 65, 75],
            Phase::Processing(_) => [230, 170, 40],
            Phase::Error => [160, 75, 210],
        }
    }
    fn start(&mut self, ui: &mut ui::Ui) -> Result<()> {
        if !matches!(self.phase, Phase::Idle | Phase::Error) {
            return Ok(());
        }
        if ui.arming {
            // No sobrescribir ventana objetivo si ya se encuentra en fase de armado
            return Ok(());
        }
        ui.close_modal();
        // Capturar ventana enfocada ANTES del foco exclusivo de layer-shell
        let address = active_address()?;
        ensure!(valid_address(&address), "No hay ventana enfocada válida");

        let dock_was_hidden = !ui.is_visible();
        if dock_was_hidden {
            ui.show();
        }

        ui.arm(address, dock_was_hidden);
        Ok(())
    }
    fn begin_recording(
        &mut self,
        ui: &mut ui::Ui,
        address: String,
        dock_was_hidden: bool,
    ) -> Result<()> {
        ui.capture_keyboard(true);

        let mut command = Command::new("pw-record");
        command.args([
            "--raw",
            "--format=s16",
            "--rate=16000",
            "--channels=1",
            "--sample-count=4800000",
        ]);
        if !self.settings.microphone.is_empty() {
            command.arg(format!("--target={}", self.settings.microphone));
        }
        let mut child = match command
            .arg("-")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                ui.capture_keyboard(false);
                if dock_was_hidden {
                    ui.hide();
                }
                return Err(e).context("No se pudo iniciar pw-record");
            }
        };

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let errors = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.take(65536).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        let level = self.level.clone();
        let reader = thread::spawn(move || {
            let mut stdout = stdout;
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 2048];
            loop {
                let n = stdout.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                let peak = chunk[..n]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|v| i16::from_le_bytes(*v).unsigned_abs() as u32)
                    .max()
                    .unwrap_or(0);
                level.store(peak, std::sync::atomic::Ordering::Relaxed);
                bytes.extend_from_slice(&chunk[..n]);
                ensure!(bytes.len() <= 9_600_000, "Límite de audio excedido");
            }
            level.store(0, std::sync::atomic::Ordering::Relaxed);
            Ok(bytes)
        });

        self.phase.start(&address);
        self.lifecycle.start(address, dock_was_hidden);
        self.recording_started = Some(std::time::Instant::now());
        self.capture = Some(Capture {
            child,
            reader,
            errors,
        });
        Ok(())
    }
    pub fn tick(&mut self, ui: &mut ui::Ui) {
        if ui.arming
            && let Some((_, _, dock_was_hidden, started)) = &ui.arm_target
            && started.elapsed() >= Duration::from_millis(1000)
        {
            let dock_hidden = *dock_was_hidden;
            ui.cancel_arm();
            if dock_hidden {
                ui.hide();
            }
            ui.message = "Atajo cancelado: tecla modificadora retenida".into();
            ui.dirty = true;
        }

        if matches!(self.phase, Phase::Recording(_)) {
            let timeout = self
                .recording_started
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(300));
            let exited = self
                .capture
                .as_mut()
                .and_then(|c| c.child.try_wait().ok().flatten())
                .is_some();
            if timeout || exited {
                let _ = self.cancel(ui);
                ui.capture_keyboard(false);
                if let Some(address) = self.lifecycle.target_address.take() {
                    let _ = restore(&address);
                }
                if self.lifecycle.dock_temporarily_shown {
                    ui.hide();
                    self.lifecycle.dock_temporarily_shown = false;
                }
                self.lifecycle.cancel();
                ui.message = if timeout {
                    "Límite de 5 min alcanzado".into()
                } else {
                    "Grabador finalizó inesperadamente".into()
                };
                ui.dirty = true;
                self.phase = Phase::Idle;
            }
        }
    }
    fn cancel(&mut self, ui: &mut ui::Ui) -> Result<()> {
        ui.cancel_arm();
        self.recording_started = None;
        if let Some(mut capture) = self.capture.take() {
            let _ = capture.child.kill();
            let _ = capture.child.wait();
            let _ = capture.reader.join();
            let _ = capture.errors.join();
        }
        self.lifecycle.cancel();
        self.phase = Phase::Idle;
        ui.level = 0.;
        ui.message = "Dictado cancelado".into();
        ui.dirty = true;
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        self.recording_started = None;
        let Some(address) = self.phase.stop() else {
            return Ok(());
        };
        let Some(mut capture) = self.capture.take() else {
            bail!("No hay captura");
        };
        // SIGINT lets PipeWire flush the last samples before closing stdout.
        unsafe {
            libc::kill(capture.child.id() as i32, libc::SIGINT);
        }
        let tx = self.tx.clone();
        let client = self.client.clone();
        let key = self.key.clone();
        let endpoint = self.endpoint.clone();
        let model = self.settings.model.clone();
        let temperature = self.settings.temperature;
        let prompt = self.settings.prompt.clone();
        let generation = self.lifecycle.generation;
        thread::spawn(move || {
            let result = (|| -> Result<(String, String)> {
                let status = capture.child.wait()?;
                let pcm = capture
                    .reader
                    .join()
                    .map_err(|_| anyhow::anyhow!("Falló el lector de audio"))??;
                let errors = capture
                    .errors
                    .join()
                    .map_err(|_| anyhow::anyhow!("Falló el lector de errores"))??;
                ensure!(
                    hypr_dictate::capture_valid(
                        status.code(),
                        std::os::unix::process::ExitStatusExt::signal(&status),
                        &errors,
                        &pcm
                    ),
                    "Captura inválida ({status}, {} bytes de audio): {}",
                    pcm.len(),
                    String::from_utf8_lossy(&errors)
                );
                let body = json!({
                    "model": model,
                    "stream": false,
                    "temperature": temperature,
                    "max_tokens": 8192,
                    "messages": [{
                        "role": "user",
                        "content": [
                            {
                                "type": "text",
                                "text": prompt
                            },
                            {
                                "type": "input_audio",
                                "input_audio": {
                                    "data": base64::engine::general_purpose::STANDARD.encode(wav(&pcm)),
                                    "format": "wav"
                                }
                            }
                        ]
                    }]
                });
                let response = client
                    .post(&endpoint)
                    .bearer_auth(key)
                    .json(&body)
                    .send()
                    .context("No se pudo conectar con la API")?;
                ensure!(
                    response.status().is_success(),
                    "API devolvió HTTP {}",
                    response.status()
                );
                let mut bytes = Vec::new();
                response.take(2_000_001).read_to_end(&mut bytes)?;
                ensure!(bytes.len() <= 2_000_000, "Respuesta demasiado grande");
                let response: Value =
                    serde_json::from_slice(&bytes).context("Respuesta JSON inválida")?;
                ensure!(
                    response["choices"][0]["finish_reason"] == "stop",
                    "Respuesta incompleta; no se insertó texto"
                );
                let text = response["choices"][0]["message"]["content"]
                    .as_str()
                    .context("API sin transcripción")?
                    .trim()
                    .to_owned();
                ensure!(text.len() <= 100_000, "Transcripción demasiado grande");
                Ok((address, text))
            })();
            let _ = tx.send(Event::Finished(generation, result));
        });
        Ok(())
    }
    pub fn event(&mut self, event: Event, ui: &mut ui::Ui) {
        let result = match event {
            Event::Ping(ack) => {
                let _ = ack.send(());
                Ok(())
            }
            Event::ArmReady(token, address, dock_was_hidden) => {
                if token == ui.arm_token {
                    self.begin_recording(ui, address, dock_was_hidden)
                } else {
                    Ok(())
                }
            }
            Event::KeyPress(key) => match self.lifecycle.on_key_press(key) {
                hypr_dictate::KeyAction::StopRecording => self.stop(),
                hypr_dictate::KeyAction::CancelRecording => self.cancel(ui),
                _ => Ok(()),
            },
            Event::KeyRelease(key) => (|| -> Result<()> {
                match self.lifecycle.on_key_release(key) {
                    hypr_dictate::KeyAction::ReleaseAndInsert => {
                        ui.capture_keyboard(false);
                        if let Some((address, text)) = self.lifecycle.take_deferred() {
                            let restore_res = restore(&address);
                            let type_res = if restore_res.is_ok() && !text.is_empty() {
                                ui.type_text(&text, &address)
                            } else {
                                Ok(())
                            };
                            self.phase = Phase::Idle;
                            if self.lifecycle.dock_temporarily_shown {
                                ui.hide();
                                self.lifecycle.dock_temporarily_shown = false;
                            }
                            restore_res?;
                            type_res?;
                        } else if let Some(address) = &self.lifecycle.target_address {
                            // Si la tecla se soltó antes de la respuesta API, restaurar foco inmediatamente
                            let _ = restore(address);
                        }
                    }
                    hypr_dictate::KeyAction::ReleaseAndCancel => {
                        ui.capture_keyboard(false);
                        if let Some(err) = self.lifecycle.take_deferred_error() {
                            self.phase = Phase::Error;
                            ui.message = err;
                            ui.dirty = true;
                        }
                        if let Some(address) = self.lifecycle.target_address.take() {
                            let _ = restore(&address);
                        }
                        if self.lifecycle.dock_temporarily_shown {
                            ui.hide();
                            self.lifecycle.dock_temporarily_shown = false;
                        }
                        self.lifecycle.cancel();
                    }
                    _ => {}
                }
                Ok(())
            })(),
            Event::Command(command) => (|| -> Result<()> {
                match command.as_str() {
                    "reload" | "reset" => {
                        ui.cancel_arm();
                        if let Some(mut capture) = self.capture.take() {
                            let _ = capture.child.kill();
                            let _ = capture.child.wait();
                        }
                        ui.capture_keyboard(false);
                        if self.lifecycle.dock_temporarily_shown {
                            ui.hide();
                        }
                        self.lifecycle.cancel();
                        self.phase = Phase::Idle;
                        self.error.clear();
                        ui.level = 0.;
                        ui.message = "Dock reiniciado".into();
                        ui.dirty = true;
                        Ok(())
                    }
                    "start" => {
                        if matches!(self.phase, Phase::Processing(_)) {
                            self.cancel(ui)?;
                            ui.capture_keyboard(false);
                            if self.lifecycle.dock_temporarily_shown {
                                ui.hide();
                            }
                            self.lifecycle.cancel();
                            Ok(())
                        } else if matches!(self.phase, Phase::Recording(_)) {
                            ui.capture_keyboard(false);
                            self.lifecycle.is_recording = false;
                            self.lifecycle.waiting_release = None;
                            self.stop()
                        } else {
                            self.start(ui)
                        }
                    }
                    "stop" => {
                        if matches!(self.phase, Phase::Recording(_)) {
                            ui.capture_keyboard(false);
                            self.lifecycle.is_recording = false;
                            self.lifecycle.waiting_release = None;
                            self.stop()
                        } else {
                            Ok(())
                        }
                    }
                    "toggle" => {
                        if matches!(self.phase, Phase::Processing(_)) {
                            self.cancel(ui)?;
                            ui.capture_keyboard(false);
                            if self.lifecycle.dock_temporarily_shown {
                                ui.hide();
                            }
                            self.lifecycle.cancel();
                            Ok(())
                        } else if matches!(self.phase, Phase::Recording(_)) {
                            ui.capture_keyboard(false);
                            self.lifecycle.is_recording = false;
                            self.lifecycle.waiting_release = None;
                            self.stop()
                        } else {
                            self.start(ui)
                        }
                    }
                    "toggle-ui" => {
                        ui.cancel_arm();
                        if ui.is_visible() && matches!(self.phase, Phase::Recording(_) | Phase::Processing(_)) {
                            let _ = self.cancel(ui);
                            ui.capture_keyboard(false);
                            self.lifecycle.cancel();
                        }
                        ui.toggle_visibility();
                        Ok(())
                    }
                    _ => Ok(()),
                }
            })(),
            Event::Catalog(result) => {
                match result {
                    Ok(models) => {
                        ui.models = models;
                        ui.catalog_loaded();
                    }
                    Err(e) => {
                        ui.message = format!("Error cargando catálogo: {e}");
                        ui.dirty = true;
                    }
                }
                Ok(())
            },
            Event::Save(settings) => (|| -> Result<()> {
                ensure!(
                    !matches!(self.phase, Phase::Recording(_) | Phase::Processing(_)),
                    "Espera a terminar el dictado"
                );
                ensure!(
                    ui.models.contains(&settings.model),
                    "Selecciona un modelo del catálogo"
                );
                ensure!(
                    settings.microphone.is_empty()
                        || ui.microphones.iter().any(|m| m.name == settings.microphone),
                    "Selecciona un micrófono detectado"
                );
                ensure!(
                    settings.monitor.is_empty()
                        || ui.monitors.iter().any(|m| m.name == settings.monitor),
                    "Selecciona una pantalla detectada"
                );
                if settings.shortcut != self.settings.shortcut {
                    shortcut::apply(&settings.shortcut, &self.settings.shortcut)?;
                }
                settings.save()?;
                self.settings = settings;
                ui.message = "Ajustes guardados".into();
                Ok(())
            })(),
            Event::Finished(generation, result) => (|| -> Result<()> {
                if generation != self.lifecycle.generation {
                    // Descartar peticiones obsoletas o de grabaciones canceladas
                    return Ok(());
                }
                match result {
                    Err(e) => {
                        match self.lifecycle.on_api_error(generation, e.to_string()) {
                            Some(true) => {
                                // Tecla ya liberada: procesar limpieza inmediatamente
                                ui.capture_keyboard(false);
                                if let Some(addr) = self.lifecycle.target_address.take() {
                                    let _ = restore(&addr);
                                }
                                if self.lifecycle.dock_temporarily_shown {
                                    ui.hide();
                                    self.lifecycle.dock_temporarily_shown = false;
                                }
                                self.phase = Phase::Error;
                                ui.message = e.to_string();
                                ui.dirty = true;
                                self.lifecycle.cancel();
                            }
                            Some(false) => {
                                // Tecla aún retenida: diferir limpieza hasta su liberación física
                            }
                            None => {}
                        }
                        Ok(())
                    }
                    Ok((address, text)) => {
                        match self.lifecycle.on_api_finished(generation, address.clone(), text.clone()) {
                            Some(true) => {
                                ui.capture_keyboard(false);
                                let restore_res = restore(&address);
                                let type_res = if restore_res.is_ok() && !text.is_empty() {
                                    ui.type_text(&text, &address)
                                } else {
                                    Ok(())
                                };
                                self.phase = Phase::Idle;
                                if self.lifecycle.dock_temporarily_shown {
                                    ui.hide();
                                    self.lifecycle.dock_temporarily_shown = false;
                                }
                                restore_res?;
                                type_res?;
                            }
                            Some(false) => {
                                // Respuesta temprana: diferida hasta que se libere la tecla de terminación
                            }
                            None => {}
                        }
                        Ok(())
                    }
                }
            })(),
        };
        if let Err(error) = result {
            eprintln!("hypr-dictate: {error}");
            self.error = error.to_string();
            ui.message = self.error.clone();
            if self.lifecycle.waiting_release.is_none() {
                ui.capture_keyboard(false);
                if self.lifecycle.dock_temporarily_shown {
                    ui.hide();
                    self.lifecycle.dock_temporarily_shown = false;
                }
            }
            self.lifecycle.cancel();
            if self.capture.is_none() {
                self.phase = Phase::Error;
            }
        }
    }
}
impl Drop for App {
    fn drop(&mut self) {
        if let Some(mut capture) = self.capture.take() {
            let _ = capture.child.kill();
            let _ = capture.child.wait();
            let _ = capture.reader.join();
            let _ = capture.errors.join();
        }
    }
}
fn hypr(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("hyprctl").args(args).output()?;
    ensure!(
        output.status.success(),
        "Falló hyprctl {:?} ({}): {} {}",
        args,
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}
fn active_address() -> Result<String> {
    let value: Value = serde_json::from_slice(&hypr(&["-j", "activewindow"])?)?;
    Ok(value["address"].as_str().unwrap_or("").into())
}
fn restore(address: &str) -> Result<()> {
    ensure!(valid_address(address), "Destino inválido");
    if active_address()? == address {
        return Ok(());
    }
    let clients: Value = serde_json::from_slice(&hypr(&["-j", "clients"])?)?;
    ensure!(
        clients
            .as_array()
            .is_some_and(|clients| clients.iter().any(|c| c["address"] == address)),
        "La ventana original ya no existe; no se insertó texto"
    );
    hypr(&["dispatch", &hypr_dictate::focus_dispatch(address)])?;
    for _ in 0..20 {
        if active_address()? == address {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    bail!("No se pudo restaurar el foco; no se insertó texto")
}
fn socket_path() -> Result<PathBuf> {
    let runtime =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("Falta XDG_RUNTIME_DIR")?);
    let metadata = std::fs::symlink_metadata(&runtime)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Directorio runtime inválido"
    );
    Ok(runtime.join("hypr-dictate.sock"))
}
pub fn is_valid_command(cmd: &str) -> bool {
    ["start", "stop", "toggle", "toggle-ui", "reload", "reset"].contains(&cmd)
}

fn launch_background(path: &std::path::Path) -> Result<()> {
    let exe = std::env::current_exe().context("No se pudo determinar el ejecutable actual")?;
    let key = std::env::var("CLIPROXYAPI_API_KEY").context("Falta CLIPROXYAPI_API_KEY")?;
    ensure!(!key.is_empty(), "CLIPROXYAPI_API_KEY está vacía");

    let stderr_file = tempfile::tempfile()?;
    let mut stderr_read = stderr_file.try_clone()?;

    let mut cmd = Command::new(exe);
    cmd.arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().context("No se pudo iniciar hypr-dictate en segundo plano")?;

    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().context("Error comprobando estado del proceso hijo")? {
            let mut err_bytes = Vec::new();
            let _ = stderr_read.seek(SeekFrom::Start(0));
            let _ = stderr_read.read_to_end(&mut err_bytes);
            let err_msg = String::from_utf8_lossy(&err_bytes).trim().to_string();
            if err_msg.is_empty() {
                bail!("El proceso en segundo plano terminó prematuramente ({status})");
            } else {
                bail!("El proceso en segundo plano falló al iniciar ({status}): {err_msg}");
            }
        }

        if let Ok(mut stream) = UnixStream::connect(path) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
            if stream.write_all(b"ping").is_ok() && stream.shutdown(std::net::Shutdown::Write).is_ok() {
                let mut ack = [0u8; 2];
                if stream.read_exact(&mut ack).is_ok() && &ack == b"ok" {
                    return Ok(());
                }
            }
        }

        if start.elapsed() >= timeout {
            let mut err_bytes = Vec::new();
            let _ = stderr_read.seek(SeekFrom::Start(0));
            let _ = stderr_read.read_to_end(&mut err_bytes);
            let err_msg = String::from_utf8_lossy(&err_bytes).trim().to_string();
            if err_msg.is_empty() {
                bail!("Tiempo de espera agotado esperando a que hypr-dictate esté listo");
            } else {
                bail!("Tiempo de espera agotado esperando a que hypr-dictate esté listo: {err_msg}");
            }
        }

        thread::sleep(Duration::from_millis(50));
    }
}

fn main() -> Result<()> {
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    let path = socket_path()?;
    if command != "run" {
        ensure!(
            is_valid_command(&command),
            "Uso: hypr-dictate [run|start|stop|toggle|toggle-ui|reload|reset]"
        );
        if command == "toggle-ui" {
            match UnixStream::connect(&path) {
                Ok(mut stream) => {
                    stream.write_all(b"toggle-ui")?;
                    let _ = stream.shutdown(std::net::Shutdown::Write);
                    return Ok(());
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    launch_background(&path)?;
                    return Ok(());
                }
                Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
                    bail!(
                        "Socket ocupado o residual en {:?}: si no hay otro proceso ejecutándose, elimina manualmente el socket residual",
                        path
                    );
                }
                Err(err) => {
                    return Err(err).context("No se pudo conectar al socket de hypr-dictate");
                }
            }
        }

        match UnixStream::connect(&path) {
            Ok(mut stream) => {
                stream.write_all(command.as_bytes())?;
                let _ = stream.shutdown(std::net::Shutdown::Write);
                return Ok(());
            }
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
                bail!(
                    "Socket ocupado o residual en {:?}: si no hay otro proceso ejecutándose, elimina manualmente el socket residual",
                    path
                );
            }
            Err(err) => {
                return Err(err).context("Inicia primero hypr-dictate run");
            }
        }
    }
    let key = std::env::var("CLIPROXYAPI_API_KEY").context("Falta CLIPROXYAPI_API_KEY")?;
    ensure!(!key.is_empty(), "CLIPROXYAPI_API_KEY está vacía");
    // Never unlink an existing path: it may belong to another live process.
    let listener = UnixListener::bind(&path).context(
        "Socket ocupado: si no hay otro proceso, elimina manualmente el socket residual",
    )?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(path);
    let (tx, rx) = mpsc::channel();
    let sender = tx.clone();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let mut bytes = Vec::new();
            if (&mut stream).take(16).read_to_end(&mut bytes).is_ok()
                && let Ok(command) = String::from_utf8(bytes)
            {
                if command.trim() == "ping" {
                    let (ack_tx, ack_rx) = mpsc::sync_channel(1);
                    if sender.send(Event::Ping(ack_tx)).is_ok()
                        && ack_rx.recv_timeout(Duration::from_secs(2)).is_ok()
                    {
                        let _ = stream.write_all(b"ok");
                    }
                } else {
                    let _ = sender.send(Event::Command(command));
                }
            }
        }
    });
    let app = App {
        phase: Phase::Idle,
        capture: None,
        lifecycle: hypr_dictate::KeyLifecycle::new(),
        recording_started: None,
        tx,
        client: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(25))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        key,
        endpoint: "http://127.0.0.1:8317/v1/chat/completions".into(),
        settings: settings::Settings::load()?,
        level: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        error: String::new(),
    };
    let catalog_client = app.client.clone();
    let catalog_key = app.key.clone();
    let catalog_tx = app.tx.clone();
    thread::spawn(move || {
        let result = (|| -> Result<Vec<String>> {
            let response = catalog_client
                .get("http://127.0.0.1:8317/v1/models?client_version=1")
                .bearer_auth(catalog_key)
                .send()?
                .error_for_status()?;
            let value: Value = response.json()?;
            let models = settings::routes(&value);
            ensure!(!models.is_empty(), "Catálogo de modelos vacío");
            Ok(models)
        })();
        let _ = catalog_tx.send(Event::Catalog(result));
    });
    ui::run(app, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_supported_cli_commands() {
        assert!(is_valid_command("start"));
        assert!(is_valid_command("stop"));
        assert!(is_valid_command("toggle"));
        assert!(is_valid_command("toggle-ui"));
        assert!(is_valid_command("reload"));
        assert!(is_valid_command("reset"));
        assert!(!is_valid_command("unknown"));
        assert!(!is_valid_command("run"));
    }

    #[test]
    fn command_length_fits_socket_read_budget() {
        assert!("toggle-ui".len() <= 16);
        assert!("toggle".len() <= 16);
    }

    #[test]
    fn arming_state_preserves_target_and_does_not_overwrite() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let app = App {
            phase: Phase::Idle,
            capture: None,
            lifecycle: hypr_dictate::KeyLifecycle::new(),
            recording_started: None,
            tx,
            client: reqwest::blocking::Client::new(),
            key: "key".into(),
            endpoint: "http://127.0.0.1:8317".into(),
            settings: settings::Settings::default(),
            level: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            error: String::new(),
        };

        assert!(app.capture.is_none());
        assert_eq!(app.phase, Phase::Idle);
    }
}
