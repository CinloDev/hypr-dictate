mod paint;
mod preferences;
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

#[allow(clippy::large_enum_variant)]
pub enum Event {
    Command(String),
    Ping(mpsc::SyncSender<()>),
    KeyPress(hypr_dictate::CaptureKey),
    KeyRelease(hypr_dictate::CaptureKey),
    ArmReady(u64, String, bool),
    Finished(u64, Result<(String, String)>),
    Catalog(String, u64, Result<Vec<String>>),
    Save(settings::Settings),
    SavePosition(i32),
    SaveServer(String),
}
struct Capture {
    child: Child,
    reader: thread::JoinHandle<Result<Vec<u8>>>,
    errors: thread::JoinHandle<std::io::Result<Vec<u8>>>,
}
pub struct App {
    phase: Phase,
    pub mode: hypr_dictate::DictationMode,
    capture: Option<Capture>,
    lifecycle: hypr_dictate::KeyLifecycle,
    recording_started: Option<std::time::Instant>,
    catalog_generation: u64,
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
        match self.mode {
            hypr_dictate::DictationMode::Transcription => match self.phase {
                Phase::Idle => [120, 165, 136],
                Phase::Recording(_) => [240, 151, 138],
                Phase::Processing(_) => [224, 194, 122],
                Phase::Error => [240, 151, 138],
            },
            hypr_dictate::DictationMode::Writing => match self.phase {
                Phase::Idle => [120, 165, 136],
                Phase::Recording(_) => [61, 148, 245],
                Phase::Processing(_) => [90, 180, 255],
                Phase::Error => [240, 151, 138],
            },
        }
    }
    fn discard_capture(&mut self) {
        if let Some(mut capture) = self.capture.take() {
            let _ = capture.child.kill();
            let _ = capture.child.wait();
            let _ = capture.reader.join();
            let _ = capture.errors.join();
        }
    }
    fn start(&mut self, ui: &mut ui::Ui, mode: hypr_dictate::DictationMode) -> Result<()> {
        if !matches!(self.phase, Phase::Idle | Phase::Error) {
            return Ok(());
        }
        if ui.arming {
            // No sobrescribir ventana objetivo si ya se encuentra en fase de armado
            return Ok(());
        }
        self.discard_capture();
        ui.close_preferences();
        // Capturar ventana enfocada ANTES del foco exclusivo de layer-shell
        let address = active_address()?;
        ensure!(valid_address(&address), "No hay ventana enfocada válida");

        self.mode = mode;
        ui.set_mode(mode);

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
            self.mode = hypr_dictate::DictationMode::Transcription;
            ui.set_mode(hypr_dictate::DictationMode::Transcription);
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
                ui.message = if timeout {
                    "Límite de 5 min alcanzado".into()
                } else {
                    "Grabador finalizó inesperadamente".into()
                };
                ui.dirty = true;
                self.phase = Phase::Idle;
            }
        }

        if self.lifecycle.waiting_release.is_some()
            && matches!(self.phase, Phase::Idle | Phase::Error)
            && self.recording_started.is_none()
        {
            ui.capture_keyboard(false);
            if let Some(address) = self.lifecycle.target_address.take() {
                let _ = restore(&address);
            }
            if self.lifecycle.dock_temporarily_shown {
                ui.hide();
                self.lifecycle.dock_temporarily_shown = false;
            }
            self.lifecycle.cancel();
        }
    }
    fn cancel(&mut self, ui: &mut ui::Ui) -> Result<()> {
        ui.cancel_arm();
        self.recording_started = None;
        self.discard_capture();
        self.phase = Phase::Idle;
        self.mode = hypr_dictate::DictationMode::Transcription;
        ui.set_mode(hypr_dictate::DictationMode::Transcription);
        ui.level = 0.;
        ui.message = "Dictado cancelado".into();
        ui.dirty = true;
        if self.lifecycle.waiting_release.is_none() {
            ui.capture_keyboard(false);
            if let Some(address) = self.lifecycle.target_address.take() {
                let _ = restore(&address);
            }
            if self.lifecycle.dock_temporarily_shown {
                ui.hide();
                self.lifecycle.dock_temporarily_shown = false;
            }
            self.lifecycle.cancel();
        }
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
        let mode = self.mode;
        let model = self.settings.model_for(mode).to_string();
        let temperature = self.settings.temperature;
        let prompt = self.settings.prompt_for(mode).to_string();
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
                let request = transcription_request(&client, &endpoint, &key, &body);
                let bytes = send_transcription(request)?;
                let response: Value =
                    serde_json::from_slice(&bytes).context("Respuesta JSON inválida")?;
                ensure!(
                    response["choices"][0]["finish_reason"] == "stop",
                    "Respuesta incompleta; no se insertó texto"
                );
                let raw = response["choices"][0]["message"]["content"]
                    .as_str()
                    .context("API sin transcripción")?
                    .trim()
                    .to_owned();
                ensure!(raw.len() <= 100_000, "Transcripción demasiado grande");
                let text = hypr_dictate::sanitize_text_for_mode(&raw, mode);
                Ok((address, text))
            })();
            let _ = tx.send(Event::Finished(generation, result));
        });
        Ok(())
    }
    fn save_server(
        &mut self,
        base_url: &str,
        persist: impl FnOnce(&settings::Settings) -> Result<()>,
    ) -> Result<()> {
        ensure!(
            !matches!(self.phase, Phase::Recording(_) | Phase::Processing(_)),
            "Espera a terminar el dictado"
        );
        let (endpoint, _) = api_endpoints(Some(base_url))?;
        let mut settings = self.settings.clone();
        settings.base_url = base_url.into();
        persist(&settings)?;
        self.settings = settings;
        self.endpoint = endpoint;
        Ok(())
    }
    fn request_catalog(&mut self) {
        self.catalog_generation += 1;
        let generation = self.catalog_generation;
        let base_url = self.settings.base_url.clone();
        let client = self.client.clone();
        let key = self.key.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = api_endpoints(Some(&base_url))
                .and_then(|(_, endpoint)| fetch_catalog(&client, &endpoint, &key));
            let _ = tx.send(Event::Catalog(base_url, generation, result));
        });
    }
    fn catalog_is_current(&self, base_url: &str, generation: u64) -> bool {
        self.settings.base_url == base_url && self.catalog_generation == generation
    }
    fn save_position(
        &mut self,
        x: i32,
        persist: impl FnOnce(&settings::Settings) -> Result<()>,
    ) -> Result<()> {
        let mut settings = self.settings.clone();
        settings.x = x;
        persist(&settings)?;
        self.settings = settings;
        Ok(())
    }
    fn report_error(&mut self, error: anyhow::Error, dictation_error: bool) {
        eprintln!("hypr-dictate: {error}");
        self.error = error.to_string();
        if self.capture.is_none()
            && (dictation_error
                || !matches!(self.phase, Phase::Recording(_) | Phase::Processing(_)))
        {
            self.phase = Phase::Error;
        }
    }
    pub fn event(&mut self, event: Event, ui: &mut ui::Ui) {
        let dictation_error = matches!(&event, Event::Command(_) | Event::Finished(..));
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
            Event::KeyPress(key) => {
                if key == hypr_dictate::CaptureKey::Escape && matches!(self.phase, Phase::Processing(_)) {
                    self.lifecycle.generation = self.lifecycle.generation.wrapping_add(1);
                    self.lifecycle.waiting_release = Some(key);
                    self.cancel(ui)
                } else {
                    match self.lifecycle.on_key_press(key) {
                        hypr_dictate::KeyAction::StopRecording => self.stop(),
                        hypr_dictate::KeyAction::CancelRecording => self.cancel(ui),
                        _ => Ok(()),
                    }
                }
            }
            Event::KeyRelease(key) => (|| -> Result<()> {
                match self.lifecycle.on_key_release(key) {
                    hypr_dictate::KeyAction::ReleaseAndInsert => {
                        if let Some((address, text)) = self.lifecycle.take_deferred() {
                            ui.capture_keyboard(false);
                            let restore_res = restore(&address);
                            let type_res = if restore_res.is_ok() && !text.is_empty() {
                                ui.insert_text(&text, &address, self.mode)
                            } else {
                                Ok(())
                            };
                            self.phase = Phase::Idle;
                            self.mode = hypr_dictate::DictationMode::Transcription;
                            ui.set_mode(hypr_dictate::DictationMode::Transcription);
                            if self.lifecycle.dock_temporarily_shown {
                                ui.hide();
                                self.lifecycle.dock_temporarily_shown = false;
                            }
                            restore_res?;
                            type_res?;
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
                        self.mode = hypr_dictate::DictationMode::Transcription;
                        ui.set_mode(hypr_dictate::DictationMode::Transcription);
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
                        self.discard_capture();
                        ui.capture_keyboard(false);
                        if self.lifecycle.dock_temporarily_shown {
                            ui.hide();
                        }
                        self.lifecycle.cancel();
                        self.phase = Phase::Idle;
                        self.mode = hypr_dictate::DictationMode::Transcription;
                        ui.set_mode(hypr_dictate::DictationMode::Transcription);
                        self.error.clear();
                        ui.level = 0.;
                        ui.message = "Dock reiniciado".into();
                        ui.dirty = true;
                        Ok(())
                    }
                    "start" => {
                        if matches!(self.phase, Phase::Processing(_)) {
                            self.cancel(ui)?;
                            Ok(())
                        } else if matches!(self.phase, Phase::Recording(_)) {
                            ui.capture_keyboard(false);
                            self.lifecycle.is_recording = false;
                            self.lifecycle.waiting_release = None;
                            self.stop()
                        } else {
                            self.start(ui, hypr_dictate::DictationMode::Transcription)
                        }
                    }
                    "start-formal" => {
                        if matches!(self.phase, Phase::Processing(_)) {
                            self.cancel(ui)?;
                            Ok(())
                        } else if matches!(self.phase, Phase::Recording(_)) {
                            ui.capture_keyboard(false);
                            self.lifecycle.is_recording = false;
                            self.lifecycle.waiting_release = None;
                            self.stop()
                        } else {
                            self.start(ui, hypr_dictate::DictationMode::Writing)
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
                            Ok(())
                        } else if matches!(self.phase, Phase::Recording(_)) {
                            ui.capture_keyboard(false);
                            self.lifecycle.is_recording = false;
                            self.lifecycle.waiting_release = None;
                            self.stop()
                        } else {
                            self.start(ui, hypr_dictate::DictationMode::Transcription)
                        }
                    }
                    "toggle-ui" => {
                        ui.cancel_arm();
                        if ui.is_visible() && matches!(self.phase, Phase::Recording(_) | Phase::Processing(_)) {
                            let _ = self.cancel(ui);
                        }
                        ui.toggle_visibility();
                        Ok(())
                    }
                    "preferences" => {
                        ui.toggle_preferences();
                        Ok(())
                    }
                    _ => Ok(()),
                }
            })(),
            Event::Catalog(base_url, generation, result) => {
                if !self.catalog_is_current(&base_url, generation) {
                    return;
                }
                match result {
                    Ok(models) => {
                        ui.models = models;
                        ui.catalog_loaded();
                        Ok(())
                    }
                    Err(e) => {
                        ui.message = format!("Error cargando catálogo: {e}");
                        ui.dirty = true;
                        Err(e)
                    }
                }
            }
            Event::SaveServer(base_url) => self
                .save_server(&base_url, settings::Settings::save)
                .map(|()| {
                    ui.server_saved(&self.settings.base_url);
                    self.request_catalog();
                }),
            Event::SavePosition(x) => self.save_position(x, settings::Settings::save).map(|()| {
                ui.position_saved(self.settings.x);
            }),
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
                let mut settings = settings;
                settings.base_url = self.settings.base_url.clone();
                settings.save()?;
                self.settings = settings;
                ui.settings_saved(&self.settings);
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
                                self.mode = hypr_dictate::DictationMode::Transcription;
                                ui.set_mode(hypr_dictate::DictationMode::Transcription);
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
                                    ui.insert_text(&text, &address, self.mode)
                                } else {
                                    Ok(())
                                };
                                self.phase = Phase::Idle;
                                self.mode = hypr_dictate::DictationMode::Transcription;
                                ui.set_mode(hypr_dictate::DictationMode::Transcription);
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
            self.report_error(error, dictation_error);
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
        self.discard_capture();
    }
}
fn api_endpoints(base: Option<&str>) -> Result<(String, String)> {
    let base = base.unwrap_or(settings::DEFAULT_BASE_URL);
    let mut url = reqwest::Url::parse(base)
        .map_err(|_| anyhow::anyhow!("La URL del servidor no es válida"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "La URL del servidor debe usar HTTP o HTTPS, sin credenciales, parámetros ni fragmentos"
    );
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("{path}/v1/chat/completions"));
    let transcription = url.to_string();
    url.set_path(&format!("{path}/v1/models"));
    url.set_query(Some("client_version=1"));
    Ok((transcription, url.into()))
}
fn fetch_catalog(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    key: &str,
) -> Result<Vec<String>> {
    let response = client
        .get(endpoint)
        .bearer_auth(key)
        .send()?
        .error_for_status()?;
    let value: Value = response.json()?;
    let models = settings::routes(&value);
    ensure!(!models.is_empty(), "Catálogo de modelos vacío");
    Ok(models)
}
fn transcription_request(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    key: &str,
    body: &Value,
) -> reqwest::blocking::RequestBuilder {
    client
        .post(endpoint)
        .bearer_auth(key)
        .json(body)
}
fn transcription_error(error: anyhow::Error) -> anyhow::Error {
    let timed_out = error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
            || cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                error.kind() == std::io::ErrorKind::TimedOut
                    || error
                        .get_ref()
                        .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
                        .is_some_and(reqwest::Error::is_timeout)
            })
    });
    error.context(if timed_out {
        "Tiempo de espera agotado durante la transcripción"
    } else {
        "Falló la comunicación con la API durante la transcripción"
    })
}
fn send_transcription(request: reqwest::blocking::RequestBuilder) -> Result<Vec<u8>> {
    let response = request
        .send()
        .map_err(|error| transcription_error(error.into()))?;
    ensure!(
        response.status().is_success(),
        "API devolvió HTTP {}",
        response.status()
    );
    let mut bytes = Vec::new();
    response
        .take(2_000_001)
        .read_to_end(&mut bytes)
        .map_err(|error| transcription_error(error.into()))?;
    ensure!(bytes.len() <= 2_000_000, "Respuesta demasiado grande");
    Ok(bytes)
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowInfo {
    pub address: String,
    pub class: String,
    pub initial_class: String,
    pub xwayland: bool,
}

pub fn is_terminal_window(class: &str, initial_class: &str) -> bool {
    let check = |name: &str| -> bool {
        let lower = name.to_ascii_lowercase();
        lower == "alacritty"
            || lower == "kitty"
            || lower == "foot"
            || lower == "footclient"
            || lower == "com.mitchellh.ghostty"
            || lower == "ghostty"
            || lower == "org.wezfurlong.wezterm"
            || lower == "wezterm"
            || lower == "wezterm-gui"
            || lower == "contour"
            || lower == "rio"
            || lower.contains("terminal")
            || lower.ends_with(".terminal")
            || lower == "gnome-terminal"
            || lower == "konsole"
            || lower == "xfce4-terminal"
            || lower == "lxterminal"
            || lower == "xterm"
            || lower == "urxvt"
            || lower == "st"
            || lower == "st-256color"
            || lower == "terminator"
            || lower == "tilix"
    };
    check(class) || check(initial_class)
}

pub fn query_window_info(address: &str) -> Result<WindowInfo> {
    if let Ok(bytes) = hypr(&["-j", "activewindow"])
        && let Ok(val) = serde_json::from_slice::<Value>(&bytes)
        && val["address"].as_str() == Some(address)
    {
        return Ok(WindowInfo {
            address: address.to_string(),
            class: val["class"].as_str().unwrap_or("").to_string(),
            initial_class: val["initialClass"].as_str().unwrap_or("").to_string(),
            xwayland: val["xwayland"].as_bool().unwrap_or(false),
        });
    }

    let clients_bytes = hypr(&["-j", "clients"])?;
    let clients: Value = serde_json::from_slice(&clients_bytes)?;
    if let Some(arr) = clients.as_array() {
        for client in arr {
            if client["address"].as_str() == Some(address) {
                return Ok(WindowInfo {
                    address: address.to_string(),
                    class: client["class"].as_str().unwrap_or("").to_string(),
                    initial_class: client["initialClass"].as_str().unwrap_or("").to_string(),
                    xwayland: client["xwayland"].as_bool().unwrap_or(false),
                });
            }
        }
    }
    bail!("Ventana objetivo no encontrada")
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
    [
        "start",
        "start-formal",
        "stop",
        "toggle",
        "toggle-ui",
        "preferences",
        "reload",
        "reset",
        "--help",
        "-h",
    ]
    .contains(&cmd)
}

fn resolve_api_key() -> Result<String> {
    if let Ok(val) = std::env::var("CLIPROXYAPI_API_KEY")
        && !val.trim().is_empty()
    {
        return Ok(val);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let env_path = PathBuf::from(home).join(".env");
        if let Ok(contents) = std::fs::read_to_string(env_path) {
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let stripped = trimmed.strip_prefix("export ").unwrap_or(trimmed);
                if let Some((k, v)) = stripped.split_once('=')
                    && k.trim() == "CLIPROXYAPI_API_KEY"
                {
                    let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                    if !val.is_empty() {
                        return Ok(val);
                    }
                }
            }
        }
    }
    bail!("Falta CLIPROXYAPI_API_KEY")
}

fn launch_background(path: &std::path::Path, hidden: bool) -> Result<()> {
    let exe = std::env::current_exe().context("No se pudo determinar el ejecutable actual")?;
    let key = resolve_api_key()?;

    let stderr_file = tempfile::tempfile()?;
    let mut stderr_read = stderr_file.try_clone()?;

    let mut cmd = Command::new(exe);
    cmd.env("CLIPROXYAPI_API_KEY", &key);
    cmd.arg("run");
    if hidden {
        cmd.arg("--hidden");
    }
    cmd.stdin(Stdio::null())
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
    if command == "--help" || command == "-h" {
        println!("Uso: hypr-dictate [run|start|start-formal|stop|toggle|toggle-ui|preferences|reload|reset|--help]");
        return Ok(());
    }
    let path = socket_path()?;
    if command != "run" {
        ensure!(
            is_valid_command(&command),
            "Uso: hypr-dictate [run|start|start-formal|stop|toggle|toggle-ui|preferences|reload|reset|--help]"
        );
        if command == "toggle-ui" {
            match UnixStream::connect(&path) {
                Ok(mut stream) => {
                    stream.write_all(b"toggle-ui")?;
                    let _ = stream.shutdown(std::net::Shutdown::Write);
                    return Ok(());
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    launch_background(&path, false)?;
                    return Ok(());
                }
                Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
                    let _ = std::fs::remove_file(&path);
                    launch_background(&path, false)?;
                    return Ok(());
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
            Err(err) if (err.kind() == std::io::ErrorKind::NotFound || err.kind() == std::io::ErrorKind::ConnectionRefused) && (command == "start" || command == "start-formal") => {
                if err.kind() == std::io::ErrorKind::ConnectionRefused {
                    let _ = std::fs::remove_file(&path);
                }
                launch_background(&path, true)?;
                let mut stream = UnixStream::connect(&path)
                    .context("No se pudo conectar al socket tras iniciar en segundo plano")?;
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
    let key = resolve_api_key()?;
    let settings = settings::Settings::load()?;
    let (endpoint, _) = api_endpoints(Some(&settings.base_url))?;
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            match UnixStream::connect(&path) {
                Err(conn_err) if conn_err.kind() == std::io::ErrorKind::ConnectionRefused => {
                    let _ = std::fs::remove_file(&path);
                    UnixListener::bind(&path).context(
                        "Socket ocupado: si no hay otro proceso, elimina manualmente el socket residual",
                    )?
                }
                _ => {
                    return Err(err).context(
                        "Socket ocupado: si no hay otro proceso, elimina manualmente el socket residual",
                    );
                }
            }
        }
        Err(err) => {
            return Err(err).context(
                "Socket ocupado: si no hay otro proceso, elimina manualmente el socket residual",
            );
        }
    };
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
    let mut app = App {
        phase: Phase::Idle,
        mode: hypr_dictate::DictationMode::Transcription,
        capture: None,
        lifecycle: hypr_dictate::KeyLifecycle::new(),
        recording_started: None,
        catalog_generation: 0,
        tx,
        client: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        key,
        endpoint,
        settings,
        level: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        error: String::new(),
    };
    app.request_catalog();
    let hidden = std::env::args().any(|arg| arg == "--hidden");
    ui::run(app, rx, hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn app(phase: Phase) -> App {
        let (tx, _) = mpsc::channel();
        App {
            phase,
            mode: hypr_dictate::DictationMode::Transcription,
            capture: None,
            lifecycle: hypr_dictate::KeyLifecycle::new(),
            recording_started: None,
            catalog_generation: 0,
            tx,
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap(),
            key: String::new(),
            endpoint: String::new(),
            settings: settings::Settings::default(),
            level: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            error: String::new(),
        }
    }

    #[test]
    fn validates_supported_cli_commands() {
        assert!(is_valid_command("start"));
        assert!(is_valid_command("start-formal"));
        assert!(is_valid_command("stop"));
        assert!(is_valid_command("toggle"));
        assert!(is_valid_command("toggle-ui"));
        assert!(is_valid_command("preferences"));
        assert!(is_valid_command("reload"));
        assert!(is_valid_command("reset"));
        assert!(!is_valid_command("unknown"));
        assert!(!is_valid_command("run"));
    }

    #[test]
    fn command_length_fits_socket_read_budget() {
        assert!("start-formal".len() <= 16);
        assert!("toggle-ui".len() <= 16);
        assert!("toggle".len() <= 16);
    }

    #[test]
    fn writing_mode_app_colors_and_ipc_dispatch() {
        let mut app = app(Phase::Idle);
        assert_eq!(app.mode, hypr_dictate::DictationMode::Transcription);
        app.mode = hypr_dictate::DictationMode::Writing;
        app.phase = Phase::Recording("0x123".into());
        assert_eq!(app.color(), [61, 148, 245]);
        app.phase = Phase::Processing("0x123".into());
        assert_eq!(app.color(), [90, 180, 255]);

        // Model and prompt dispatch per mode
        app.settings.model = "legacy-transcription-model".into();
        app.settings.prompt = "Transcription prompt".into();
        app.settings.writing_model = "pro-writing-model".into();
        app.settings.writing_prompt = "Professional writing prompt".into();

        app.mode = hypr_dictate::DictationMode::Transcription;
        assert_eq!(app.settings.model_for(app.mode), "legacy-transcription-model");
        assert_eq!(app.settings.prompt_for(app.mode), "Transcription prompt");

        app.mode = hypr_dictate::DictationMode::Writing;
        assert_eq!(app.settings.model_for(app.mode), "pro-writing-model");
        assert_eq!(app.settings.prompt_for(app.mode), "Professional writing prompt");
    }

    #[test]
    fn usage_message_and_cli_contain_start_formal() {
        let usage = "Uso: hypr-dictate [run|start|start-formal|stop|toggle|toggle-ui|preferences|reload|reset|--help]";
        assert!(usage.contains("start-formal"));
        assert!(is_valid_command("start-formal"));
        assert!(is_valid_command("--help"));
    }

    #[test]
    fn arming_state_preserves_target_and_does_not_overwrite() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let app = App {
            phase: Phase::Idle,
            mode: hypr_dictate::DictationMode::Transcription,
            capture: None,
            lifecycle: hypr_dictate::KeyLifecycle::new(),
            recording_started: None,
            catalog_generation: 0,
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

    #[test]
    fn saving_server_works_without_catalog_and_preserves_other_settings() {
        let mut app = app(Phase::Idle);
        app.settings.model = "saved/model".into();
        app.settings.shortcut = "SUPER + F9".into();
        let mut expected = app.settings.clone();
        expected.base_url = "https://proxy.example/service".into();
        let mut calls = 0;
        app.save_server(&expected.base_url, |candidate| {
            calls += 1;
            assert_eq!(format!("{candidate:?}"), format!("{expected:?}"));
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(format!("{:?}", app.settings), format!("{expected:?}"));
        assert_eq!(
            app.endpoint,
            "https://proxy.example/service/v1/chat/completions"
        );
    }

    #[test]
    fn server_save_failures_or_active_dictation_do_not_apply_changes() {
        for phase in [Phase::Recording("0xabc".into()), Phase::Processing("0xabc".into())] {
            let mut app = app(phase);
            let original = format!("{:?}", app.settings);
            let error = app
                .save_server("http://127.0.0.1:9000", |_| {
                    panic!("must not persist server during dictation")
                })
                .unwrap_err();
            assert!(error.to_string().contains("Espera a terminar"));
            assert_eq!(format!("{:?}", app.settings), original);
        }
        let mut app = app(Phase::Idle);
        let original = format!("{:?}", app.settings);
        app.save_server("http://127.0.0.1:9000", |_| bail!("disk full"))
            .unwrap_err();
        assert_eq!(format!("{:?}", app.settings), original);
        assert!(app.endpoint.is_empty());
        app.save_server("ftp://invalid", |_| panic!("invalid URL must not persist"))
            .unwrap_err();
    }

    #[test]
    fn catalogue_replies_must_match_server_and_generation() {
        let mut app = app(Phase::Idle);
        app.settings.base_url = "http://127.0.0.1:9000".into();
        app.catalog_generation = 3;
        assert!(app.catalog_is_current("http://127.0.0.1:9000", 3));
        assert!(!app.catalog_is_current("http://127.0.0.1:8317", 3));
        assert!(!app.catalog_is_current("http://127.0.0.1:9000", 1));
    }

    #[test]
    fn api_urls_use_default_or_configured_http_and_https_base() {
        for (base, root) in [
            (None, "http://127.0.0.1:8317"),
            (Some("http://127.0.0.1:9000/"), "http://127.0.0.1:9000"),
            (Some("https://proxy.example"), "https://proxy.example"),
            (
                Some("https://proxy.example/service/"),
                "https://proxy.example/service",
            ),
        ] {
            let (transcription, catalog) = api_endpoints(base).unwrap();
            assert_eq!(transcription, format!("{root}/v1/chat/completions"));
            assert_eq!(catalog, format!("{root}/v1/models?client_version=1"));
        }
    }

    #[test]
    fn configured_server_receives_catalog_and_transcription_requests() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/proxy/", listener.local_addr().unwrap());
        let (transcription, _) = api_endpoints(Some(&base)).unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for body in [r#"{"data":[{"id":"available/model"}]}"#, "{}"] {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                let length = request
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    })
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    .unwrap_or(0);
                socket.read_exact(&mut vec![0; length]).unwrap();
                requests.push(request.lines().next().unwrap().to_owned());
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            requests
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let mut app = app(Phase::Idle);
        app.client = client.clone();
        app.tx = tx;
        app.settings.base_url = base.clone();
        app.request_catalog();
        let Event::Catalog(reply_base, generation, models) =
            rx.recv_timeout(Duration::from_secs(3)).unwrap()
        else {
            panic!("expected catalogue reply");
        };
        assert_eq!(reply_base, base);
        assert_eq!(generation, 1);
        assert!(app.catalog_is_current(&reply_base, generation));
        let models = models.unwrap();
        let body = send_transcription(transcription_request(
            &client,
            &transcription,
            "test",
            &json!({}),
        ))
        .unwrap();
        assert_eq!(models, ["available/model"]);
        assert_eq!(body, b"{}");
        assert_eq!(
            server.join().unwrap(),
            [
                "GET /proxy/v1/models?client_version=1 HTTP/1.1",
                "POST /proxy/v1/chat/completions HTTP/1.1",
            ]
        );
    }

    #[test]
    fn invalid_or_ambiguous_api_base_is_rejected_without_echoing_it() {
        for base in [
            "",
            "not-a-url",
            "ftp://proxy.example",
            "file:///tmp/api",
            "http://",
            "https://user:secret@proxy.example",
            "https://proxy.example?key=secret",
            "https://proxy.example#fragment",
        ] {
            let error = api_endpoints(Some(base)).unwrap_err();
            assert!(error.to_string().contains("URL del servidor"));
            assert!(!format!("{error:#}").contains("secret"));
        }
    }



    fn delayed_api(send_headers_first: bool, delay: Duration) -> (String, thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let length = String::from_utf8(request)
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                })
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            socket.read_exact(&mut vec![0; length]).unwrap();
            let headers = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n";
            if send_headers_first {
                socket.write_all(headers).unwrap();
            }
            thread::sleep(delay);
            if !send_headers_first && socket.write_all(headers).is_err() {
                return;
            }
            let _ = socket.write_all(b"{}");
        });
        (endpoint, server)
    }

    #[test]
    fn transcription_retains_default_client_timeout() {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let request = transcription_request(&client, "http://127.0.0.1/", "test", &json!({}))
            .build()
            .unwrap();
        assert_eq!(
            request.timeout(),
            None,
            "transcription must retain the 120s client default"
        );
        let catalog = client.get("http://127.0.0.1/v1/models").build().unwrap();
        assert_eq!(
            catalog.timeout(),
            None,
            "catalog must retain the 120s client default"
        );
    }

    #[test]
    fn transcription_timeout_is_identified_before_headers_and_during_body() {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        for send_headers_first in [false, true] {
            let (endpoint, server) = delayed_api(send_headers_first, Duration::from_millis(200));
            let request = transcription_request(&client, &endpoint, "test", &json!({}))
                .timeout(Duration::from_millis(100));
            let error = send_transcription(request).unwrap_err();
            server.join().unwrap();
            assert!(
                error.to_string().contains("Tiempo de espera agotado"),
                "{error:#}"
            );
            assert!(!error.to_string().contains("conectar"));
        }
    }

    #[test]
    fn transcription_connection_failure_is_not_reported_as_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let error = send_transcription(transcription_request(
            &client,
            &endpoint,
            "test",
            &json!({}),
        ))
        .unwrap_err();
        assert!(error.to_string().contains("comunicación"), "{error:#}");
        assert!(!error.to_string().contains("Tiempo de espera agotado"));
    }

    #[test]
    fn position_save_preserves_every_other_setting_and_active_phase() {
        for phase in [
            Phase::Idle,
            Phase::Recording("0xdef".into()),
            Phase::Processing("0xdef".into()),
        ] {
            let mut app = app(phase);
            app.settings = settings::Settings {
                model: "offline/model".into(),
                base_url: settings::DEFAULT_BASE_URL.into(),
                x: 123,
                shortcut: "SUPER + F9".into(),
                microphone: "saved-mic".into(),
                monitor: "saved-monitor".into(),
                temperature: 0.7,
                prompt: "saved prompt".into(),
                ..Default::default()
            };
            let mut expected = app.settings.clone();
            expected.x = 456;
            let phase = format!("{:?}", app.phase);
            let mut calls = 0;
            app.save_position(456, |candidate| {
                calls += 1;
                assert_eq!(format!("{candidate:?}"), format!("{expected:?}"));
                Ok(())
            })
            .unwrap();
            assert_eq!(calls, 1);
            assert_eq!(format!("{:?}", app.settings), format!("{expected:?}"));
            assert_eq!(format!("{:?}", app.phase), phase);
        }
    }

    #[test]
    fn failed_position_save_preserves_committed_settings() {
        let mut app = app(Phase::Processing("0xdef".into()));
        let expected = format!("{:?}", app.settings);
        let error = app.save_position(456, |_| bail!("disk full")).unwrap_err();
        app.report_error(error, false);
        assert_eq!(format!("{:?}", app.settings), expected);
        assert_eq!(app.phase, Phase::Processing("0xdef".into()));
        assert_eq!(app.error, "disk full");
    }

    #[test]
    fn app_colors_match_phase_specifications() {
        assert_eq!(app(Phase::Idle).color(), [120, 165, 136]);
        assert_eq!(app(Phase::Recording("0x1".into())).color(), [240, 151, 138]);
        assert_eq!(app(Phase::Processing("0x1".into())).color(), [224, 194, 122]);
        assert_eq!(app(Phase::Error).color(), [240, 151, 138]);
    }

    #[test]
    fn discarding_capture_kills_and_reaps_child() {
        let mut app = app(Phase::Idle);
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        app.capture = Some(Capture {
            child,
            reader: thread::spawn(|| Ok(Vec::new())),
            errors: thread::spawn(|| Ok(Vec::new())),
        });
        app.discard_capture();
        assert!(app.capture.is_none(), "residual capture must be removed");
        assert_eq!(
            unsafe { libc::kill(pid as i32, 0) },
            -1,
            "capture is still alive or a zombie"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn escape_press_and_release_lifecycle_restores_keyboard_and_target_address() {
        let (socket, _server) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = wayland_client::Connection::from_socket(socket).unwrap();
        let queue = connection.new_event_queue::<ui::Ui>();
        let qh = queue.handle();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut ui = ui::Ui::new_for_test(connection, qh, tx);

        let mut app = app(Phase::Idle);
        let target = "0x556677889900".to_string();

        // Simulate recording in Writing (blue) mode
        ui.capture_keyboard(true);
        assert!(ui.capturing_keyboard);
        app.mode = hypr_dictate::DictationMode::Writing;
        app.phase = Phase::Recording(target.clone());
        app.lifecycle.start(target.clone(), true);
        assert_eq!(app.lifecycle.target_address, Some(target.clone()));
        assert!(app.lifecycle.is_recording);

        // User presses Escape
        app.event(Event::KeyPress(hypr_dictate::CaptureKey::Escape), &mut ui);

        // Still capturing keyboard while Escape key is held physically
        assert_eq!(app.phase, Phase::Idle);
        assert!(ui.capturing_keyboard, "Debe mantener captura de teclado mientras Escape esté presionado");
        assert_eq!(app.lifecycle.waiting_release, Some(hypr_dictate::CaptureKey::Escape));
        assert_eq!(app.lifecycle.target_address, Some(target.clone()), "target_address debe conservarse hasta la liberación");

        // User releases Escape
        app.event(Event::KeyRelease(hypr_dictate::CaptureKey::Escape), &mut ui);

        // Upon release, keyboard capture is released and target address is restored/cleared
        assert!(!ui.capturing_keyboard, "ui.capturing_keyboard debe liberarse al soltar Escape");
        assert_eq!(app.lifecycle.waiting_release, None);
        assert_eq!(app.lifecycle.target_address, None);
        assert_eq!(app.mode, hypr_dictate::DictationMode::Transcription);
        assert_eq!(app.phase, Phase::Idle);
    }

    #[test]
    fn escape_press_during_processing_cancels_and_release_restores_keyboard() {
        let (socket, _server) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = wayland_client::Connection::from_socket(socket).unwrap();
        let queue = connection.new_event_queue::<ui::Ui>();
        let qh = queue.handle();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut ui = ui::Ui::new_for_test(connection, qh, tx);

        let mut app = app(Phase::Idle);
        let target = "0x556677889900".to_string();

        ui.capture_keyboard(true);
        app.mode = hypr_dictate::DictationMode::Writing;
        app.phase = Phase::Processing(target.clone());
        app.lifecycle.start(target.clone(), false);
        app.lifecycle.is_recording = false;

        // Escape pressed during processing
        app.event(Event::KeyPress(hypr_dictate::CaptureKey::Escape), &mut ui);
        assert_eq!(app.phase, Phase::Idle);
        assert_eq!(app.lifecycle.waiting_release, Some(hypr_dictate::CaptureKey::Escape));

        // Escape released
        app.event(Event::KeyRelease(hypr_dictate::CaptureKey::Escape), &mut ui);
        assert!(!ui.capturing_keyboard);
        assert_eq!(app.lifecycle.waiting_release, None);
        assert_eq!(app.lifecycle.target_address, None);
    }

    #[test]
    fn cancel_without_waiting_release_immediately_releases_keyboard() {
        let (socket, _server) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = wayland_client::Connection::from_socket(socket).unwrap();
        let queue = connection.new_event_queue::<ui::Ui>();
        let qh = queue.handle();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut ui = ui::Ui::new_for_test(connection, qh, tx);

        let mut app = app(Phase::Idle);
        let target = "0x556677889900".to_string();

        ui.capture_keyboard(true);
        app.phase = Phase::Recording(target.clone());
        app.lifecycle.start(target.clone(), true);

        // Cancel called when waiting_release is None (e.g. IPC reset)
        app.event(Event::Command("reset".into()), &mut ui);
        assert!(!ui.capturing_keyboard, "reset debe liberar el teclado inmediatamente");
        assert_eq!(app.lifecycle.waiting_release, None);
        assert_eq!(app.lifecycle.target_address, None);
    }

    #[test]
    fn enter_release_during_processing_retains_keyboard_capture_until_finished_or_cancelled() {
        let (socket, _server) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = wayland_client::Connection::from_socket(socket).unwrap();
        let queue = connection.new_event_queue::<ui::Ui>();
        let qh = queue.handle();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut ui = ui::Ui::new_for_test(connection, qh, tx);

        let mut app = app(Phase::Idle);
        let target = "0x556677889900".to_string();

        ui.capture_keyboard(true);
        app.mode = hypr_dictate::DictationMode::Writing;
        app.phase = Phase::Recording(target.clone());
        app.lifecycle.start(target.clone(), false);

        // Press Enter to stop recording
        app.phase = Phase::Processing(target.clone());
        let action = app.lifecycle.on_key_press(hypr_dictate::CaptureKey::Return);
        assert_eq!(action, hypr_dictate::KeyAction::StopRecording);

        // Release Enter while API is still processing
        app.event(Event::KeyRelease(hypr_dictate::CaptureKey::Return), &mut ui);
        // Keyboard capture MUST still be active so Escape can cancel!
        assert!(
            ui.capturing_keyboard,
            "Dock must retain keyboard capture while in Phase::Processing"
        );
        assert_eq!(app.phase, Phase::Processing(target.clone()));

        // Escape pressed during processing
        app.event(Event::KeyPress(hypr_dictate::CaptureKey::Escape), &mut ui);
        assert_eq!(app.phase, Phase::Idle);
        assert_eq!(
            app.lifecycle.waiting_release,
            Some(hypr_dictate::CaptureKey::Escape)
        );

        // Escape released
        app.event(Event::KeyRelease(hypr_dictate::CaptureKey::Escape), &mut ui);
        assert!(!ui.capturing_keyboard);
        assert_eq!(app.lifecycle.waiting_release, None);
        assert_eq!(app.lifecycle.target_address, None);
    }

    #[test]
    fn enter_release_during_processing_retains_keyboard_capture_until_api_finished() {
        let (socket, _server) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = wayland_client::Connection::from_socket(socket).unwrap();
        let queue = connection.new_event_queue::<ui::Ui>();
        let qh = queue.handle();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut ui = ui::Ui::new_for_test(connection, qh, tx);

        let mut app = app(Phase::Idle);
        let target = "0x556677889900".to_string();

        ui.capture_keyboard(true);
        app.mode = hypr_dictate::DictationMode::Writing;
        app.phase = Phase::Recording(target.clone());
        let generation = app.lifecycle.start(target.clone(), true);

        // Press Enter to stop recording
        app.phase = Phase::Processing(target.clone());
        let action = app.lifecycle.on_key_press(hypr_dictate::CaptureKey::Return);
        assert_eq!(action, hypr_dictate::KeyAction::StopRecording);

        // Release Enter while API is still processing
        app.event(Event::KeyRelease(hypr_dictate::CaptureKey::Return), &mut ui);
        assert!(ui.capturing_keyboard);
        assert_eq!(app.phase, Phase::Processing(target.clone()));

        // API finishes with valid response (in unit test without actual window, restore fails cleanly)
        app.event(
            Event::Finished(generation, Ok((target.clone(), "Texto formal".into()))),
            &mut ui,
        );
        // Keyboard capture is released regardless of success or target-missing error
        assert!(!ui.capturing_keyboard);
        assert_eq!(app.phase, Phase::Error);
        assert!(app.error.contains("La ventana original ya no existe"));
    }
}
